use wisp_protocol::log::LEAD_PROCESSES;

use super::*;
use crate::agent::{FINISHED_MESSAGE, INTERRUPTED_MESSAGE};

fn resumed(plan: Vec<Step>, records: &[LogRecord]) -> Rig {
    let mut setup = Setup::new(plan);
    setup.from = Some(records.to_vec());
    setup.rig()
}

#[tokio::test]
async fn a_failed_turn_resumed_then_retried_sends_what_the_live_retry_sent() {
    let mut live = Setup::new(vec![
        uses(&[tool_call()]).then(ClientCommand::send("tui-2", "and also this")),
        failed(ErrorKind::Server),
        done("finished"),
    ])
    .rig();
    live.run(vec![ClientCommand::send("tui-1", "go")], 1).await;
    let records = records(&live.store);
    let retried = live.run(vec![retry_turn(1, None, 3)], 1).await;

    let mut again = resumed(vec![done("finished")], &records);
    assert_eq!(again.agent.retry_offer().map(|(turn, _)| turn), Some(1));
    let resumed_retry = again.run(vec![retry_turn(1, None, 3)], 1).await;
    assert_eq!(again.a.texts()[0], live.a.texts()[2]);
    assert_eq!(accepted(&resumed_retry), accepted(&retried));
    assert_eq!(delivered(&resumed_retry), delivered(&retried));
    assert_eq!(again.agent.history(), live.agent.history());
    again.assert_rebuilds();
}

#[tokio::test]
async fn a_failed_queued_turn_resumed_then_retried_keeps_its_mark() {
    let mut live = Setup::new(vec![
        done("one").then(ClientCommand::queue("tui-2", "and then this")),
        failed(ErrorKind::Server),
        done("finished"),
    ])
    .rig();
    live.run(vec![ClientCommand::send("tui-1", "go")], 2).await;
    let records = records(&live.store);
    live.run(vec![retry_turn(2, None, 3)], 1).await;
    let bodies = live.a.texts();
    assert!(bodies[1].contains(STEER_NOTE), "{}", bodies[1]);
    assert_eq!(bodies[2], bodies[1]);

    let mut again = resumed(vec![done("finished")], &records);
    assert_eq!(again.agent.retry_offer().map(|(turn, _)| turn), Some(2));
    again.run(vec![retry_turn(2, None, 3)], 1).await;
    assert_eq!(again.a.texts()[0], bodies[2]);
    assert_eq!(again.agent.history(), live.agent.history());
    again.assert_rebuilds();
}

#[tokio::test]
async fn a_failed_first_step_resumed_then_retried_resends_the_message_and_the_steer() {
    for replied in [false, true] {
        let mut steps = vec![failed(ErrorKind::Server), done("yes")];
        if replied {
            steps.insert(
                0,
                reply(&[B::Think], StopReason::EndTurn)
                    .then(ClientCommand::send("tui-2", "are you there")),
            );
        }
        let mut live = Setup::new(steps).rig();
        live.idle(vec![ClientCommand::shortcut(
            RequestId(1),
            None,
            None,
            Some(Effort::High),
        )])
        .await;
        live.run(vec![ClientCommand::send("tui-1", "hello")], 1)
            .await;
        let records = records(&live.store);
        let retried = live.run(vec![retry_turn(1, None, 5)], 1).await;
        let expected = live.a.texts().last().unwrap().clone();
        for closed in [false, true] {
            let mut records = records.clone();
            if closed {
                let ended = records
                    .iter()
                    .position(|record| matches!(record.event, LogEvent::TurnEnded { .. }))
                    .unwrap();
                let mut withdrawal = records[ended].clone();
                withdrawal.event = LogEvent::InputWithdrawn {
                    turn: Some(1),
                    ids: vec![InputId(if replied { "tui-2" } else { "tui-1" }.into())],
                    by: WithdrawnBy::SessionEnded,
                };
                records.insert(ended, withdrawal);
                if let LogEvent::TurnEnded {
                    outcome: TurnOutcome::Error { offer, .. },
                    ..
                } = &mut records[ended + 1].event
                {
                    *offer = None;
                }
                for (seq, record) in records.iter_mut().enumerate() {
                    record.seq = u64::try_from(seq).unwrap();
                }
                let mut accepted = records[ended + 1].clone();
                accepted.event = LogEvent::InputAccepted {
                    turn: None,
                    id: InputId("tui-3".into()),
                    mode: DeliveryMode::Queue,
                    text: "a later input".into(),
                    images: Vec::new(),
                    replaces: None,
                    held: false,
                    from: None,
                    checklist_spans: Vec::new(),
                };
                let mut lapsed = records.clone();
                lapsed.push(accepted);
                for (seq, record) in lapsed.iter_mut().enumerate() {
                    record.seq = u64::try_from(seq).unwrap();
                }
                let lapsed = resumed(Vec::new(), &lapsed);
                assert!(lapsed.agent.retry_offer().is_none());
                let id = InputId(if replied { "tui-2" } else { "tui-1" }.into());
                assert!(lapsed.agent.returned_inputs().iter().any(|r| r.id == id));
            }
            if replied && closed {
                let mut carried = resumed(Vec::new(), &records);
                assert!(carried.agent.carry_on(true));
                assert!(carried.agent.retry_offer().is_none());
                assert!(
                    carried
                        .agent
                        .returned_inputs()
                        .iter()
                        .any(|r| r.id.0 == "tui-2"),
                    "carrying on must give back the dropped Retry input"
                );
            }
            let mut again = resumed(vec![done("yes")], &records);
            assert_eq!(again.agent.retry_offer().map(|(turn, _)| turn), Some(1));
            assert!(
                again.agent.returned_inputs().is_empty(),
                "Retry inputs also returned to the composer: {:?}",
                again.agent.returned_inputs()
            );
            let resumed_retry = again.run(vec![retry_turn(1, None, 5)], 1).await;
            assert_eq!(again.a.texts()[0], expected);
            assert_eq!(accepted(&resumed_retry), accepted(&retried));
            assert_eq!(again.agent.history(), live.agent.history());
            again.assert_rebuilds();
        }
    }
}

#[tokio::test]
async fn a_crash_while_a_step_streamed_is_retried_with_the_request_it_cut_short() {
    let mut live = Setup::new(vec![
        uses(&[tool_call()]).then(ClientCommand::send("tui-2", "and also this")),
        failed(ErrorKind::Server),
    ])
    .rig();
    live.run(vec![ClientCommand::send("tui-1", "go")], 1).await;
    let records = before(&records(&live.store), |e| {
        matches!(e, LogEvent::StepEnded { step: 2, .. })
    });

    let mut again = resumed(vec![done("finished")], &records);
    let repaired = again.idle(Vec::new()).await;
    let ends = outcomes(&repaired);
    let [
        TurnOutcome::Error {
            kind: ErrorKind::Interrupted,
            message,
            cost: Some(cost),
            offer: Some(_),
            ..
        },
    ] = ends.as_slice()
    else {
        panic!("{ends:#?}");
    };
    let retried = again.run(vec![retry_turn(1, None, 3)], 1).await;
    assert!(matches!(
        outcomes(&retried).as_slice(),
        [TurnOutcome::Completed { .. }]
    ));
    assert_eq!(message, INTERRUPTED_MESSAGE);
    assert!(cost.turn_lower_bound && cost.session_lower_bound);
    assert_eq!(
        again.a.texts()[0],
        live.a.texts()[1],
        "the request the crash cut short, with the steer it carried"
    );
    assert!(again.a.texts()[0].contains("and also this"));
    again.assert_rebuilds();
}

fn repaired_end(again: &[AgentEvent]) -> (String, bool) {
    match outcomes(again).as_slice() {
        [
            TurnOutcome::Error {
                kind: ErrorKind::Interrupted,
                message,
                offer,
                ..
            },
        ] => (message.clone(), offer.is_some()),
        other => panic!("{other:#?}"),
    }
}

async fn two_turns() -> Rig {
    let mut live = Setup::new(vec![done("one"), done("two")]).rig();
    live.run(vec![ClientCommand::send("tui-1", "first")], 1)
        .await;
    live.run(vec![ClientCommand::send("tui-2", "second")], 1)
        .await;
    live
}

#[tokio::test]
async fn a_second_turn_cut_before_its_reply_is_interrupted_and_retried() {
    let live = two_turns().await;
    let records = before(&records(&live.store), |e| {
        matches!(e, LogEvent::MessageCommitted { turn: 2, .. })
    });
    let mut again = resumed(vec![done("two")], &records);
    let (message, offered) = repaired_end(&again.idle(Vec::new()).await);
    assert_eq!(message, INTERRUPTED_MESSAGE);
    assert!(offered);
    let retried = again.run(vec![retry_turn(2, None, 3)], 1).await;
    assert!(matches!(
        outcomes(&retried).as_slice(),
        [TurnOutcome::Completed { .. }]
    ));
    assert_eq!(again.a.texts()[0], live.a.texts()[1]);
    again.assert_rebuilds();
}

#[tokio::test]
async fn a_second_turn_cut_after_its_reply_finished_says_so_and_offers_nothing() {
    let live = two_turns().await;
    let records = before(&records(&live.store), |e| {
        matches!(e, LogEvent::TurnEnded { turn: 2, .. })
    });
    let mut again = resumed(vec![done("three")], &records);
    let (message, offered) = repaired_end(&again.idle(Vec::new()).await);
    assert_eq!(message, FINISHED_MESSAGE);
    assert!(!offered);
    assert!(again.agent.retry_offer().is_none());
    again.assert_rebuilds();
}

#[tokio::test]
async fn a_crash_before_a_delivered_steer_commits_after_a_final_reply_is_interrupted_and_retried() {
    let mut live = Setup::new(vec![
        done("hi there").then(ClientCommand::send("tui-2", "one more thing")),
        done("noted"),
    ])
    .rig();
    live.run(vec![ClientCommand::send("tui-1", "hello")], 1)
        .await;
    let all = records(&live.store);
    assert!(
        all.iter()
            .any(|r| matches!(&r.event, LogEvent::InputDelivered { step: 2, .. })),
        "the steer went out with step 2"
    );
    let records = before(&all, |e| {
        matches!(e, LogEvent::InputCommitted { step: 2, .. })
    });
    assert!(
        records
            .iter()
            .any(|r| matches!(&r.event, LogEvent::MessageCommitted { .. })),
        "the first reply committed before the steer went out"
    );
    let mut again = resumed(vec![done("noted")], &records);
    assert!(
        again.agent.returned_inputs().is_empty(),
        "Retry inputs must not also return to the composer"
    );
    let (message, offered) = repaired_end(&again.idle(Vec::new()).await);
    assert_eq!(message, INTERRUPTED_MESSAGE);
    assert!(offered);
    let retried = again.run(vec![retry_turn(1, None, 3)], 1).await;
    assert!(matches!(
        outcomes(&retried).as_slice(),
        [TurnOutcome::Completed { .. }]
    ));
    assert_eq!(again.a.texts()[0], live.a.texts()[1]);
    assert!(again.a.texts()[0].contains("one more thing"));
    again.assert_rebuilds();
    let finished = resumed(Vec::new(), &super::records(&again.store));
    assert!(finished.agent.returned_inputs().is_empty());
    let mut carried = resumed(Vec::new(), &records);
    assert!(carried.agent.carry_on(true));
    assert_eq!(
        carried
            .agent
            .returned_inputs()
            .iter()
            .map(|r| r.text.as_str())
            .collect::<Vec<_>>(),
        ["one more thing"]
    );
    let (_, offered) = repaired_end(&carried.idle(Vec::new()).await);
    assert!(!offered);
}

fn resumed_with_processes(plan: Vec<Step>, records: &[LogRecord]) -> Rig {
    let mut setup = Setup::new(plan).with_toolbox(job_tools());
    setup.from = Some(records.to_vec());
    setup.rig()
}

fn notices(records: &[LogRecord]) -> Vec<(u64, Option<u64>, Option<String>)> {
    records
        .iter()
        .filter_map(|r| match &r.event {
            LogEvent::TurnStarted {
                turn,
                retry_of,
                lead,
                ..
            } => Some((
                *turn,
                *retry_of,
                lead.text(LEAD_PROCESSES).map(str::to_owned),
            )),
            _ => None,
        })
        .collect()
}

async fn a_failure_after_a_notice(wake: bool) -> Option<(Rig, Vec<LogRecord>)> {
    crate::jobs::exec::find_program("bash")?;
    let a = ExitGate::new("a.go", 2);
    let start = call(
        "a",
        "bash",
        json!({"command": &a.command, "run_in_background": true}),
    );
    let mut live = Setup::new(vec![
        uses(&[start]),
        done("started"),
        failed(ErrorKind::Server),
        done("again"),
    ])
    .with_toolbox(job_tools())
    .rig();
    live.run(vec![ClientCommand::send("tui-1", "go")], 1).await;
    a.open(&live._env.work);
    if wake {
        live.run(Vec::new(), 1).await;
    } else {
        live.drive(
            Vec::new(),
            |seen| {
                seen.iter()
                    .any(|e| matches!(e, AgentEvent::ProcessEnded { id: 1, .. }))
            },
            PATIENT,
        )
        .await;
        live.run(vec![ClientCommand::send("tui-2", "next")], 1)
            .await;
    }
    let records = records(&live.store);
    let notice = notices(&records)[1].2.clone();
    assert!(
        notice
            .as_deref()
            .is_some_and(|n| n.contains(&a.ended(1, 2))),
        "{notice:?}"
    );
    assert_eq!(
        notices(&records)[1].0,
        2,
        "the second turn carried the notice"
    );
    live.run(vec![retry_turn(2, None, 2)], 1).await;
    let bodies = live.a.texts();
    assert_eq!(bodies.len(), 4);
    assert_eq!(bodies[3], bodies[2]);
    Some((live, records))
}

async fn assert_resumed_retry_matches(live: &Rig, records: &[LogRecord], crashed: bool) {
    let records = if crashed {
        before(records, |e| {
            matches!(e, LogEvent::TurnEnded { turn: 2, .. })
        })
    } else {
        records.to_vec()
    };
    let mut again = resumed_with_processes(vec![done("again")], &records);
    again.idle(Vec::new()).await;
    assert_eq!(
        again.agent.retry_offer().map(|(turn, _)| turn),
        Some(2),
        "crashed: {crashed}"
    );
    let retried = again.run(vec![retry_turn(2, None, 2)], 1).await;
    assert!(
        matches!(
            outcomes(&retried).as_slice(),
            [TurnOutcome::Completed { .. }]
        ),
        "crashed: {crashed}"
    );
    assert_eq!(again.a.texts()[0], live.a.texts()[2], "crashed: {crashed}");
    let live_notices = notices(&test_guard::records(&live.store));
    let resumed_notices = notices(&test_guard::records(&again.store));
    assert_eq!(
        resumed_notices.last(),
        live_notices.last(),
        "crashed: {crashed}"
    );
    again.assert_rebuilds();
}

#[tokio::test]
async fn a_retry_after_resume_sends_the_failed_turns_process_notice_again() {
    let Some((live, records)) = a_failure_after_a_notice(false).await else {
        return;
    };
    for crashed in [false, true] {
        assert_resumed_retry_matches(&live, &records, crashed).await;
    }
}

#[tokio::test]
async fn a_failed_wake_turn_is_offered_again_after_resume_and_resent_whole() {
    let Some((live, records)) = a_failure_after_a_notice(true).await else {
        return;
    };
    for crashed in [false, true] {
        assert_resumed_retry_matches(&live, &records, crashed).await;
    }
}

#[tokio::test]
async fn ids_withdrawn_by_the_repair_are_free_for_new_messages() {
    let mut live = Setup::new(vec![
        done("hi there")
            .then(ClientCommand::queue("tui-1", "one"))
            .then(ClientCommand::queue("tui-3", "three")),
        done("one and three"),
    ])
    .rig();
    live.run(vec![ClientCommand::send("tui-0", "hello")], 2)
        .await;
    let all = records(&live.store);
    let records = before(&all, |e| matches!(e, LogEvent::TurnEnded { turn: 1, .. }));
    let accepted_ids: Vec<InputId> = records
        .iter()
        .filter_map(|r| match &r.event {
            LogEvent::InputAccepted { id, .. } => Some(id.clone()),
            _ => None,
        })
        .collect();
    assert_eq!(
        accepted_ids,
        [InputId("tui-1".into()), InputId("tui-3".into())]
    );

    let mut again = resumed(vec![done("a"), done("b"), done("c")], &records);
    let repaired = again.idle(Vec::new()).await;
    assert!(
        repaired.iter().any(|e| matches!(
            e,
            AgentEvent::InputWithdrawn {
                by: WithdrawnBy::SessionEnded,
                ..
            }
        )),
        "{repaired:#?}"
    );
    for (id, text) in [("tui-1", "first"), ("tui-2", "second"), ("tui-3", "third")] {
        let events = again.run(vec![ClientCommand::send(id, text)], 1).await;
        assert!(
            !events
                .iter()
                .any(|e| matches!(e, AgentEvent::Rejected { .. })),
            "{id}: {events:#?}"
        );
        assert!(
            matches!(
                outcomes(&events).as_slice(),
                [TurnOutcome::Completed { .. }]
            ),
            "{id}: {events:#?}"
        );
    }
    let bodies = again.a.texts();
    assert_eq!(bodies.len(), 3);
    for (body, text) in bodies.iter().zip(["first", "second", "third"]) {
        assert!(body.contains(text), "{body}");
    }
    again.assert_rebuilds();
}

#[tokio::test]
async fn failed_brief_wake_turn_retry_uses_persisted_waiting_mail_revision_across_two_resumes() {
    use crate::agent::testing::checklist_mail;
    let canary = "CANARYOpaqueFailedWakeRetry";
    let task = "A failed waking review task";
    for restart in [false, true] {
        crate::session::reset_held_for_test();
        let mut live = Setup::new(vec![failed(ErrorKind::Server)]).rig();
        live.agent = live
            .agent
            .with_path(wisp_protocol::AgentPath::new("/child"));
        live.agent
            .mail_sender()
            .send(checklist_mail("retry-brief", task, &[canary]))
            .unwrap();
        let failed = live.run(Vec::new(), 1).await;
        assert!(offer_of(&failed).is_some());
        let before = records(&live.store);
        let mut fresh = resumed(
            if restart {
                Vec::new()
            } else {
                vec![done("retried")]
            },
            &before,
        );
        crate::session::register_secrets([canary.to_owned()]);
        let seen = fresh.idle(Vec::new()).await;
        assert!(seen.iter().any(|event| matches!(event, AgentEvent::InputAccepted { text, .. } if text.contains("[redacted]") && !text.contains(canary))));
        let after = records(&fresh.store);
        assert!(after.iter().any(|row| matches!(&row.event, LogEvent::InputRevised { turn: None, inputs } if inputs[0].text.contains("[redacted]"))));
        if restart {
            crate::session::reset_held_for_test();
            assert!(crate::session::held().is_empty());
            fresh = resumed(vec![done("retried")], &after);
        }
        fresh.run(vec![retry_turn(1, None, 3)], 1).await;
        let body = fresh.a.texts().pop().unwrap();
        assert_eq!(body.matches(task).count(), 1, "{body}");
        assert!(body.contains("[redacted]") && !body.contains(canary));
        assert!(records(&fresh.store).iter().any(|row| matches!(&row.event, LogEvent::TurnStarted { turn: 2, inputs, .. } if inputs.iter().any(|input| input.text.contains("[redacted]") && !input.text.contains(canary)))));
        fresh.assert_rebuilds();
    }
}
