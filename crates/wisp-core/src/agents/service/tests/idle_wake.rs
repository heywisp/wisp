use super::*;

const IDLE: Duration = Duration::from_secs(40 * 60);

fn stuck(minutes: u32) -> Setup {
    Setup {
        limits: Limits {
            wake_when_stuck: minutes,
            ..Limits::default()
        },
        plugins: vec!["write_todos"],
        ..Setup::default()
    }
}

fn idle_wakes(run: &Run) -> usize {
    run.mail_to("/")
        .iter()
        .filter(|(kind, wake, text)| {
            *kind == MailKind::Notice && *wake && text.contains(wisp_protocol::agents::IDLE_WAKE)
        })
        .count()
}

#[tokio::test(start_paused = true)]
async fn an_idle_tree_with_an_open_checklist_wakes_main_once() {
    let mut run = start(
        "idle-wake-once",
        stuck(40),
        vec![
            ("/a", vec![uses(&[report_call("r1", "Done")])]),
            (
                "/",
                vec![
                    uses(&[todos_call("t1", "Looked", "Finish")]),
                    done("Waiting"),
                    done("Noted"),
                    done("Still waiting"),
                ],
            ),
        ],
    )
    .await;
    run.say("Work").await;
    run.root_ended().await;
    run.spawn(spawn_request("a", "Look")).await;
    run.child_ended("/a").await;
    run.root_ended().await;
    let before = run.requests("/");
    tokio::time::sleep(IDLE - Duration::from_secs(1)).await;
    assert_eq!(idle_wakes(&run), 0);
    assert_eq!(run.requests("/").len(), before.len());
    tokio::time::sleep(Duration::from_secs(2)).await;
    run.root_ended().await;
    let after = run.requests("/");
    assert_eq!(idle_wakes(&run), 1);
    assert_eq!(after.len(), before.len() + 1);
    grows_from(after.last().unwrap(), before.last().unwrap(), "idle wake");
    assert_eq!(
        after
            .last()
            .unwrap()
            .all_text()
            .matches(wisp_protocol::agents::IDLE_WAKE)
            .count(),
        1
    );
    tokio::time::sleep(IDLE * 3).await;
    let run = run.quit().await;
    assert_eq!(idle_wakes(&run), 1);
    assert_eq!(run.requests("/").len(), after.len());
}

#[tokio::test(start_paused = true)]
async fn a_later_wake_needs_a_childs_run_or_a_changed_checklist() {
    let mut run = start(
        "idle-wake-rearm",
        stuck(40),
        vec![
            (
                "/a",
                vec![
                    uses(&[report_call("r1", "First")]),
                    uses(&[report_call("r2", "Again")]),
                ],
            ),
            (
                "/",
                vec![
                    uses(&[todos_call("t1", "Looked", "Finish")]),
                    done("Waiting"),
                    done("Noted"),
                    done("Still waiting"),
                    done("Noted again"),
                    uses(&[todos_call("t2", "Looked again", "Finish")]),
                    done("Changed checklist"),
                    done("Waiting again"),
                ],
            ),
        ],
    )
    .await;
    run.say("Work").await;
    run.root_ended().await;
    run.spawn(spawn_request("a", "Look")).await;
    run.child_ended("/a").await;
    run.root_ended().await;
    tokio::time::sleep(IDLE + Duration::from_secs(1)).await;
    run.root_ended().await;
    assert_eq!(idle_wakes(&run), 1);
    let requests = run.requests("/").len();
    tokio::time::sleep(IDLE * 3).await;
    assert_eq!(idle_wakes(&run), 1);
    assert_eq!(run.requests("/").len(), requests);
    run.send(ClientCommand::Child {
        agent: path("/a"),
        command: Box::new(ClientCommand::send("c1", "Look again")),
    })
    .await;
    run.child_ended("/a").await;
    run.root_ended().await;
    tokio::time::sleep(IDLE + Duration::from_secs(1)).await;
    run.root_ended().await;
    assert_eq!(idle_wakes(&run), 2);
    tokio::time::sleep(IDLE + Duration::from_secs(1)).await;
    run.root_ended().await;
    assert_eq!(idle_wakes(&run), 3);
    let requests = run.requests("/").len();
    tokio::time::sleep(IDLE * 3).await;
    let run = run.quit().await;
    assert_eq!(idle_wakes(&run), 3);
    assert_eq!(run.requests("/").len(), requests);
}

#[tokio::test(start_paused = true)]
async fn a_busy_child_holds_the_clock_until_it_has_been_idle_the_whole_time() {
    let mut run = start(
        "idle-wake-busy",
        stuck(40),
        vec![
            (
                "/a",
                vec![
                    uses(&[report_call("r1", "Done")])
                        .held(Wait::Sleep(Duration::from_secs(30 * 60))),
                ],
            ),
            (
                "/",
                vec![
                    uses(&[todos_call("t1", "Looked", "Finish")]),
                    done("Waiting"),
                    done("Noted"),
                    done("Awake"),
                ],
            ),
        ],
    )
    .await;
    run.say("Work").await;
    run.root_ended().await;
    tokio::time::sleep(Duration::from_secs(20 * 60)).await;
    run.spawn(spawn_request("a", "Look")).await;
    run.until("the child's request", |e| {
        matches!(inner(e, "/a"), Some(AgentEvent::StepStarted { .. }))
    })
    .await;
    tokio::time::sleep(Duration::from_secs(29 * 60)).await;
    assert_eq!(idle_wakes(&run), 0);
    assert_eq!(run.requests("/").len(), 2);
    tokio::time::sleep(Duration::from_secs(2 * 60)).await;
    run.child_ended("/a").await;
    run.root_ended().await;
    tokio::time::sleep(Duration::from_secs(38 * 60)).await;
    assert_eq!(idle_wakes(&run), 0);
    assert_eq!(run.requests("/").len(), 3);
    tokio::time::sleep(Duration::from_secs(2 * 60)).await;
    run.root_ended().await;
    let run = run.quit().await;
    assert_eq!(idle_wakes(&run), 1);
    assert_eq!(run.requests("/").len(), 4);
}

#[tokio::test(start_paused = true)]
async fn no_wake_without_an_open_item_with_the_setting_at_0_or_after_a_stop() {
    for (minutes, open, stop) in [(40, false, false), (0, true, false), (40, true, true)] {
        let todos = if open {
            todos_call("t1", "Looked", "Finish")
        } else {
            call(
                "t1",
                WRITE_TODOS,
                json!({"todos": [{"text": "Done", "status": "completed"}]}),
            )
        };
        let mut run = start(
            "idle-wake-off",
            stuck(minutes),
            vec![(
                "/",
                vec![
                    uses(&[todos]),
                    if stop {
                        hanging("Working")
                    } else {
                        done("Waiting")
                    },
                    done("Unexpected wake"),
                ],
            )],
        )
        .await;
        run.say("Work").await;
        if stop {
            run.until(
                "the running main session",
                |e| matches!(e, AgentEvent::TextDelta { text, .. } if text == "Working"),
            )
            .await;
            run.send(ClientCommand::Cancel).await;
        }
        let outcome = run.root_ended().await;
        assert_eq!(matches!(outcome, TurnOutcome::Cancelled { .. }), stop);
        tokio::time::sleep(IDLE * 3).await;
        let run = run.quit().await;
        assert_eq!(idle_wakes(&run), 0, "{minutes}, {open}, {stop}");
        assert_eq!(run.requests("/").len(), 2, "{minutes}, {open}, {stop}");
        assert!(run.mail_to("/").is_empty(), "{minutes}, {open}, {stop}");
    }
}

#[tokio::test(start_paused = true)]
async fn the_wake_waits_while_main_retries_at_its_used_up_limits_reset() {
    let at: Timestamp = "2026-10-28T06:00:00Z".parse().unwrap();
    let reset = at.as_second() + 6 * 3600;
    let mut error = classified(
        ErrorKind::RateLimit,
        "used up",
        FailurePhase::Response,
        RetryClass::Never,
    );
    crate::provider::limit::reached(
        &mut error.detail,
        at.as_second(),
        Some(reset),
        wisp_protocol::plan_usage::UsageSource::RetryAfter,
        None,
    );
    let mut run = start(
        "idle-wake-reset",
        Setup {
            clock: Some(Arc::new(ShiftedClock::paused(at, None))),
            ..stuck(40)
        },
        vec![(
            "/",
            vec![
                uses(&[todos_call("t1", "Looked", "Finish")]),
                Step::from(vec![Ok(ProviderEvent::Sending), Err(error)]),
                done("Retried"),
                done("Awake"),
            ],
        )],
    )
    .await;
    run.say("Work").await;
    let TurnOutcome::Error { offer, .. } = run.root_ended().await else {
        panic!("the limit ends the turn");
    };
    assert_eq!(offer.unwrap().retries_at, Some(reset + 60));
    tokio::time::sleep(Duration::from_secs(6 * 3600)).await;
    assert_eq!(idle_wakes(&run), 0);
    assert_eq!(run.requests("/").len(), 2);
    tokio::time::sleep(Duration::from_secs(2 * 60)).await;
    assert!(matches!(
        run.root_ended().await,
        TurnOutcome::Completed { .. }
    ));
    assert_eq!(run.requests("/").len(), 3);
    tokio::time::sleep(IDLE + Duration::from_secs(1)).await;
    run.root_ended().await;
    let run = run.quit().await;
    assert_eq!(idle_wakes(&run), 1);
    assert_eq!(run.requests("/").len(), 4);
}
