use super::*;
use crate::agents::guarantee::StopCause;
use crate::resume::LoggedSession;
use wisp_protocol::InputId;
use wisp_protocol::{QueueReason, RetryClass};

pub(super) const SUMMARY: &str = "## Goal and standing instructions\nReport twice.\n\n## Decisions\nNone yet.\n\n## State of the work\nDone: one report.\n\n## Files, branches and artefacts\nNone.\n\n## Commands, errors and fixes\nNone.\n\n## Open questions\nNone.";

pub(super) const LOW_WISP: &str =
    "[models.\"claude-opus-5-5\"]\ncompact_at = 2000\nnative_compaction = false\n";

pub(super) fn priced(mut step: Step, input: u64, output: u64) -> Step {
    step.script = step
        .script
        .into_iter()
        .map(|event| match event {
            Ok(ProviderEvent::MessageStart {
                id,
                model,
                mut usage,
                iterations,
                diagnostics,
            }) => {
                usage.input_tokens = input;
                Ok(ProviderEvent::MessageStart {
                    id,
                    model,
                    usage,
                    iterations,
                    diagnostics,
                })
            }
            Ok(ProviderEvent::MessageDelta {
                stop_reason,
                stop_details,
                mut usage,
                iterations,
                diagnostics,
            }) => {
                usage.input_tokens = input;
                usage.output_tokens = output;
                Ok(ProviderEvent::MessageDelta {
                    stop_reason,
                    stop_details,
                    usage,
                    iterations,
                    diagnostics,
                })
            }
            other => other,
        })
        .collect();
    step
}

fn broken_attempt() -> Step {
    let usage = Usage {
        input_tokens: 800,
        ..Usage::default()
    };
    Step::from(vec![
        Ok(ProviderEvent::Sending),
        Ok(started(Some("msg_cut"), MODEL, usage)),
        Err(mid_stream(RetryClass::Transient)),
    ])
}

fn state_changes(seen: &[AgentEvent], agent: &str) -> Vec<(AgentState, f64, bool)> {
    seen.iter()
        .filter_map(|e| match e {
            AgentEvent::AgentStateChanged {
                agent: a,
                state,
                spent_usd,
                lower_bound,
                ..
            } if a.as_str() == agent => Some((*state, *spent_usd, *lower_bound)),
            _ => None,
        })
        .collect()
}

fn streamed_at(timeline: &[Moment], path: &str) -> Option<usize> {
    timeline
        .iter()
        .position(|m| matches!(m, Moment::Streamed(p) if p == path))
}

fn queued(reason: QueueReason) -> AgentState {
    AgentState::Queued { reason }
}

async fn spawned(run: &mut Run, name: &str) -> AgentEntry {
    run.spawn(spawn_request(name, "Work.")).await;
    spawn_seen(run, &format!("/{name}")).await
}

async fn spawn_seen(run: &mut Run, wanted: &str) -> AgentEntry {
    let event = run
        .until(
            "the spawn",
            |e| matches!(e, AgentEvent::AgentSpawned { agent, .. } if agent.path.as_str() == wanted),
        )
        .await;
    let AgentEvent::AgentSpawned { agent, .. } = event else {
        unreachable!()
    };
    agent
}

async fn sent_once(run: &Run, path: &str) {
    for _ in 0..200 {
        if !sent_at(&run.providers.timeline(), path).is_empty() {
            return;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    panic!("{path} sent nothing: {:?}", run.providers.timeline());
}

#[tokio::test(start_paused = true)]
async fn a_fifth_running_child_queues_and_starts_when_one_ends() {
    let names = ["a", "b", "c", "d", "e", "f"];
    let paths = ["/a", "/b", "/c", "/d", "/e", "/f"];
    let gates: Vec<Arc<Semaphore>> = names.iter().map(|_| Arc::new(Semaphore::new(0))).collect();
    let mut scripts: Vec<(&str, Vec<Step>)> = paths
        .iter()
        .zip(&gates)
        .map(|(p, g)| (*p, vec![gated(uses(&[report_call("r1", "done")]), g)]))
        .collect();
    scripts.push(("/", vec![done("noted"); 7]));
    let mut run = start("limits-fifth", Setup::default(), scripts).await;
    for name in &names[..4] {
        let entry = spawned(&mut run, name).await;
        assert_eq!(entry.state, AgentState::Running, "{name}");
    }
    let fifth = spawned(&mut run, "e").await;
    assert_eq!(fifth.state, queued(QueueReason::Concurrency));
    assert_eq!(fifth.running_ms, None);
    run.until("the fifth's held wake", |e| {
        matches!(
            inner(e, "/e"),
            Some(AgentEvent::WakeHeld {
                reason: HoldReason::Concurrency,
                ..
            })
        )
    })
    .await;
    let sixth = spawned(&mut run, "f").await;
    assert_eq!(sixth.state, queued(QueueReason::Concurrency));
    run.send(ClientCommand::Child {
        agent: path("/e"),
        command: Box::new(ClientCommand::send("you-1", "Also this.")),
    })
    .await;
    tokio::time::sleep(Duration::from_secs(30)).await;
    for path in &paths[..4] {
        assert_eq!(run.requests(path).len(), 1, "{path}");
    }
    assert!(run.requests("/e").is_empty());
    run.send(ClientCommand::GetReady { on: true }).await;
    run.until("getting ready", |e| matches!(e, AgentEvent::Restart { .. }))
        .await;
    gates[2].add_permits(1);
    run.child_ended("/c").await;
    tokio::time::sleep(Duration::from_secs(30)).await;
    assert!(run.requests("/e").is_empty());
    assert!(run.requests("/f").is_empty());
    run.send(ClientCommand::GetReady { on: false }).await;
    run.until("the fifth to run", |e| {
        matches!(
            e,
            AgentEvent::AgentStateChanged { agent, state: AgentState::Running, .. }
                if agent.as_str() == "/e"
        )
    })
    .await;
    sent_once(&run, "/e").await;
    let timeline = run.providers.timeline();
    let ended_c = streamed_at(&timeline, "/c").unwrap();
    assert!(sent_at(&timeline, "/e")[0].0 > ended_c, "{timeline:?}");
    for path in ["/a", "/b", "/d"] {
        assert!(streamed_at(&timeline, path).is_none(), "{timeline:?}");
    }
    assert!(run.requests("/f").is_empty());
    for gate in &gates {
        gate.add_permits(1);
    }
    for path in ["/a", "/b", "/d", "/e", "/f"] {
        run.child_ended(path).await;
    }
    let run = run.quit().await;
    assert_eq!(
        run.requests("/e")[0].prefix(),
        run.requests("/a")[0].prefix(),
        "a queued spawn sends the same prefix"
    );
    let states = state_changes(&run.seen, "/e");
    assert!(
        states.iter().all(|(s, ..)| *s
            != AgentState::Held {
                reason: HoldReason::Concurrency
            }),
        "{states:?}"
    );
    let held: Vec<HoldReason> = pick!(
        run.records("/e"),
        LogEvent::WakeHeld { reason, .. } => reason
    );
    assert_eq!(held, [HoldReason::Concurrency]);
}

#[tokio::test(start_paused = true)]
async fn a_freed_place_goes_to_the_oldest_waiter_across_the_tree() {
    let [a1, a2, b1, w1, open] = [(); 5].map(|()| Arc::new(Semaphore::new(0)));
    let work = |gate: &Arc<Semaphore>| vec![gated(uses(&[report_call("r1", "done")]), gate)];
    let setup = Setup {
        presets: presets("[agents.presets.lead]\ndescription = \"Leads\"\nworker = false\n"),
        limits: Limits {
            max_running: Some(1),
            max_tree_running: Some(2),
            ..Limits::default()
        },
        ..Setup::default()
    };
    let lead = |calls: &[B], turns: usize| {
        let mut steps = vec![uses(calls), gated(done("noted"), &open)];
        steps.extend(vec![done("noted"); turns]);
        steps.push(uses(&[report_call("r9", "led")]));
        steps
    };
    let scripts = vec![
        ("/", vec![done("noted"); 6]),
        (
            "/a",
            lead(
                &[
                    spawn_call("s1", "a1", "Work."),
                    spawn_call("s2", "a2", "Work."),
                ],
                1,
            ),
        ),
        ("/b", lead(&[spawn_call("s1", "b1", "Work.")], 0)),
        ("/a/a1", work(&a1)),
        ("/a/a2", work(&a2)),
        ("/b/b1", work(&b1)),
        ("/w1", work(&w1)),
    ];
    let mut run = start("limits-tree", setup, scripts).await;
    let leader = |name| SpawnRequest {
        preset: Some("lead".into()),
        ..spawn_request(name, "Lead.")
    };
    let waiting = queued(QueueReason::Concurrency);
    run.spawn(leader("a")).await;
    for (path, state) in [
        ("/a", AgentState::Running),
        ("/a/a1", AgentState::Running),
        ("/a/a2", waiting),
    ] {
        assert_eq!(spawn_seen(&mut run, path).await.state, state, "{path}");
    }
    assert_eq!(spawned(&mut run, "w1").await.state, AgentState::Running);
    open.add_permits(2);
    run.spawn(leader("b")).await;
    assert_eq!(spawn_seen(&mut run, "/b").await.state, AgentState::Running);
    assert_eq!(spawn_seen(&mut run, "/b/b1").await.state, waiting);
    sent_once(&run, "/a/a1").await;
    sent_once(&run, "/w1").await;
    w1.add_permits(1);
    run.child_ended("/w1").await;
    sent_once(&run, "/b/b1").await;
    tokio::time::sleep(Duration::from_secs(30)).await;
    assert!(run.requests("/a/a2").is_empty());
    a1.add_permits(1);
    run.child_ended("/a/a1").await;
    sent_once(&run, "/a/a2").await;
    run.child_ended("/a").await;
    run.child_ended("/a").await;
    assert_eq!(run.requests("/a").len(), 3);
    let timeline = run.providers.timeline();
    for (waiter, freed) in [("/b/b1", "/w1"), ("/a/a2", "/a/a1")] {
        let freed_at = streamed_at(&timeline, freed).unwrap();
        assert!(sent_at(&timeline, waiter)[0].0 > freed_at, "{timeline:?}");
    }
    for running in ["/a/a2", "/b/b1"] {
        assert!(streamed_at(&timeline, running).is_none(), "{timeline:?}");
    }
    a2.add_permits(1);
    b1.add_permits(1);
    for path in ["/a/a2", "/b/b1", "/a", "/b", "/b"] {
        run.child_ended(path).await;
    }
    run.quit().await;
}

#[tokio::test(start_paused = true)]
async fn main_compacting_inside_a_turn_publishes_its_spend_before_the_turn_ends() {
    let setup = Setup {
        models: Some(LOW_WISP),
        ..Setup::default()
    };
    let steps = vec![
        priced(uses(&[call("c1", "nothing", json!({}))]), 5_000, 40),
        priced(done(SUMMARY), 3_000, 300),
        done("finished"),
    ];
    let mut run = start("limits-main-compacts", setup, vec![("/", steps)]).await;
    run.say("go").await;
    run.root_ended().await;
    let run = run.quit().await;
    let at = |want: fn(&AgentEvent) -> bool| run.seen.iter().position(want).unwrap();
    let compacted = at(|e| matches!(e, AgentEvent::Compacted { .. }));
    let ended = at(|e| turn_ended(e).is_some());
    let AgentEvent::Compacted { cost: Some(c), .. } = &run.seen[compacted] else {
        panic!("{:?}", run.seen[compacted]);
    };
    let published = run.seen[..ended]
        .iter()
        .any(|e| matches!(e, AgentEvent::SpendChanged { total, .. } if total.usd == c.session_usd));
    assert!(c.turn_usd > 0.0 && published, "{:#?}", run.seen);
    main_spend_follows_each_cost(&run);
}

#[tokio::test(start_paused = true)]
async fn the_tally_counts_a_compaction_and_a_failed_attempt_and_equals_the_childs_session_cost() {
    let mut run = start(
        "limits-tally",
        Setup {
            models: Some(LOW_WISP),
            ..Setup::default()
        },
        vec![
            (
                "/c",
                vec![
                    priced(uses(&[report_call("r1", "first")]), 5_000, 40),
                    priced(done(SUMMARY), 3_000, 300),
                    broken_attempt(),
                    uses(&[report_call("r2", "second")]),
                ],
            ),
            ("/", vec![done("noted"), done("noted again")]),
        ],
    )
    .await;
    run.spawn(spawn_request("c", "Work.")).await;
    run.child_ended("/c").await;
    let first = run.root_ended().await;
    let TurnOutcome::Completed {
        cost: Some(first_cost),
        ..
    } = first
    else {
        panic!("{first:?}");
    };
    run.send(ClientCommand::Child {
        agent: path("/c"),
        command: Box::new(ClientCommand::send("you-1", "Again, please.")),
    })
    .await;
    let second_turn = run.child_ended("/c").await;
    let TurnOutcome::Completed {
        cost: Some(child_cost),
        ..
    } = second_turn
    else {
        panic!("{second_turn:?}");
    };
    let root = run.root_ended().await;
    let TurnOutcome::Completed {
        cost: Some(root_cost),
        ..
    } = root
    else {
        panic!("{root:?}");
    };
    let run = run.quit().await;
    let child: Vec<&AgentEvent> = run.seen.iter().filter_map(|e| inner(e, "/c")).collect();
    let compacted: f64 = child
        .iter()
        .filter_map(|e| match e {
            AgentEvent::Compacted { cost: Some(c), .. } => Some(c.turn_usd),
            _ => None,
        })
        .sum();
    let retried: f64 = child
        .iter()
        .filter_map(|e| match e {
            AgentEvent::Retrying { cost_usd, .. } => *cost_usd,
            _ => None,
        })
        .sum();
    let steps: f64 = child
        .iter()
        .filter_map(|e| match e {
            AgentEvent::StepEnded { cost_usd, .. } => *cost_usd,
            _ => None,
        })
        .sum();
    assert!(compacted > 0.0, "{child:#?}");
    assert!(retried > 0.0, "{child:#?}");
    let session = child_cost.session_usd;
    assert!((session - (steps + compacted + retried)).abs() < 1e-9);
    let logged = LoggedSession::read(&run.store.read(&session_name("/c")).unwrap()).unwrap();
    assert!((logged.session_usd - session).abs() < 1e-12);
    let lines: Vec<crate::spend::Record> = run
        .store
        .spend_lines()
        .iter()
        .map(|line| serde_json::from_str(line).unwrap())
        .filter(|record: &crate::spend::Record| record.session == session_name("/c"))
        .collect();
    assert_eq!(
        lines.len(),
        4,
        "each step, compaction and failed attempt: {lines:?}"
    );
    assert!(
        lines
            .iter()
            .all(|record| record.account == ENV && record.model == MODEL)
    );
    assert!(
        (lines
            .iter()
            .filter_map(|record| record.spend.usd)
            .sum::<f64>()
            - session)
            .abs()
            < 1e-12
    );
    assert!(child_cost.session_lower_bound);
    let (_, spent, lower) = *state_changes(&run.seen, "/c").last().unwrap();
    assert!((spent - session).abs() < 1e-12);
    assert!(lower);
    assert!((root_cost.agents_usd.unwrap() - session).abs() < 1e-12);
    assert!(root_cost.agents_lower_bound);
    assert!(first_cost.agents_usd.unwrap() < session);
    assert!(!first_cost.agents_lower_bound);
    let mut last = 0.0;
    let spends: Vec<f64> = state_changes(&run.seen, "/c")
        .into_iter()
        .map(|(_, usd, _)| usd)
        .collect();
    for usd in &spends {
        assert!(*usd >= last, "{spends:?}");
        last = *usd;
    }
    let mut distinct = spends.clone();
    distinct.dedup();
    assert_eq!(
        distinct.len(),
        4,
        "each step, the compaction and the attempt: {spends:?}"
    );
}

#[tokio::test(start_paused = true)]
async fn same_configuration_spawns_wait_for_the_first_request_to_stream() {
    let gate = Arc::new(Semaphore::new(0));
    let mut run = start(
        "limits-stagger",
        Setup {
            prints: Some(Prints::Real),
            ..Setup::default()
        },
        vec![
            ("/a", vec![gated(uses(&[report_call("r1", "a")]), &gate)]),
            ("/b", vec![uses(&[report_call("r1", "b")])]),
            ("/c", vec![uses(&[report_call("r1", "c")])]),
            ("/", vec![done("noted"); 4]),
        ],
    )
    .await;
    spawned(&mut run, "a").await;
    sent_once(&run, "/a").await;
    let canary = "CANARYOpaqueInStaggeredBrief";
    run.spawn(SpawnRequest {
        todos: vec![canary.into()],
        ..spawn_request("b", "Work.")
    })
    .await;
    spawn_seen(&mut run, "/b").await;
    spawned(&mut run, "c").await;
    for agent in ["/b", "/c"] {
        run.until("a staggered spawn", |e| {
            matches!(
                e,
                AgentEvent::AgentStateChanged { agent: a, state, .. }
                    if a.as_str() == agent && *state == queued(QueueReason::Stagger)
            )
        })
        .await;
    }
    let held_at = tokio::time::Instant::now();
    tokio::time::sleep(Duration::from_secs(2)).await;
    let timeline = run.providers.timeline();
    assert_eq!(timeline.len(), 1, "{timeline:?}");
    crate::session::register_secrets([canary.to_owned()]);
    gate.add_permits(1);
    for agent in ["/a", "/b", "/c"] {
        run.child_ended(agent).await;
    }
    let run = run.quit().await;
    let timeline = run.providers.timeline();
    let first = streamed_at(&timeline, "/a").unwrap();
    for agent in ["/b", "/c"] {
        let sent = sent_at(&timeline, agent);
        assert_eq!(sent.len(), 1, "{timeline:?}");
        assert!(sent[0].0 > first, "{timeline:?}");
        assert!(sent[0].1 < held_at + crate::warmth::STAGGER_FOR);
    }
    let writes: Vec<u64> = ["/a", "/b", "/c"]
        .iter()
        .map(|agent| {
            run.seen
                .iter()
                .filter_map(|e| inner(e, agent))
                .filter_map(|e| match e {
                    AgentEvent::TurnEnded {
                        outcome: TurnOutcome::Completed { usage, .. },
                        ..
                    } => Some(usage.cache_creation_input_tokens),
                    _ => None,
                })
                .sum()
        })
        .collect();
    assert_eq!(writes, [PREFIX_TOKENS, 0, 0]);
    let prefixes: Vec<_> = ["/a", "/b", "/c"]
        .iter()
        .map(|agent| run.requests(agent)[0].prefix())
        .collect();
    assert_eq!(prefixes[0], prefixes[1]);
    assert_eq!(prefixes[0], prefixes[2]);
    let a = &run.requests("/a")[0];
    let b = &run.requests("/b")[0];
    let a_body = a.body();
    let b_body = b.body();
    assert!(b_body.to_string().contains("Work."));
    assert!(b_body.to_string().contains("[redacted]"));
    assert!(!b_body.to_string().contains(canary));
    for key in ["model", "tools", "system", "cache_control"] {
        assert_eq!(a_body[key], b_body[key], "{key}");
    }
    let print = crate::provider::anthropic::wire_print(&b.request()).unwrap();
    let first = crate::provider::anthropic::wire_print(&a.request()).unwrap();
    assert_eq!(print.pinned, first.pinned);
    assert_eq!(print.betas, first.betas);
    assert_ne!(print.messages[0], first.messages[0]);
    let mut stale = b.clone();
    for block in &mut stale.messages[0].content {
        if let ContentBlock::Text { text } = block {
            *text = text.replace("[redacted]", canary);
        }
    }
    let stale = crate::provider::anthropic::wire_print(&stale.request()).unwrap();
    assert_eq!(stale.pinned, print.pinned);
    assert_eq!(stale.betas, print.betas);
    assert_ne!(stale.messages[0], print.messages[0]);
    let states = state_changes(&run.seen, "/b");
    let at = states
        .iter()
        .position(|(s, ..)| *s == queued(QueueReason::Stagger))
        .unwrap();
    assert_eq!(states[at + 1].0, AgentState::Running, "{states:?}");
    let mut run = super::resume::resume(
        run,
        Setup {
            prints: Some(Prints::Real),
            ..Setup::default()
        },
        vec![
            (
                "/",
                vec![
                    uses(&[send_call("followup", "/b", "Continue the task.")]),
                    done("noted"),
                    done("received"),
                ],
            ),
            ("/b", vec![uses(&[report_call("r2", "b followed up")])]),
        ],
    )
    .await
    .run;
    run.say("Follow up with b").await;
    super::resume::reported(&mut run, "/b").await;
    let next = run.requests("/b").last().unwrap().body();
    crate::test_guard::prefix::assert_prefix_stable(&[b_body, next], None);
    run.quit().await;
}

#[tokio::test(start_paused = true)]
async fn a_failing_first_request_hands_the_prefix_to_one_waiting_request() {
    let gate = Arc::new(Semaphore::new(0));
    let next = Arc::new(Semaphore::new(0));
    let mut run = start(
        "limits-stagger-fails",
        Setup {
            prints: Some(Prints::Real),
            ..Setup::default()
        },
        vec![
            ("/a", vec![gated(refused_request(), &gate)]),
            ("/b", vec![gated(uses(&[report_call("r1", "b")]), &next)]),
            ("/c", vec![gated(uses(&[report_call("r1", "c")]), &next)]),
            ("/", vec![done("noted"); 4]),
        ],
    )
    .await;
    spawned(&mut run, "a").await;
    sent_once(&run, "/a").await;
    spawned(&mut run, "b").await;
    spawned(&mut run, "c").await;
    let waiting_from = tokio::time::Instant::now();
    tokio::time::sleep(Duration::from_secs(1)).await;
    assert_eq!(run.providers.timeline().len(), 1);
    gate.add_permits(1);
    let failed = run.child_ended("/a").await;
    assert!(matches!(failed, TurnOutcome::Error { .. }), "{failed:?}");
    tokio::time::sleep(Duration::from_secs(1)).await;
    let timeline = run.providers.timeline();
    let sent: Vec<&str> = ["/b", "/c"]
        .into_iter()
        .filter(|agent| !sent_at(&timeline, agent).is_empty())
        .collect();
    assert_eq!(
        sent.len(),
        1,
        "one waiting request takes the prefix: {timeline:?}"
    );
    let warmer = sent[0];
    let other = if warmer == "/b" { "/c" } else { "/b" };
    next.add_permits(2);
    for agent in ["/b", "/c"] {
        run.child_ended(agent).await;
    }
    let run = run.quit().await;
    let timeline = run.providers.timeline();
    let streamed = streamed_at(&timeline, warmer).unwrap();
    let later = sent_at(&timeline, other);
    assert_eq!(later.len(), 1, "{timeline:?}");
    assert!(later[0].0 > streamed, "{timeline:?}");
    assert!(
        later[0].1 < waiting_from + crate::warmth::STAGGER_FOR,
        "within what was left of its 5 s"
    );
    assert!(sent_at(&timeline, warmer)[0].1 < waiting_from + Duration::from_secs(2));
}

#[tokio::test(start_paused = true)]
async fn the_root_never_waits_on_a_warming_prefix() {
    let gate = Arc::new(Semaphore::new(0));
    let mut run = start(
        "limits-root",
        Setup {
            prints: Some(Prints::Shared),
            ..Setup::default()
        },
        vec![
            ("/a", vec![gated(uses(&[report_call("r1", "a")]), &gate)]),
            ("/", vec![done("hello"), done("noted")]),
        ],
    )
    .await;
    spawned(&mut run, "a").await;
    sent_once(&run, "/a").await;
    run.say("hi").await;
    let outcome = run.root_ended().await;
    assert!(
        matches!(outcome, TurnOutcome::Completed { .. }),
        "{outcome:?}"
    );
    let timeline = run.providers.timeline();
    assert!(streamed_at(&timeline, "/a").is_none(), "{timeline:?}");
    assert_eq!(sent_at(&timeline, "/").len(), 1, "{timeline:?}");
    gate.add_permits(1);
    run.child_ended("/a").await;
    run.quit().await;
}

#[tokio::test(start_paused = true)]
async fn nothing_stops_on_cost() {
    let expensive = |step: Step| priced(step, 100, 20_000_000);
    let mut run = start(
        "limits-no-caps",
        Setup::default(),
        vec![
            (
                "/rich",
                vec![
                    expensive(uses(&[spawn_call("s1", "deeper", "Dig.")])),
                    expensive(uses(&[report_call("r1", "spent a lot")])),
                ],
            ),
            (
                "/rich/deeper",
                vec![expensive(uses(&[report_call("r1", "deeper")]))],
            ),
            ("/", vec![expensive(done("noted")); 3]),
        ],
    )
    .await;
    run.spawn(spawn_request("rich", "Spend.")).await;
    run.child_ended("/rich/deeper").await;
    let outcome = run.child_ended("/rich").await;
    assert!(
        matches!(outcome, TurnOutcome::Completed { .. }),
        "{outcome:?}"
    );
    let root = run.root_ended().await;
    let TurnOutcome::Completed {
        cost: Some(cost), ..
    } = root
    else {
        panic!("{root:?}");
    };
    assert!(cost.agents_usd.unwrap() > 1_000.0, "{cost:?}");
    assert!(cost.turn_usd > 100.0, "{cost:?}");
    let run = run.quit().await;
    assert!(
        !run.seen.iter().any(|e| matches!(
            e,
            AgentEvent::TurnEnded {
                outcome: TurnOutcome::Error { .. },
                ..
            }
        ) || matches!(
            inner(e, "/rich"),
            Some(AgentEvent::TurnEnded {
                outcome: TurnOutcome::Error { .. },
                ..
            })
        )),
        "{:#?}",
        run.seen
    );
}

fn reached(seen: &[AgentEvent], agent: &str, wanted: AgentState) -> bool {
    state_changes(seen, agent)
        .iter()
        .any(|(state, ..)| *state == wanted)
}

#[tokio::test(start_paused = true)]
async fn a_stopped_queued_child_never_runs_when_a_permit_frees() {
    let paths = ["/a", "/b", "/c", "/d"];
    let gates: Vec<Arc<Semaphore>> = paths.iter().map(|_| Arc::new(Semaphore::new(0))).collect();
    let mut scripts: Vec<(&str, Vec<Step>)> = paths
        .iter()
        .zip(&gates)
        .map(|(p, g)| (*p, vec![gated(uses(&[report_call("r1", "done")]), g)]))
        .collect();
    scripts.push(("/e", vec![uses(&[report_call("r1", "e")])]));
    scripts.push(("/", vec![done("noted"); 8]));
    let mut run = start("limits-stop-queued", Setup::default(), scripts).await;
    for name in ["a", "b", "c", "d"] {
        spawned(&mut run, name).await;
    }
    let fifth = spawned(&mut run, "e").await;
    assert_eq!(fifth.state, queued(QueueReason::Concurrency));
    run.send(ClientCommand::Child {
        agent: path("/e"),
        command: Box::new(ClientCommand::Cancel),
    })
    .await;
    run.until("the queued child to stop", |e| {
        matches!(
            e,
            AgentEvent::AgentStateChanged { agent, state, .. }
                if agent.as_str() == "/e" && *state == stopped()
        )
    })
    .await;
    gates[2].add_permits(1);
    run.child_ended("/c").await;
    tokio::time::sleep(Duration::from_secs(30)).await;
    assert!(run.requests("/e").is_empty());
    let notice = crate::agents::stopped_text(&path("/e"), StopCause::User);
    assert!(
        run.mail_to("/")
            .iter()
            .any(|(kind, wake, text)| *kind == MailKind::Notice && !wake && text.contains(&notice)),
        "{:?}",
        run.mail_to("/")
    );
    run.send(ClientCommand::Child {
        agent: path("/e"),
        command: Box::new(ClientCommand::send("you-1", "Go on.")),
    })
    .await;
    run.child_ended("/e").await;
    let sent = run.requests("/e");
    assert_eq!(sent.len(), 1);
    let all = sent[0].all_text();
    assert!(all.contains("Work.") && all.contains("Go on."), "{all}");
    for gate in &gates {
        gate.add_permits(1);
    }
    for path in ["/a", "/b", "/d"] {
        run.child_ended(path).await;
    }
    let run = run.quit().await;
    let states = state_changes(&run.seen, "/e");
    let stop = states.iter().position(|(s, ..)| *s == stopped()).unwrap();
    let ran = states
        .iter()
        .position(|(s, ..)| *s == AgentState::Running)
        .unwrap();
    assert!(ran > stop, "{states:?}");
}

#[tokio::test(start_paused = true)]
async fn a_stop_cascades_to_a_queued_grandchild() {
    let workers = ["w1", "w2", "w3", "w4", "w5"];
    let gate = Arc::new(Semaphore::new(0));
    let calls: Vec<B> = workers
        .iter()
        .enumerate()
        .map(|(i, w)| spawn_call(&format!("s{i}"), w, "Work."))
        .collect();
    let mut scripts: Vec<(&str, Vec<Step>)> = vec![
        ("/lead", vec![uses(&calls), done("my workers are on it")]),
        ("/", vec![done("noted"); 4]),
        ("/lead/w5", vec![uses(&[report_call("r1", "w5")])]),
    ];
    for path in ["/lead/w1", "/lead/w2", "/lead/w3", "/lead/w4"] {
        scripts.push((path, vec![gated(uses(&[report_call("r1", "done")]), &gate)]));
    }
    let mut run = start("limits-stop-cascade", Setup::default(), scripts).await;
    run.spawn(spawn_request("lead", "Lead.")).await;
    let queued_w5 = run
        .until("the fifth worker's spawn", |e| {
            matches!(e, AgentEvent::AgentSpawned { agent, .. } if agent.path.as_str() == "/lead/w5")
        })
        .await;
    let AgentEvent::AgentSpawned { agent, .. } = queued_w5 else {
        unreachable!()
    };
    assert_eq!(agent.state, queued(QueueReason::Concurrency));
    run.child_ended("/lead").await;
    let answers = &run.requests("/lead")[1];
    let queued_line = wisp_tool_text::agents::QUEUED_SENTENCE;
    assert!(result_of(answers, "s4").1.ends_with(queued_line));
    assert!(!result_of(answers, "s0").1.contains(queued_line));
    run.send(ClientCommand::Child {
        agent: path("/lead"),
        command: Box::new(ClientCommand::Cancel),
    })
    .await;
    run.until("the queued worker to stop", |e| {
        matches!(
            e,
            AgentEvent::AgentStateChanged { agent, state, .. }
                if agent.as_str() == "/lead/w5" && *state == stopped()
        )
    })
    .await;
    for worker in ["/lead/w1", "/lead/w2", "/lead/w3", "/lead/w4"] {
        run.child_ended(worker).await;
    }
    gate.add_permits(4);
    tokio::time::sleep(Duration::from_secs(30)).await;
    assert!(run.requests("/lead/w5").is_empty());
    let notice = crate::agents::stopped_text(&path("/lead/w5"), StopCause::WithParent);
    assert!(
        run.mail_to("/lead")
            .iter()
            .any(|(kind, wake, text)| *kind == MailKind::Notice && !wake && text.contains(&notice)),
        "{:?}",
        run.mail_to("/lead")
    );
    let run = run.quit().await;
    assert!(!reached(&run.seen, "/lead/w5", AgentState::Running));
}

#[tokio::test(start_paused = true)]
async fn a_child_that_leaves_the_queue_shows_its_idle_state_again() {
    let gate = Arc::new(Semaphore::new(0));
    let mut run = start(
        "limits-leave-queue",
        one_running(),
        vec![
            ("/b", vec![uses(&[report_call("r1", "b")]), done("more")]),
            ("/a", vec![gated(uses(&[report_call("r1", "a")]), &gate)]),
            ("/", vec![done("noted"); 4]),
        ],
    )
    .await;
    spawned(&mut run, "b").await;
    run.child_ended("/b").await;
    spawned(&mut run, "a").await;
    sent_once(&run, "/a").await;
    run.send(ClientCommand::Child {
        agent: path("/b"),
        command: Box::new(ClientCommand::send("you-1", "One more thing.")),
    })
    .await;
    run.until("the follow-up to queue", |e| {
        matches!(
            e,
            AgentEvent::AgentStateChanged { agent, state, .. }
                if agent.as_str() == "/b" && *state == queued(QueueReason::Concurrency)
        )
    })
    .await;
    run.send(ClientCommand::Child {
        agent: path("/b"),
        command: Box::new(ClientCommand::Withdraw {
            ids: vec![InputId("you-1".into())],
        }),
    })
    .await;
    run.until("the idle state to come back", |e| {
        matches!(
            e,
            AgentEvent::AgentStateChanged { agent, state, .. }
                if agent.as_str() == "/b"
                    && *state == AgentState::Idle { outcome: Some(IdleOutcome::Reported) }
        )
    })
    .await;
    assert!(streamed_at(&run.providers.timeline(), "/a").is_none());
    gate.add_permits(1);
    run.child_ended("/a").await;
    tokio::time::sleep(Duration::from_secs(30)).await;
    assert_eq!(run.requests("/b").len(), 1);
    run.quit().await;
}

#[tokio::test(start_paused = true)]
async fn a_stop_cascades_to_a_child_held_for_concurrency() {
    let busy = Arc::new(Semaphore::new(0));
    let report = Arc::new(Semaphore::new(0));
    let mut run = start(
        "limits-stop-held",
        Setup {
            limits: Limits {
                max_running: Some(1),
                max_depth: Some(3),
                ..Limits::default()
            },
            ..Setup::default()
        },
        vec![
            (
                "/a",
                vec![
                    uses(&[
                        spawn_call("s1", "m", "Middle."),
                        spawn_call("s2", "s", "Busy."),
                    ]),
                    done("both on it"),
                ],
            ),
            (
                "/a/m",
                vec![
                    uses(&[spawn_call("s1", "g", "Dig.")]),
                    done("my child is on it"),
                    done("got the report"),
                ],
            ),
            (
                "/a/s",
                vec![gated(uses(&[report_call("r1", "busy")]), &busy)],
            ),
            (
                "/a/m/g",
                vec![gated(uses(&[report_call("r1", "found")]), &report)],
            ),
            ("/", vec![done("noted"); 4]),
        ],
    )
    .await;
    run.spawn(spawn_request("a", "Lead.")).await;
    run.child_ended("/a/m").await;
    sent_once(&run, "/a/s").await;
    report.add_permits(1);
    run.child_ended("/a/m/g").await;
    run.until("the middle agent's held wake", |e| {
        matches!(
            e,
            AgentEvent::AgentStateChanged { agent, state, .. }
                if agent.as_str() == "/a/m"
                    && *state == AgentState::Held { reason: HoldReason::Concurrency }
        )
    })
    .await;
    let asked = run.requests("/a/m").len();
    run.send(ClientCommand::Child {
        agent: path("/a"),
        command: Box::new(ClientCommand::Cancel),
    })
    .await;
    run.until("the held agent to stop", |e| {
        matches!(
            e,
            AgentEvent::AgentStateChanged { agent, state, .. }
                if agent.as_str() == "/a/m" && *state == stopped()
        )
    })
    .await;
    run.child_ended("/a/s").await;
    busy.add_permits(1);
    tokio::time::sleep(Duration::from_secs(30)).await;
    assert_eq!(run.requests("/a/m").len(), asked);
    let notice = crate::agents::stopped_text(&path("/a/m"), StopCause::WithParent);
    assert!(
        run.mail_to("/a")
            .iter()
            .any(|(kind, wake, text)| *kind == MailKind::Notice && !wake && text.contains(&notice)),
        "{:?}",
        run.mail_to("/a")
    );
    run.quit().await;
}

fn no_limits() -> Limits {
    Limits {
        max_running: None,
        max_tree_running: None,
        max_agents: None,
        max_depth: None,
        ..Limits::default()
    }
}

#[tokio::test(start_paused = true)]
async fn with_no_depth_limit_a_child_spawns_past_depth_two_and_is_never_told_of_a_limit() {
    let mut run = start(
        "limits-none-depth",
        Setup {
            limits: no_limits(),
            ..Setup::default()
        },
        vec![
            (
                "/a",
                vec![
                    uses(&[spawn_call("s1", "m", "Go deeper.")]),
                    uses(&[report_call("r1", "a done")]),
                    done("x"),
                ],
            ),
            (
                "/a/m",
                vec![
                    uses(&[spawn_call("s2", "g", "Deeper still.")]),
                    uses(&[report_call("r2", "m done")]),
                    done("x"),
                ],
            ),
            (
                "/a/m/g",
                vec![uses(&[report_call("r3", "g done")]), done("x")],
            ),
            ("/", vec![done("ok"); 2]),
        ],
    )
    .await;
    run.spawn(spawn_request("a", "Top.")).await;
    run.child_ended("/a/m/g").await;
    run.child_ended("/a/m").await;
    run.child_ended("/a").await;
    let deepest = run.requests("/a/m/g");
    assert!(!deepest.is_empty());
    assert!(deepest[0].tool_names().contains(&SPAWN_AGENT.to_owned()));
    assert_eq!(deepest[0].prefix(), run.requests("/a/m")[0].prefix());
    for agent in ["/a/m", "/a/m/g"] {
        let env = run.requests(agent)[0].first_user_texts().join("\n");
        assert!(!env.contains("depth limit"), "{agent}: {env}");
    }
    let run = run.quit().await;
    for agent in ["/a", "/a/m"] {
        assert!(
            !run.records(agent)
                .iter()
                .any(|e| matches!(e, LogEvent::AgentRefused { .. })),
            "{agent}"
        );
    }
}

#[tokio::test(start_paused = true)]
async fn with_no_running_or_tree_limit_every_child_runs_at_once() {
    let names = ["a", "b", "c", "d", "e", "f"];
    let paths = ["/a", "/b", "/c", "/d", "/e", "/f"];
    let gate = Arc::new(Semaphore::new(0));
    let mut scripts: Vec<(&str, Vec<Step>)> = paths
        .iter()
        .map(|p| (*p, vec![gated(uses(&[report_call("r1", "done")]), &gate)]))
        .collect();
    scripts.push(("/", vec![done("noted"); 7]));
    let mut run = start(
        "limits-none-running",
        Setup {
            limits: no_limits(),
            ..Setup::default()
        },
        scripts,
    )
    .await;
    for name in names {
        let entry = spawned(&mut run, name).await;
        assert_eq!(entry.state, AgentState::Running, "{name}");
    }
    for path in paths {
        sent_once(&run, path).await;
    }
    gate.add_permits(paths.len());
    for path in paths {
        run.child_ended(path).await;
    }
    let run = run.quit().await;
    for path in paths {
        assert!(
            !run.records(path)
                .into_iter()
                .any(|e| matches!(e, LogEvent::WakeHeld { .. })),
            "{path}"
        );
    }
}

#[tokio::test(start_paused = true)]
async fn a_child_waiting_for_a_slot_shows_queued_reads_mail_as_running_and_keeps_its_clock() {
    const SPAWNED: i64 = 1_800_000_000_000;
    let clock = Arc::new(ShiftedClock::at(Timestamp::UNIX_EPOCH, None));
    clock.set(Timestamp::from_millisecond(SPAWNED).unwrap());
    let lanes = crate::lanes::Lanes::new(
        std::collections::BTreeMap::from([("env".to_owned(), Some(1))]),
        None,
    );
    let held = match lanes.enter("env", 0) {
        Ok(pass) => pass,
        Err(_) => panic!("the lane was busy"),
    };
    let kernels = Kernels::default();
    let mut run = start(
        "limits-lanes",
        Setup {
            lanes: lanes.clone(),
            kernels: kernels.clone(),
            clock: Some(clock.clone()),
            ..Setup::default()
        },
        vec![
            ("/a", vec![uses(&[report_call("r1", "a")])]),
            ("/", vec![done("noted")]),
        ],
    )
    .await;
    spawned(&mut run, "a").await;
    run.until("a child waiting for a slot", |e| {
        matches!(
            e,
            AgentEvent::AgentStateChanged { agent, state, .. }
                if agent.as_str() == "/a" && *state == queued(QueueReason::Endpoint)
        )
    })
    .await;
    assert!(sent_at(&run.providers.timeline(), "/a").is_empty());
    let (_, delivery) = service_of(&kernels)
        .send_from(&path("/"), &path("/a"), "hello".into(), true)
        .unwrap();
    assert!(
        matches!(delivery, wisp_plugin_api::host::Delivery::Running),
        "{delivery:?}"
    );
    clock.set(Timestamp::from_millisecond(SPAWNED + 60_000).unwrap());
    drop(held);
    run.child_ended("/a").await;
    let run = run.quit().await;
    let states: Vec<AgentState> = state_changes(&run.seen, "/a")
        .into_iter()
        .map(|(state, ..)| state)
        .collect();
    let at = states
        .iter()
        .position(|s| *s == queued(QueueReason::Endpoint))
        .unwrap();
    assert_eq!(states[at + 1], AgentState::Running, "{states:?}");
    let clocks: Vec<(AgentState, Option<u64>)> = run
        .seen
        .iter()
        .filter_map(|e| match e {
            AgentEvent::AgentStateChanged {
                agent,
                state,
                since_ms,
                ..
            } if agent.as_str() == "/a" => Some((*state, *since_ms)),
            _ => None,
        })
        .take(at + 2)
        .collect();
    assert!(
        clocks
            .iter()
            .all(|(_, since)| *since == Some(SPAWNED as u64)),
        "{clocks:?}"
    );
    assert_eq!(lanes.in_flight("env"), 0);
}

#[tokio::test(start_paused = true)]
async fn an_idle_child_stays_idle_while_its_manual_compaction_waits_for_a_slot() {
    let lanes = crate::lanes::Lanes::new(BTreeMap::from([(ENV.to_owned(), Some(1))]), None);
    let kernels = Kernels::default();
    let summary = Arc::new(Semaphore::new(0));
    let mut run = start(
        "limits-idle-compaction-lane",
        Setup {
            lanes: lanes.clone(),
            kernels: kernels.clone(),
            models: Some("[models.\"claude-opus-5-5\"]\nnative_compaction = false\n"),
            ..Setup::default()
        },
        vec![
            (
                "/a",
                vec![
                    priced(uses(&[report_call("r1", "a")]), 60_000, 10),
                    gated(priced(done(SUMMARY), 60_000, 300), &summary),
                ],
            ),
            ("/", vec![done("noted")]),
        ],
    )
    .await;
    spawned(&mut run, "a").await;
    run.child_ended("/a").await;
    let service = service_of(&kernels);
    let idle = AgentState::Idle {
        outcome: Some(IdleOutcome::Reported),
    };
    assert_eq!(service.snapshot()[0].state, idle);
    let at = run.seen.len();
    let held = lanes.enter(ENV, 0).unwrap();
    run.send(child(
        "/a",
        ClientCommand::Compact {
            focus: None,
            request: RequestId(70),
        },
    ))
    .await;
    run.until("the idle compaction waiting for a slot", |e| {
        matches!(
            inner(e, "/a"),
            Some(AgentEvent::RequestQueued { step: 0, .. })
        )
    })
    .await;
    assert_eq!(service.snapshot()[0].state, idle);
    drop(held);
    assert!(until(|| run.requests("/a").len() == 2).await);
    run.send(ClientCommand::GetReady { on: true }).await;
    let rows = super::restart::restart(&mut run, wisp_protocol::RestartState::GettingReady).await;
    assert!(rows.iter().any(|row| row.agent == path("/a")
        && row.what == wisp_protocol::Unfinished::Reply
        && !row.opening));
    summary.add_permits(1);
    run.until("the manual compaction to finish", |e| {
        matches!(
            inner(e, "/a"),
            Some(AgentEvent::Compacted {
                answers: Some(RequestId(70)),
                ..
            })
        )
    })
    .await;
    super::restart::restart(&mut run, wisp_protocol::RestartState::Ready).await;
    assert_eq!(service.snapshot()[0].state, idle);
    assert!(
        state_changes(&run.seen[at..], "/a")
            .iter()
            .all(|(state, ..)| *state == idle)
    );
    let admission: Vec<&str> = run
        .seen
        .iter()
        .filter_map(|e| match inner(e, "/a") {
            Some(AgentEvent::RequestQueued { step: 0, .. }) => Some("queued"),
            Some(AgentEvent::RequestAdmitted { step: 0, .. }) => Some("admitted"),
            _ => None,
        })
        .collect();
    assert_eq!(admission, ["queued", "admitted"]);
    let run = run.quit().await;
    assert_eq!(
        run.seen
            .iter()
            .filter(|e| inner(e, "/a").and_then(turn_ended).is_some())
            .count(),
        1
    );
}

#[tokio::test(start_paused = true)]
async fn a_child_waiting_to_retry_shows_queued_then_running() {
    let busy = classified(
        ErrorKind::RateLimit,
        "HTTP 429: Too many concurrent requests",
        FailurePhase::Response,
        RetryClass::Transient,
    );
    let mut run = start(
        "limits-retry",
        Setup::default(),
        vec![
            (
                "/a",
                vec![
                    Step::from(vec![Ok(ProviderEvent::Sending), Err(busy)]),
                    uses(&[report_call("r1", "a")]),
                ],
            ),
            ("/", vec![done("noted")]),
        ],
    )
    .await;
    spawned(&mut run, "a").await;
    run.child_ended("/a").await;
    let run = run.quit().await;
    let states: Vec<AgentState> = state_changes(&run.seen, "/a")
        .into_iter()
        .map(|(state, ..)| state)
        .collect();
    let at = states
        .iter()
        .position(|s| *s == queued(QueueReason::Retry))
        .unwrap_or_else(|| panic!("{states:?}"));
    assert_eq!(states[at + 1], AgentState::Running, "{states:?}");
}

#[tokio::test(start_paused = true)]
async fn a_child_retrying_past_the_set_minutes_tells_its_parent_once() {
    for (minutes, ms) in [(0, 25_000), (1, 25_000), (1, 125_000)] {
        let error = classified(
            ErrorKind::RateLimit,
            "HTTP 429: placeholder provider detail",
            FailurePhase::Response,
            RetryClass::After { ms },
        );
        let mut steps = vec![Step::from(vec![Ok(ProviderEvent::Sending), Err(error)]); 4];
        steps.push(uses(&[report_call("r1", "Done")]));
        let mut run = start(
            "limits-retry-parent",
            Setup {
                limits: Limits {
                    wake_when_stuck: minutes,
                    ..Limits::default()
                },
                ..Setup::default()
            },
            vec![("/a", steps), ("/", vec![done("Noted"); 2])],
        )
        .await;
        spawned(&mut run, "a").await;
        tokio::time::sleep(Duration::from_secs(59)).await;
        assert!(run.mail_to("/").is_empty());
        assert!(run.requests("/").is_empty());
        tokio::time::sleep(Duration::from_secs(2)).await;
        if minutes > 0 {
            run.root_ended().await;
            assert_eq!(run.mail_to("/").len(), 1);
            assert_eq!(run.requests("/a").len(), if ms == 25_000 { 3 } else { 1 });
        }
        tokio::time::sleep(Duration::from_millis(4 * ms)).await;
        run.child_ended("/a").await;
        run.root_ended().await;
        let run = run.quit().await;
        let mail = run.mail_to("/");
        let kinds: Vec<_> = mail.iter().map(|(kind, _, _)| *kind).collect();
        assert_eq!(
            kinds,
            if minutes == 0 {
                vec![MailKind::Report]
            } else {
                vec![MailKind::Notice, MailKind::Report]
            }
        );
        if minutes > 0 {
            assert!(mail[0].1);
            assert!(
                mail[0]
                    .2
                    .contains(&crate::agents::retrying_text(&path("/a"), 1))
            );
            assert!(!mail[0].2.contains("placeholder provider detail"));
        }
        assert_eq!(run.requests("/a").len(), 5);
        assert_eq!(run.requests("/").len(), if minutes == 0 { 1 } else { 2 });
    }
}

async fn mixed_tree(name: &str, with_c2: bool) -> Run {
    let unpriced = FakeEndpoint {
        priced: false,
        ..endpoint("c")
    };
    let mut run = start(
        name,
        Setup {
            endpoints: vec![endpoint(ENV), endpoint("b"), unpriced],
            ..Setup::default()
        },
        vec![
            ("b:/c1", vec![uses(&[report_call("r1", "one")])]),
            ("c:/c2", vec![uses(&[report_call("r2", "two")])]),
            ("/", vec![done("noted"); 4]),
        ],
    )
    .await;
    run.spawn(SpawnRequest {
        endpoint: Some("b".into()),
        model: Some("claude-sonnet-5-5".into()),
        ..spawn_request("c1", "Work.")
    })
    .await;
    run.child_ended("/c1").await;
    if with_c2 {
        run.spawn(SpawnRequest {
            endpoint: Some("c".into()),
            ..spawn_request("c2", "Work.")
        })
        .await;
        run.child_ended("/c2").await;
    }
    tokio::time::sleep(Duration::from_secs(30)).await;
    run.say("And the total?").await;
    tokio::time::sleep(Duration::from_secs(30)).await;
    run
}

fn last_root_cost(run: &Run) -> wisp_protocol::Cost {
    run.seen
        .iter()
        .filter_map(turn_ended)
        .filter_map(|o| match o {
            TurnOutcome::Completed { cost: Some(c), .. } => Some(*c),
            _ => None,
        })
        .next_back()
        .unwrap()
}

#[tokio::test(start_paused = true)]
async fn mixed_endpoints_price_each_agent_on_its_own() {
    let mut run = mixed_tree("limits-mixed", true).await;
    let agents = super::resume::list(&mut run).await;
    let run = run.quit().await;
    let usage: Usage = run
        .seen
        .iter()
        .filter_map(|e| inner(e, "/c1"))
        .filter_map(|e| match e {
            AgentEvent::TurnEnded {
                outcome: TurnOutcome::Completed { usage, .. },
                ..
            } => Some(*usage),
            _ => None,
        })
        .next()
        .unwrap();
    assert_eq!(run.requests("b:/c1")[0].session.model, "claude-sonnet-5-5");
    let answered_as = MODEL;
    let info = crate::pricing::model_info(answered_as).unwrap();
    let expected = crate::pricing::estimate_usd(info, &usage, CacheTtl::OneHour);
    let c1 = super::resume::entry(&agents, "/c1");
    let c2 = super::resume::entry(&agents, "/c2");
    assert!(expected > 0.0);
    assert!(
        (c1.spent_usd - expected).abs() < 1e-12,
        "{} {expected}",
        c1.spent_usd
    );
    assert!(!c1.lower_bound);
    assert!(c1.priced);
    assert_eq!(c2.spent_usd, 0.0);
    assert!(c2.lower_bound);
    assert!(!c2.priced);
    assert_eq!(c1.accounts.keys().collect::<Vec<_>>(), ["b"]);
    assert_eq!(c1.accounts["b"].usd, c1.spent_usd);
    assert_eq!(c2.accounts.keys().collect::<Vec<_>>(), ["c"]);
    assert!(c2.accounts["c"].lower_bound);
    let main = main_spends(&run).pop().unwrap();
    assert_eq!(main.keys().collect::<Vec<_>>(), [ENV]);
    let cost = last_root_cost(&run);
    assert!(
        (cost.agents_usd.unwrap() - (c1.spent_usd + c2.spent_usd)).abs() < 1e-12,
        "{cost:?}"
    );
    assert!(cost.agents_lower_bound);
    let unpriced = [&c1, &c2].iter().any(|e| e.spend().lower_bound);
    assert_eq!(cost.agents_lower_bound, unpriced);
    main_spend_follows_each_cost(&run);
    let alone = mixed_tree("limits-mixed-alone", false).await.quit().await;
    let cost = last_root_cost(&alone);
    assert!(!cost.agents_lower_bound, "{cost:?}");
}

#[tokio::test(start_paused = true)]
async fn metered_root_without_children_omits_unused_plan_splits() {
    let mut run = start(
        "metered-root-no-children",
        Setup::default(),
        vec![("/", vec![priced(done("ok"), 1000, 20)])],
    )
    .await;
    run.say("hello").await;
    let outcome = run.root_ended().await;
    let TurnOutcome::Completed {
        cost: Some(cost), ..
    } = outcome
    else {
        panic!("{outcome:?}")
    };
    assert_eq!(cost.session_plan_usd, None);
    assert_eq!(cost.agents_usd, None);
    assert_eq!(cost.agents_plan_usd, None);
    let value = serde_json::to_value(cost).unwrap();
    assert!(value.get("session_plan_usd").is_none());
    assert!(value.get("agents_plan_usd").is_none());
    run.quit().await;
}

#[tokio::test(start_paused = true)]
async fn voice_calls_are_booked_in_main_spend_and_a_subscription_call_as_subscription_use() {
    use crate::voice::testing::booked;
    use wisp_protocol::{AccountSpend, Billing};
    let rooted = rooted("limits-voice", Setup::default(), Vec::new()).await;
    let calls = rooted.agent.voice_sender();
    let mut run = serve(rooted);
    let api = booked("api", "gpt-live-1", Billing::Metered, 30);
    let talk = booked("talk", "gpt-live-1-codex", Billing::ChatGptPlan, 60);
    let (api_usd, talk_usd) = (api.cost_usd.unwrap(), talk.cost_usd.unwrap());
    for call in [api, talk] {
        calls.send(call).unwrap();
    }
    let total = api_usd + talk_usd;
    let event = run
        .until(
            "both calls' spend",
            |e| matches!(e, AgentEvent::SpendChanged { total: t, .. } if t.usd == total),
        )
        .await;
    let AgentEvent::SpendChanged { total, accounts } = event else {
        unreachable!()
    };
    assert_eq!(total.plan_usd, Some(talk_usd));
    let expected = BTreeMap::from([
        (
            "api".to_owned(),
            AccountSpend::of(api_usd, None, Billing::Metered, false),
        ),
        (
            "talk".to_owned(),
            AccountSpend::of(talk_usd, None, Billing::ChatGptPlan, false),
        ),
    ]);
    assert_eq!(accounts, expected);
    let run = run.quit().await;
    let records = run.store.read(&session_name("/")).unwrap();
    let logged = LoggedSession::read(&records).unwrap();
    assert_eq!(logged.session_usd, total.usd);
    assert_eq!(logged.plan_usd, talk_usd);
    assert_eq!(logged.accounts, expected);
    let lines: Vec<crate::spend::Record> = run
        .store
        .spend_lines()
        .iter()
        .map(|line| serde_json::from_str(line).unwrap())
        .collect();
    assert_eq!(lines.len(), 2);
    for (line, (account, model, billing, usd)) in lines.iter().zip([
        ("api", "gpt-live-1", Billing::Metered, api_usd),
        ("talk", "gpt-live-1-codex", Billing::ChatGptPlan, talk_usd),
    ]) {
        assert_eq!(
            (
                &*line.account,
                &*line.model,
                line.spend.usage.unwrap().billing,
                line.spend.usd
            ),
            (account, model, billing, Some(usd))
        );
    }
}

#[tokio::test(start_paused = true)]
async fn ending_a_queued_agent_is_refused_even_in_the_reply_that_stops_it() {
    use wisp_protocol::NotRunReason;
    use wisp_tool_text::agents::{EndRefusal, LIST_AGENTS, end_refused};
    let gate = Arc::new(Semaphore::new(0));
    let mut run = start(
        "limits-end-refused-queued",
        one_running(),
        vec![
            (
                "/",
                vec![
                    uses(&[
                        spawn_call("s1", "a", "Work A."),
                        spawn_call("s2", "b", "Work B."),
                    ]),
                    gated(
                        uses(&[
                            stop_call("k1", "b", true),
                            call("l1", LIST_AGENTS, json!({})),
                        ]),
                        &gate,
                    ),
                    uses(&[stop_call("k2", "b", false), stop_call("k_same", "b", true)]),
                    done("stopped b"),
                ],
            ),
            ("/a", vec![hanging("busy")]),
            ("/b", vec![hanging("queued")]),
        ],
    )
    .await;
    run.say("go").await;
    run.until("b queued", |e| {
        matches!(
            e,
            AgentEvent::AgentSpawned { agent, .. }
                if agent.path.as_str() == "/b" && agent.state == queued(QueueReason::Concurrency)
        )
    })
    .await;
    gate.add_permits(1);
    run.root_ended().await;
    let root = run.requests("/");
    for pair in root.windows(2) {
        grows_from(&pair[1], &pair[0], "the root");
    }
    assert_eq!(
        result_of(&root[2], "k1"),
        (true, end_refused("/b", None, EndRefusal::Queued))
    );
    assert_eq!(
        result_of(&root[2], "l1"),
        (
            true,
            crate::agent::not_run_text(&NotRunReason::AfterRefusal)
        )
    );
    assert!(!result_of(&root[3], "k2").0);
    assert_eq!(
        result_of(&root[3], "k_same"),
        (true, end_refused("/b", None, EndRefusal::Queued))
    );
    assert!(run.requests("/b").is_empty());
    let run = run.quit().await;
    assert!(super::messages::ended_in(&run, "/").is_empty());
}

#[tokio::test(start_paused = true)]
async fn stop_then_end_of_a_queued_agent_succeeds_even_when_cancel_settles_mid_second_reply() {
    let stop_gate = Arc::new(Semaphore::new(0));
    let end_gate = Arc::new(Semaphore::new(0));
    let at_select = pause_at("/b", "restart-idle-select");
    let mut run = start(
        "limits-queued-settle-mid-reply",
        one_running(),
        vec![
            (
                "/",
                vec![
                    uses(&[
                        spawn_call("s1", "a", "Work A."),
                        spawn_call("s2", "b", "Work B."),
                    ]),
                    gated(uses(&[stop_call("k1", "b", false)]), &stop_gate),
                    gated(uses(&[stop_call("k2", "b", true)]), &end_gate),
                    done("ended b"),
                ],
            ),
            ("/a", vec![hanging("busy")]),
            ("/b", vec![hanging("queued")]),
        ],
    )
    .await;
    run.say("go").await;
    within("/b idle select", at_select.wait()).await;
    stop_gate.add_permits(1);
    while run.requests("/").len() < 3 {
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    within("release /b", at_select.wait()).await;
    run.until("/b stopped", |e| {
        matches!(
            e,
            AgentEvent::AgentStateChanged { agent, state, .. }
                if agent.as_str() == "/b" && *state == stopped()
        )
    })
    .await;
    end_gate.add_permits(1);
    run.root_ended().await;
    let root = run.requests("/");
    for pair in root.windows(2) {
        grows_from(&pair[1], &pair[0], "the root");
    }
    assert!(!result_of(&root[2], "k1").0);
    assert_eq!(result_of(&root[3], "k2"), (false, ended_line("/b", 0)));
    assert!(run.requests("/b").is_empty());
    let run = run.quit().await;
    assert_eq!(super::messages::ended_in(&run, "/"), ["/b"]);
}

#[tokio::test(start_paused = true)]
async fn a_stopped_idle_child_retasked_and_queued_refuses_end() {
    use wisp_tool_text::agents::{EndRefusal, LIST_AGENTS, end_refused};
    let gate = Arc::new(Semaphore::new(0));
    let mut run = start(
        "limits-stopped-idle-retasked-queued",
        one_running(),
        vec![
            (
                "/",
                vec![
                    uses(&[spawn_call("s1", "b", "First.")]),
                    done("waiting for b"),
                    done("got b"),
                    uses(&[spawn_call("s2", "a", "Busy.")]),
                    uses(&[stop_call("k_idle", "b", false)]),
                    uses(&[send_call("m1", "b", "Second.")]),
                    gated(uses(&[call("l1", LIST_AGENTS, json!({}))]), &gate),
                    uses(&[stop_call("k_end", "b", true)]),
                    done("done"),
                ],
            ),
            ("/a", vec![hanging("busy")]),
            (
                "/b",
                vec![
                    uses(&[report_call("r1", "first")]),
                    uses(&[report_call("r2", "second")]),
                ],
            ),
        ],
    )
    .await;
    run.say("first").await;
    run.root_ended().await;
    run.child_ended("/b").await;
    run.root_ended().await;
    run.say("next").await;
    run.until("/b queued for second task", |e| {
        matches!(
            e,
            AgentEvent::AgentStateChanged { agent, state, .. }
                if agent.as_str() == "/b" && *state == queued(QueueReason::Concurrency)
        )
    })
    .await;
    gate.add_permits(1);
    run.root_ended().await;
    let root = run.requests("/");
    for pair in root.windows(2) {
        grows_from(&pair[1], &pair[0], "the root");
    }
    assert!(!result_of(&root[5], "k_idle").0);
    assert!(!result_of(&root[6], "m1").0);
    assert!(!result_of(&root[7], "l1").0);
    assert_eq!(
        result_of(&root[8], "k_end"),
        (true, end_refused("/b", None, EndRefusal::Queued))
    );
    let run = run.quit().await;
    assert!(super::messages::ended_in(&run, "/").is_empty());
}

#[tokio::test(start_paused = true)]
async fn queued_child_refreshes_starting_brief_across_worker_permit_and_restart_with_todos_off() {
    use super::resume::resume;
    let setup = || Setup {
        plugins: vec!["write_todos"],
        limits: Limits {
            max_running: Some(1),
            ..Limits::default()
        },
        ..Setup::default()
    };
    let queued_brief = |canary: &str| SpawnRequest {
        todos: vec![
            format!("one step\n- [ ] same step: {canary}"),
            "safe next step".into(),
        ],
        tools_off: vec!["write_todos".into()],
        ..spawn_request("b", "Give this unique task.")
    };
    for restart in [false, true] {
        let canary = format!("CANARYOpaqueWorkerPermit-{restart}");
        let gate = Arc::new(Semaphore::new(0));
        let worker = |report| vec![uses(&[report_call("r1", report)])];
        let mut run = start(
            "brief-worker-wait",
            setup(),
            vec![
                (
                    "/a",
                    vec![gated(uses(&[report_call("r1", "blocker")]), &gate)],
                ),
                ("/b", worker("worker done")),
                ("/", vec![done("noted"); 4]),
            ],
        )
        .await;
        spawned(&mut run, "a").await;
        sent_once(&run, "/a").await;
        run.spawn(queued_brief(&canary)).await;
        spawn_seen(&mut run, "/b").await;
        run.until("worker queued", |event| matches!(event, AgentEvent::AgentStateChanged { agent, state, .. } if agent.as_str() == "/b" && *state == queued(QueueReason::Concurrency))).await;
        assert!(run.requests("/b").is_empty());
        if restart {
            let old = run.quit().await;
            crate::session::register_secrets([canary.clone()]);
            let blocker = Arc::new(Semaphore::new(0));
            let mut again = resume(
                old,
                setup(),
                vec![
                    (
                        "/a",
                        vec![gated(uses(&[report_call("r1", "blocker")]), &blocker)],
                    ),
                    ("/b", worker("worker done")),
                    ("/", vec![done("noted"); 4]),
                ],
            )
            .await
            .run;
            again.until("revised waiting brief", |event| matches!(inner(event, "/b"), Some(AgentEvent::InputAccepted { text, .. }) if text.contains("[redacted]") && !text.contains(&canary))).await;
            let old = again.quit().await;
            assert!(old.records("/b").iter().any(|event| matches!(event, LogEvent::InputRevised { turn: None, inputs } if !inputs[0].text.contains(&canary))));
            let accepted = old
                .records("/b")
                .into_iter()
                .find(|event| matches!(event, LogEvent::InputAccepted { .. }))
                .unwrap();
            assert!(
                matches!(accepted, LogEvent::InputAccepted { text, .. } if text.contains(&canary))
            );
            crate::session::reset_held_for_test();
            assert!(crate::session::held().is_empty());
            run = resume(
                old,
                setup(),
                vec![
                    ("/a", worker("blocker done")),
                    ("/b", worker("worker done")),
                    ("/", vec![done("noted"); 4]),
                ],
            )
            .await
            .run;
        } else {
            crate::session::register_secrets([canary.clone()]);
            gate.add_permits(1);
        }
        sent_once(&run, "/b").await;
        run.until("worker report", |event| matches!(event, AgentEvent::InputAccepted { text, .. } if text.contains("worker done"))).await;
        let body = run.requests("/b")[0].body().to_string();
        assert!(
            body.contains("Give this unique task.")
                && body.contains("[redacted]")
                && body.contains("safe next step")
        );
        assert!(!body.contains(&canary));
        run.quit().await;
    }
}
