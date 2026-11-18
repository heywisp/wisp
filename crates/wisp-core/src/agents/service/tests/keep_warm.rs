use super::*;

const IDLE: Duration = Duration::from_secs(56 * 60);

fn marked() -> Setup {
    Setup {
        digests: true,
        ..Setup::default()
    }
}

fn kept_warm(name: &str, task: &str) -> SpawnRequest {
    SpawnRequest {
        keep_warm: true,
        ..spawn_request(name, task)
    }
}

fn reports() -> Vec<Step> {
    vec![
        uses(&[report_call("r1", "Done")]),
        done("bye"),
        done("warm"),
    ]
}

fn refresh_records(run: &Run, path: &str) -> Vec<wisp_protocol::RefreshOutcome> {
    pick!(run.records(path), LogEvent::CacheRefresh { outcome, .. } => outcome)
}

async fn idle_children(run: &mut Run, paths: &[&str]) {
    for path in paths {
        assert!(matches!(
            run.child_ended(path).await,
            TurnOutcome::Completed { .. }
        ));
    }
    tokio::time::sleep(Duration::from_secs(5)).await;
}

#[tokio::test(start_paused = true)]
async fn only_a_child_spawned_with_keep_warm_is_refreshed() {
    let mut run = start(
        "warm-marked",
        marked(),
        vec![
            ("/a", reports()),
            ("/b", reports()),
            ("/", vec![done("noted"), done("noted"), done("noted")]),
        ],
    )
    .await;
    let request = run.spawn(kept_warm("a", "Look.")).await;
    let spawned = run
        .until(
            "a's spawn",
            |e| matches!(e, AgentEvent::AgentSpawned { request: Some(r), .. } if *r == request),
        )
        .await;
    let AgentEvent::AgentSpawned { config, .. } = spawned else {
        unreachable!()
    };
    assert!(config.keep_warm);
    run.spawn(spawn_request("b", "Look too.")).await;
    idle_children(&mut run, &["/a", "/b"]).await;
    run.send(ClientCommand::GetReady { on: true }).await;
    run.until("ready before refreshing", |event| {
        matches!(
            event,
            AgentEvent::Restart {
                state: wisp_protocol::RestartState::Ready,
                ..
            }
        )
    })
    .await;
    tokio::time::sleep(IDLE).await;
    assert_eq!(refreshes(&run, "/a"), 1);
    assert_eq!(refreshes(&run, "/b"), 0);
    assert_eq!(refreshes(&run, "/"), 0, "main is off by default");
    let run = run.quit().await;
    assert_eq!(refresh_records(&run, "/a").len(), 1);
    assert!(refresh_records(&run, "/b").is_empty());
    let spawned: Vec<bool> = pick!(
        run.records("/"),
        LogEvent::AgentSpawned { config, .. } => config.keep_warm
    );
    assert_eq!(spawned, [true, false]);
}

const REMINDER: &str = "Children idle and still kept warm: ";

fn ticking(warm_refreshes: u32) -> Setup {
    Setup {
        clock: Some(Arc::new(ShiftedClock::paused(
            "2026-10-28T06:00:00Z".parse().unwrap(),
            None,
        ))),
        ..limit(warm_refreshes)
    }
}

#[tokio::test(start_paused = true)]
async fn a_parent_reads_once_after_a_refresh_which_idle_children_it_still_keeps_warm() {
    let mut run = start(
        "warm-reminder",
        ticking(2),
        vec![
            ("/a", reports()),
            ("/b", reports()),
            ("/", (0..5).map(|_| done("noted")).collect()),
        ],
    )
    .await;
    run.spawn(kept_warm("a", "Look.")).await;
    run.spawn(spawn_request("b", "Look too.")).await;
    idle_children(&mut run, &["/a", "/b"]).await;
    let reported = run.requests("/").len();
    for _ in 0..reported {
        run.root_ended().await;
    }
    tokio::time::sleep(IDLE).await;
    assert_eq!(refreshes(&run, "/a"), 1);
    for text in ["hello", "again"] {
        run.say(text).await;
        run.root_ended().await;
    }
    let run = run.quit().await;
    let requests = run.requests("/");
    for pair in requests.windows(2) {
        grows_from(&pair[1], &pair[0], "main");
    }
    let reminders = |request: &Sent| request.all_text().matches(REMINDER).count();
    assert!(reported > 0);
    assert!(requests[..reported].iter().all(|r| reminders(r) == 0));
    let [hello, again] = &requests[reported..] else {
        panic!("{} requests after the reports", requests.len() - reported)
    };
    let opened = hello.last_user_texts().concat();
    assert!(
        opened.contains(&format!("{REMINDER}/a for 56m05s.")),
        "{opened}"
    );
    assert!(!hello.all_text().contains("/b for"));
    assert_eq!(reminders(hello), 1);
    assert_eq!(reminders(again), 1);
    assert!(!again.last_user_texts().concat().contains(REMINDER));
}

#[tokio::test(start_paused = true)]
async fn a_refreshed_child_is_not_named_beside_its_next_report() {
    let mut run = start(
        "warm-report-unnamed",
        ticking(2),
        vec![
            (
                "/a",
                vec![
                    uses(&[report_call("r1", "Done")]),
                    done("warm"),
                    uses(&[report_call("r2", "Done again")]),
                    done("bye"),
                ],
            ),
            ("/", vec![done("noted"), done("noted"), done("noted")]),
        ],
    )
    .await;
    run.spawn(kept_warm("a", "Look.")).await;
    idle_children(&mut run, &["/a"]).await;
    run.root_ended().await;
    tokio::time::sleep(IDLE).await;
    assert_eq!(refreshes(&run, "/a"), 1);
    let arming = pause_at("/a", "arming");
    run.send(super::resume::child_send("/a", "you-1", "Once more."))
        .await;
    within("the arming pause", arming.wait()).await;
    run.root_ended().await;
    within("the arming pause", arming.wait()).await;
    let run = run.quit().await;
    let requests = run.requests("/");
    let opened = requests.last().unwrap().last_user_texts().concat();
    assert!(opened.contains("Done again"), "{opened}");
    assert!(!opened.contains(REMINDER), "{opened}");
}

#[tokio::test(start_paused = true)]
async fn an_ended_or_stopped_idle_child_is_never_refreshed() {
    for stop in [
        ClientCommand::EndAgent { agent: path("/a") },
        ClientCommand::Child {
            agent: path("/a"),
            command: Box::new(ClientCommand::Cancel),
        },
    ] {
        let mut run = start(
            "warm-ended",
            marked(),
            vec![("/a", reports()), ("/", vec![done("noted"), done("noted")])],
        )
        .await;
        run.spawn(kept_warm("a", "Look.")).await;
        idle_children(&mut run, &["/a"]).await;
        run.send(stop.clone()).await;
        tokio::time::sleep(IDLE).await;
        assert_eq!(refreshes(&run, "/a"), 0, "{stop:?}");
        let run = run.quit().await;
        assert!(refresh_records(&run, "/a").is_empty(), "{stop:?}");
    }
}

#[tokio::test(start_paused = true)]
async fn the_main_session_is_refreshed_when_keep_warm_is_on() {
    let setup = Setup {
        keep_warm_main: true,
        ..marked()
    };
    let mut run = start(
        "warm-main",
        setup,
        vec![("/", vec![done("hi"), done("warm")])],
    )
    .await;
    run.say("hello").await;
    assert!(matches!(
        run.root_ended().await,
        TurnOutcome::Completed { .. }
    ));
    tokio::time::sleep(IDLE).await;
    assert_eq!(refreshes(&run, "/"), 1);
    let run = run.quit().await;
    assert_eq!(refresh_records(&run, "/").len(), 1);
    main_spend_follows_each_cost(&run);
}

#[tokio::test(start_paused = true)]
async fn a_child_spawned_with_keep_warm_resumes_marked() {
    let mut run = start(
        "warm-resume",
        marked(),
        vec![("/a", reports()), ("/", vec![done("noted"), done("noted")])],
    )
    .await;
    run.spawn(kept_warm("a", "Look.")).await;
    idle_children(&mut run, &["/a"]).await;
    let old = run.quit().await;
    let mut back = super::resume::resume(
        old,
        marked(),
        vec![
            ("/a", vec![done("again"), done("warm")]),
            ("/", vec![done("noted")]),
        ],
    )
    .await;
    back.run
        .send(ClientCommand::Child {
            agent: path("/a"),
            command: Box::new(ClientCommand::send("c1", "once more")),
        })
        .await;
    assert!(matches!(
        back.run.child_ended("/a").await,
        TurnOutcome::Completed { .. }
    ));
    tokio::time::sleep(IDLE).await;
    assert_eq!(refreshes(&back.run, "/a"), 1);
    back.run.quit().await;
}

fn warm_changes(run: &Run, agent: &str) -> Vec<(u32, u32, bool)> {
    run.seen
        .iter()
        .filter_map(|e| match e {
            AgentEvent::KeepWarmChanged {
                agent: a,
                keep_warm,
            } if *a == path(agent) => {
                Some((keep_warm.refreshes, keep_warm.limit, keep_warm.paused))
            }
            _ => None,
        })
        .collect()
}

fn limit(warm_refreshes: u32) -> Setup {
    Setup {
        limits: Limits {
            warm_refreshes,
            ..Limits::default()
        },
        prints: Some(Prints::Real),
        ..marked()
    }
}

async fn listed(run: &mut Run, agent: &str) -> Option<wisp_protocol::KeepWarm> {
    let request = run.request();
    run.send(ClientCommand::ListAgents { request }).await;
    let event = run
        .until(
            "the list",
            |e| matches!(e, AgentEvent::AgentsSnapshot { request: Some(r), .. } if *r == request),
        )
        .await;
    let AgentEvent::AgentsSnapshot { agents, .. } = event else {
        unreachable!()
    };
    agents
        .into_iter()
        .find(|a| a.path == path(agent))
        .and_then(|a| a.keep_warm)
}

#[tokio::test(start_paused = true)]
async fn a_spawn_marks_its_entry_and_each_refresh_and_pause_is_announced() {
    let mut run = start(
        "warm-announced",
        limit(2),
        vec![
            (
                "/a",
                vec![
                    uses(&[report_call("r1", "Done")]),
                    done("bye"),
                    done("warm"),
                    done("warm"),
                ],
            ),
            ("/b", reports()),
            ("/", vec![done("noted"), done("noted"), done("noted")]),
        ],
    )
    .await;
    let request = run.spawn(kept_warm("a", "Look.")).await;
    let spawned = run
        .until(
            "a's spawn",
            |e| matches!(e, AgentEvent::AgentSpawned { request: Some(r), .. } if *r == request),
        )
        .await;
    let AgentEvent::AgentSpawned { agent, .. } = spawned else {
        unreachable!()
    };
    let fresh = wisp_protocol::KeepWarm {
        refreshes: 0,
        limit: 2,
        paused: false,
    };
    assert_eq!(agent.keep_warm, Some(fresh));
    run.spawn(spawn_request("b", "Look too.")).await;
    idle_children(&mut run, &["/a", "/b"]).await;
    assert_eq!(listed(&mut run, "/a").await, Some(fresh));
    assert_eq!(listed(&mut run, "/b").await, None);
    tokio::time::sleep(IDLE).await;
    assert_eq!(
        listed(&mut run, "/a").await,
        Some(wisp_protocol::KeepWarm {
            refreshes: 1,
            ..fresh
        })
    );
    tokio::time::sleep(IDLE * 3).await;
    assert_eq!(refreshes(&run, "/a"), 2);
    run.send(ClientCommand::Child {
        agent: path("/a"),
        command: Box::new(ClientCommand::Cancel),
    })
    .await;
    assert_eq!(
        listed(&mut run, "/a").await,
        Some(wisp_protocol::KeepWarm {
            refreshes: 2,
            limit: 2,
            paused: true,
        })
    );
    let run = run.quit().await;
    assert_eq!(
        warm_changes(&run, "/a"),
        [(1, 2, false), (2, 2, false), (2, 2, true)]
    );
    assert!(warm_changes(&run, "/b").is_empty());
}

#[tokio::test(start_paused = true)]
async fn an_ended_child_is_refreshed_and_announced_no_more() {
    let mut run = start(
        "warm-end",
        marked(),
        vec![("/a", reports()), ("/", vec![done("noted"), done("noted")])],
    )
    .await;
    run.spawn(kept_warm("a", "Look.")).await;
    idle_children(&mut run, &["/a"]).await;
    run.send(ClientCommand::EndAgent { agent: path("/a") })
        .await;
    tokio::time::sleep(IDLE * 4).await;
    let run = run.quit().await;
    assert_eq!(refreshes(&run, "/a"), 0);
    assert!(refresh_records(&run, "/a").is_empty());
    assert!(warm_changes(&run, "/a").is_empty());
}

#[tokio::test(start_paused = true)]
async fn the_main_session_is_announced_paused_at_the_start_then_armed() {
    let setup = Setup {
        keep_warm_main: true,
        ..limit(2)
    };
    let mut run = start(
        "warm-main-announced",
        setup,
        vec![("/", vec![done("hi"), done("warm")])],
    )
    .await;
    run.until(
        "main's keep-warm",
        |e| matches!(e, AgentEvent::KeepWarmChanged { agent, .. } if agent.is_root()),
    )
    .await;
    run.say("hello").await;
    assert!(matches!(
        run.root_ended().await,
        TurnOutcome::Completed { .. }
    ));
    tokio::time::sleep(IDLE).await;
    let run = run.quit().await;
    assert_eq!(
        warm_changes(&run, "/"),
        [(0, 2, true), (0, 2, false), (1, 2, false)]
    );
}

#[tokio::test(start_paused = true)]
async fn the_main_session_is_not_announced_when_keep_warm_is_off_or_the_limit_is_0() {
    for setup in [
        marked(),
        Setup {
            keep_warm_main: true,
            ..limit(0)
        },
    ] {
        let mut run = start("warm-main-off", setup, vec![("/", vec![done("hi")])]).await;
        run.say("hello").await;
        run.root_ended().await;
        let run = run.quit().await;
        assert!(warm_changes(&run, "/").is_empty());
    }
}

#[tokio::test(start_paused = true)]
async fn a_resumed_kept_warm_child_lists_as_paused_until_its_next_turn() {
    let mut run = start(
        "warm-resume-paused",
        marked(),
        vec![("/a", reports()), ("/", vec![done("noted"), done("noted")])],
    )
    .await;
    run.spawn(kept_warm("a", "Look.")).await;
    idle_children(&mut run, &["/a"]).await;
    let old = run.quit().await;
    let mut back = super::resume::resume(
        old,
        marked(),
        vec![
            ("/a", vec![done("again"), done("warm")]),
            ("/", vec![done("noted")]),
        ],
    )
    .await;
    assert_eq!(
        listed(&mut back.run, "/a").await,
        Some(wisp_protocol::KeepWarm {
            refreshes: 0,
            limit: 8,
            paused: true,
        })
    );
    back.run
        .send(ClientCommand::Child {
            agent: path("/a"),
            command: Box::new(ClientCommand::send("c1", "once more")),
        })
        .await;
    back.run.child_ended("/a").await;
    tokio::time::sleep(Duration::from_secs(5)).await;
    assert_eq!(
        listed(&mut back.run, "/a").await,
        Some(wisp_protocol::KeepWarm {
            refreshes: 0,
            limit: 8,
            paused: false,
        })
    );
    back.run.quit().await;
}

#[tokio::test(start_paused = true)]
async fn a_restarted_idle_child_is_armed_with_its_count_but_a_stopped_one_is_paused() {
    let mut run = start(
        "warm-restarted",
        ticking(8),
        vec![
            ("/a", reports()),
            ("/s", reports()),
            ("/", vec![done("noted"); 4]),
        ],
    )
    .await;
    run.spawn(kept_warm("a", "Work.")).await;
    run.spawn(kept_warm("s", "Work.")).await;
    idle_children(&mut run, &["/a", "/s"]).await;
    tokio::time::sleep(IDLE).await;
    assert_eq!(refreshes(&run, "/a"), 1);
    let child = run.providers.get("/s");
    child.clear();
    child.push([hanging("unfinished")]);
    let before = run.requests("/s").len();
    run.send(super::resume::child_send("/s", "stop", "Work again."))
        .await;
    assert!(until(|| run.requests("/s").len() == before + 1).await);
    run.send(super::resume::cancel("/s")).await;
    assert!(matches!(
        run.child_ended("/s").await,
        TurnOutcome::Cancelled { .. }
    ));
    let run = run.quit().await;
    let mut back = super::resume::resume(
        run,
        Setup {
            restarted: true,
            ..ticking(8)
        },
        vec![],
    )
    .await;
    tokio::time::sleep(Duration::from_secs(5)).await;
    assert_eq!(
        listed(&mut back.run, "/a").await,
        Some(wisp_protocol::KeepWarm {
            refreshes: 1,
            limit: 8,
            paused: false
        })
    );
    assert_eq!(
        listed(&mut back.run, "/s").await,
        Some(wisp_protocol::KeepWarm {
            refreshes: 0,
            limit: 8,
            paused: true
        })
    );
    back.run.quit().await;
}

#[tokio::test(start_paused = true)]
async fn a_refresh_that_missed_is_announced_as_a_pause() {
    let mut run = start(
        "warm-missed",
        marked(),
        vec![("/a", reports()), ("/", vec![done("noted"), done("noted")])],
    )
    .await;
    run.spawn(kept_warm("a", "Look.")).await;
    idle_children(&mut run, &["/a"]).await;
    tokio::time::sleep(IDLE * 3).await;
    assert_eq!(
        listed(&mut run, "/a").await,
        Some(wisp_protocol::KeepWarm {
            refreshes: 1,
            limit: 8,
            paused: true,
        })
    );
    let run = run.quit().await;
    assert_eq!(
        refresh_records(&run, "/a"),
        [wisp_protocol::RefreshOutcome::Missed]
    );
    assert_eq!(warm_changes(&run, "/a"), [(1, 8, true)]);
}

#[tokio::test(start_paused = true)]
async fn an_idle_compaction_of_main_announces_a_pause() {
    let setup = Setup {
        keep_warm_main: true,
        ..limit(2)
    };
    let mut run = start(
        "warm-compact",
        setup,
        vec![
            ("/", vec![done("hi"), done("summary"), done("after")]),
            (
                "/a",
                vec![
                    uses(&[report_call("r1", "Done")]),
                    done("summary"),
                    done("again"),
                    done("warm"),
                ],
            ),
        ],
    )
    .await;
    run.say("hello").await;
    run.root_ended().await;
    let request = run.request();
    run.send(ClientCommand::Compact {
        focus: None,
        request,
    })
    .await;
    tokio::time::sleep(Duration::from_secs(5)).await;
    run.spawn(kept_warm("a", "Look.")).await;
    idle_children(&mut run, &["/a"]).await;
    let request = run.request();
    run.send(child(
        "/a",
        ClientCommand::Compact {
            focus: None,
            request,
        },
    ))
    .await;
    tokio::time::sleep(IDLE).await;
    assert_eq!(refreshes(&run, "/a"), 0);
    run.send(child("/a", ClientCommand::send("c1", "once more")))
        .await;
    idle_children(&mut run, &["/a"]).await;
    tokio::time::sleep(IDLE).await;
    assert_eq!(refreshes(&run, "/a"), 1);
    let run = run.quit().await;
    assert_eq!(
        warm_changes(&run, "/"),
        [(0, 2, true), (0, 2, false), (0, 2, true)]
    );
    assert_eq!(warm_changes(&run, "/a"), [(0, 2, true), (0, 2, false)]);
    main_spend_follows_each_cost(&run);
}

#[tokio::test(start_paused = true)]
async fn a_completed_turn_that_leaves_no_anchor_announces_a_pause() {
    let setup = Setup {
        digests: false,
        ..limit(2)
    };
    let mut run = start(
        "warm-no-anchor",
        setup,
        vec![("/a", reports()), ("/", vec![done("noted"), done("noted")])],
    )
    .await;
    run.spawn(kept_warm("a", "Look.")).await;
    idle_children(&mut run, &["/a"]).await;
    tokio::time::sleep(IDLE).await;
    let run = run.quit().await;
    assert_eq!(refreshes(&run, "/a"), 0);
    assert_eq!(warm_changes(&run, "/a"), [(0, 2, true)]);
}

#[tokio::test(start_paused = true)]
async fn a_refresh_landing_after_a_turn_started_announces_nothing() {
    let gate = Arc::new(Semaphore::new(0));
    let mut run = start(
        "warm-detached",
        limit(2),
        vec![
            (
                "/a",
                vec![
                    uses(&[report_call("r1", "Done")]),
                    gated(done("warm"), &gate),
                    uses(&[report_call("r2", "Again")]),
                ],
            ),
            ("/", vec![done("noted"), done("noted"), done("noted")]),
        ],
    )
    .await;
    run.spawn(kept_warm("a", "Look.")).await;
    idle_children(&mut run, &["/a"]).await;
    tokio::time::sleep(IDLE).await;
    assert_eq!(refreshes(&run, "/a"), 1);
    run.send(ClientCommand::Child {
        agent: path("/a"),
        command: Box::new(ClientCommand::send("c1", "once more")),
    })
    .await;
    run.child_ended("/a").await;
    gate.add_permits(1);
    tokio::time::sleep(Duration::from_secs(5)).await;
    let run = run.quit().await;
    assert_eq!(
        refresh_records(&run, "/a"),
        [wisp_protocol::RefreshOutcome::Read]
    );
    assert!(warm_changes(&run, "/a").is_empty());
}
