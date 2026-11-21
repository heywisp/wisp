use wisp_protocol::{InputId, MailFrom, MailId, PresetChange, QueueReason};

use super::follow::{api, subscription};
use super::guarantee::{background, idle_as};
use super::*;
use crate::kit::Resume;
use crate::resume::{LoggedSession, recover};
use crate::session::{ResumeLock, reread};
use crate::stack_alarm::{Timer, on_alarm_stack};
use crate::test_guard::server_error_step;

pub(super) struct Back {
    pub(super) run: Run,
    pub(super) warnings: Vec<String>,
    pub(super) changed: Vec<crate::agents::ChangedPreset>,
    pub(super) live: Vec<String>,
    pub(super) ended: Vec<String>,
    pub(super) moved: Vec<wisp_protocol::subscription_auth::Followed>,
    pub(super) stay: Vec<wisp_protocol::subscription_auth::Stayed>,
}

pub(super) async fn resume(old: Run, mut setup: Setup, scripts: Vec<(&str, Vec<Step>)>) -> Back {
    let Run {
        providers,
        store,
        env,
        ..
    } = old;
    for scripted in providers.each.lock().unwrap().values() {
        scripted.clear();
    }
    for (path, steps) in scripts {
        providers.script(path, steps);
    }
    let kit = kit_for(&env, &providers, &store, &setup);
    if let Some(text) = setup.preset_config {
        setup.presets = presets_at(&env, text, &kit_view(&kit));
    }
    let root_id = session_name("/");
    let lines = reread(&store.read(&root_id).unwrap()).unwrap();
    let provider: Arc<dyn Provider> = providers.get("/");
    let next = u64::try_from(lines.len()).unwrap();
    let mut recovered = recover(lines, &store, &|_| Some(provider.clone())).unwrap();
    let (logged, rebuilt) = recovered.take_checked();
    let logged = logged.unwrap();
    let log = SessionLog::continuing(Box::new(store.clone()), &root_id, next);
    let start = Start::Resume(Resume {
        log,
        recovered: &recovered,
        truncated_bytes: 0,
        logged: Some(&logged),
        rebuilt,
    });
    let flag = |name: &str| {
        setup
            .flags
            .iter()
            .find_map(|g| match (&g.target, &g.value) {
                (
                    wisp_protocol::settings::Target::Name(spec),
                    wisp_protocol::settings::SettingValue::Text(text),
                ) if spec.name == name => Some(text.clone()),
                _ => None,
            })
    };
    let flagged = flag("instructions.source").and_then(|t| InstructionsSource::parse(&t));
    let spec = Spec {
        instructions_flag: flagged.clone(),
        extra_flag: flag("instructions.extra").map(|t| crate::instructions::Extra::named(&t)),
        ..root_spec(start)
    };
    let mut instructions = tree_instructions(setup.project_instructions);
    if flagged.is_some() {
        instructions = spec.instructions();
    }
    let Built { agent, .. } = kit.build(spec).await.unwrap();
    let mut tree = TreeSetup {
        presets: setup.presets,
        tools_off: catalog_names(),
        followers: tools_off_followers(),
        limits: setup.limits,
        instructions,
        loaded: LoadedTree::default(),
        keep_warm_main: setup.keep_warm_main,
        restarted: setup.restarted,
        start: setup.start.map(str::to_owned),
        unusable: setup.unusable,
    };
    tree.loaded = load_tree(
        &kit,
        &tree,
        &agent,
        &recovered.records,
        ResumeLock::Required,
        &setup.follow,
    )
    .await;
    let changed = tree.loaded.preset_changes.clone();
    let mut warnings = std::mem::take(&mut tree.loaded.warnings);
    let moved = std::mem::take(&mut tree.loaded.moved);
    let stay = std::mem::take(&mut tree.loaded.stay);
    warnings.extend(changed.iter().map(crate::agents::ChangedPreset::warning));
    let live = tree.loaded.live().iter().map(|p| p.to_string()).collect();
    let ended = tree.loaded.ended().iter().map(|p| p.to_string()).collect();
    let (commands, events, task) = start_root(kit, tree, agent);
    Back {
        run: Run {
            commands: Some(commands),
            events,
            task: Some(task),
            providers,
            store,
            env,
            seen: Vec::new(),
            matched: std::collections::BTreeSet::new(),
            next_request: 0,
        },
        warnings,
        changed,
        live,
        ended,
        moved,
        stay,
    }
}

pub(super) async fn list(run: &mut Run) -> Vec<AgentEntry> {
    let request = run.request();
    run.send(ClientCommand::ListAgents { request }).await;
    let found = run
        .until(
            "the snapshot",
            |e| matches!(e, AgentEvent::AgentsSnapshot { request: Some(r), .. } if *r == request),
        )
        .await;
    let AgentEvent::AgentsSnapshot { agents, .. } = found else {
        unreachable!()
    };
    agents
}

pub(super) fn without_clocks(agents: &[AgentEntry]) -> Vec<AgentEntry> {
    agents
        .iter()
        .cloned()
        .map(|mut a| {
            a.running_ms = None;
            a.since_ms = None;
            a
        })
        .collect()
}

fn counts(run: &Run, paths: &[&str]) -> Vec<usize> {
    paths.iter().map(|p| run.requests(p).len()).collect()
}

pub(super) fn child_send(agent: &str, id: &str, text: &str) -> ClientCommand {
    child(agent, ClientCommand::send(id, text))
}

pub(super) async fn reported(run: &mut Run, agent: &str) {
    let agent = agent.to_owned();
    run.until(
        &format!("{agent}'s report"),
        move |e| matches!(e, AgentEvent::AgentReported { agent: a, .. } if a.as_str() == agent),
    )
    .await;
}

pub(super) async fn idle(run: &mut Run, agent: &str) {
    let agent = agent.to_owned();
    run.until(&format!("{agent} to settle"), move |e| {
        matches!(
            e,
            AgentEvent::AgentStateChanged { agent: a, state: AgentState::Idle { .. }, .. }
                if a.as_str() == agent
        )
    })
    .await;
}

fn mail_ids(run: &Run, agent: &str) -> Vec<u64> {
    run.records(agent)
        .into_iter()
        .filter_map(|e| match e {
            LogEvent::InputAccepted {
                from: Some(from), ..
            } => from.mail.0.strip_prefix('m').and_then(|n| n.parse().ok()),
            _ => None,
        })
        .collect()
}

fn durations(run: &Run, agent: &str) -> u64 {
    run.records(agent)
        .into_iter()
        .filter_map(|e| match e {
            LogEvent::TurnEnded { stats, .. } => Some(stats.duration_ms),
            _ => None,
        })
        .sum()
}

const PATHS: [&str; 4] = ["/", "/a", "/a/g", "/b"];

#[tokio::test(start_paused = true)]
async fn a_tree_resumes_every_live_agent_quietly_and_a_follow_up_extends_its_history() {
    let setup = || Setup {
        plugins: vec!["write_todos"],
        ..Setup::default()
    };
    let mut run = start(
        "resume-tree",
        setup(),
        vec![
            (
                "/",
                vec![done("noted"), done("noted"), done("noted"), done("noted")],
            ),
            (
                "/a",
                vec![
                    uses(&[spawn_call("s1", "g", "Check the parser.")]),
                    done("waiting on g"),
                    uses(&[report_call("r1", "a is done\nwith details")]),
                ],
            ),
            ("/a/g", vec![uses(&[report_call("r1", "g found it")])]),
            (
                "/b",
                vec![
                    uses(&[todos_call("t1", "Read", "Write")]),
                    uses(&[report_call("r1", "b wrote it")]),
                ],
            ),
        ],
    )
    .await;
    run.spawn(spawn_request("a", "Lead the check.")).await;
    run.spawn(SpawnRequest {
        todos: vec!["Read".into(), "Write".into()],
        ..spawn_request("b", "Write it.")
    })
    .await;
    reported(&mut run, "/a/g").await;
    reported(&mut run, "/b").await;
    reported(&mut run, "/a").await;
    tokio::time::sleep(Duration::from_secs(30)).await;
    let before = list(&mut run).await;
    assert_eq!(before.len(), 3, "{before:#?}");
    let highest = PATHS.iter().flat_map(|p| mail_ids(&run, p)).max().unwrap();
    let a_before = run.requests("/a").last().cloned().unwrap();
    let b_before = run.requests("/b").last().cloned().unwrap();
    let run = run.quit().await;
    let sizes: Vec<usize> = PATHS.iter().map(|p| run.records(p).len()).collect();
    let requests = counts(&run, &PATHS);
    let logged = LoggedSession::read(&run.store.read(&session_name("/")).unwrap());
    let logged = logged.unwrap().accounts;
    assert!(logged.contains_key(ENV));

    let back = resume(
        run,
        setup(),
        vec![
            (
                "/",
                vec![
                    uses(&[send_call("m1", "a", "Re-check line 4.")]),
                    done("ok"),
                    done("ok"),
                ],
            ),
            ("/a", vec![uses(&[report_call("r2", "rechecked")])]),
            ("/b", vec![done("Sure.")]),
        ],
    )
    .await;
    assert_eq!(back.live, ["/a", "/b", "/a/g"]);
    assert!(back.ended.is_empty());
    assert!(back.warnings.is_empty(), "{:?}", back.warnings);
    let mut run = back.run;
    tokio::time::sleep(Duration::from_secs(30)).await;
    let first = run
        .until("main's spend", |e| {
            matches!(e, AgentEvent::SpendChanged { .. })
        })
        .await;
    assert!(matches!(&first, AgentEvent::SpendChanged { accounts, .. } if *accounts == logged));
    assert_eq!(counts(&run, &PATHS), requests, "loading sends no request");
    let after = list(&mut run).await;
    assert_eq!(without_clocks(&after), without_clocks(&before));
    for (entry, path) in after.iter().zip(["/a", "/a/g", "/b"]) {
        assert_eq!(entry.running_ms, Some(durations(&run, path)), "{path}");
    }
    let b = after.iter().find(|e| e.path.as_str() == "/b").unwrap();
    assert_eq!(b.checklist.map(|c| (c.completed, c.total)), Some((1, 2)));
    assert_eq!(b.last_report.as_deref(), Some("b wrote it"));
    let a = after.iter().find(|e| e.path.as_str() == "/a").unwrap();
    assert_eq!(a.last_report.as_deref(), Some("a is done"));

    run.say("check again").await;
    reported(&mut run, "/a").await;
    let a_after = run.requests("/a").last().cloned().unwrap();
    assert!(a_after.messages.starts_with(&a_before.messages));
    assert!(a_after.messages.len() > a_before.messages.len() + 1);
    assert_eq!(a_after.prefix(), a_before.prefix());
    assert_eq!(a_after.session, a_before.session);
    assert_eq!(environment_of(&a_after), None);
    assert_eq!(a_after.tools, a_before.tools);
    assert!(
        a_after
            .last_user_texts()
            .join("\n")
            .contains("Re-check line 4.")
    );
    let new_ids = mail_ids(&run, "/a");
    assert!(
        new_ids.iter().any(|id| *id > highest),
        "{new_ids:?} {highest}"
    );

    run.send(child_send("/b", "you-1", "Anything else?")).await;
    run.child_ended("/b").await;
    idle(&mut run, "/b").await;
    tokio::time::sleep(Duration::from_secs(10)).await;
    let b_requests = run.requests("/b");
    assert_eq!(
        b_requests.len(),
        3,
        "your message owes nothing, so no nudge"
    );
    let b_after = b_requests.last().unwrap();
    assert!(b_after.messages.starts_with(&b_before.messages));
    assert_eq!(b_after.prefix(), b_before.prefix());
    let run = run.quit().await;
    let records = run.records("/a/g");
    assert!(
        matches!(
            &records[sizes[2]..],
            [
                LogEvent::SessionResumed { .. },
                LogEvent::HookRan { hook: instructions, point: section, outcome: wisp_protocol::HookOutcome::None, .. },
                LogEvent::HookRan { hook: mcp, point: start, outcome: wisp_protocol::HookOutcome::None, .. },
                LogEvent::SessionEnded { .. },
            ] if instructions == "instructions/instructions" && section == "section-start" && mcp == "mcp/list" && start == "session-start"
        ),
        "an untouched child gains only resume/end and the two no-op startup audits"
    );
}

#[tokio::test(start_paused = true)]
async fn a_child_whose_turn_took_0_ms_shows_it_ran_live_and_after_a_resume() {
    let mut run = start(
        "resume-zero-ms",
        Setup::default(),
        vec![
            ("/", vec![done("noted")]),
            ("/z", vec![uses(&[report_call("r1", "z is done")])]),
        ],
    )
    .await;
    run.spawn(spawn_request("z", "Be quick.")).await;
    reported(&mut run, "/z").await;
    idle(&mut run, "/z").await;
    let live = list(&mut run).await;
    assert_eq!(entry(&live, "/z").running_ms, Some(0));
    let run = run.quit().await;
    run.store.edit(&session_name("/z"), |records| {
        for record in records.iter_mut() {
            if let LogEvent::TurnEnded { stats, .. } = &mut record.event {
                stats.duration_ms = 0;
            }
        }
    });
    assert_eq!(durations(&run, "/z"), 0);

    let back = resume(run, Setup::default(), vec![]).await;
    assert_eq!(back.live, ["/z"]);
    let mut run = back.run;
    let resumed = list(&mut run).await;
    assert_eq!(entry(&resumed, "/z").running_ms, Some(0));
    let _ = run.quit().await;
}

async fn rejected(run: &mut Run, id: &str) -> String {
    let id = InputId(id.to_owned());
    let found = run
        .until(
            "the refusal",
            move |e| matches!(e, AgentEvent::Rejected { id: Some(i), .. } if *i == id),
        )
        .await;
    let AgentEvent::Rejected { reason, .. } = found else {
        unreachable!()
    };
    reason
}

pub(super) fn entry<'a>(agents: &'a [AgentEntry], path: &str) -> &'a AgentEntry {
    agents
        .iter()
        .find(|e| e.path.as_str() == path)
        .unwrap_or_else(|| panic!("no {path} in {agents:#?}"))
}

fn logged_spend(run: &Run, agent: &str) -> f64 {
    let records = run.store.read(&session_name(agent)).unwrap();
    LoggedSession::read(&records).unwrap().session_usd
}

#[tokio::test(start_paused = true)]
async fn ended_agents_stay_ended_unloaded_and_named_and_their_spend_comes_from_their_own_logs() {
    let todos = || Setup {
        plugins: vec!["write_todos"],
        ..Setup::default()
    };
    let mut run = start(
        "resume-ended",
        todos(),
        vec![
            ("/", vec![done("noted"), done("noted"), done("noted")]),
            (
                "/a",
                vec![
                    uses(&[
                        spawn_call("s1", "g", "Look."),
                        spawn_call("s2", "x", "Try."),
                    ]),
                    uses(&[stop_call("k1", "x", true)]),
                    done("waiting on g"),
                ],
            ),
            (
                "/a/g",
                vec![uses(&[todos_call("t1", "map", "look")]), hanging("looking")],
            ),
            ("/a/x", vec![hanging("trying")]),
            ("/b", vec![uses(&[report_call("r1", "b done")])]),
            (
                "/c",
                vec![uses(&[spawn_call("s3", "d", "Dig.")]), done("waiting on d")],
            ),
            ("/c/d", vec![hanging("digging")]),
        ],
    )
    .await;
    run.spawn(spawn_request("a", "Lead.")).await;
    run.spawn(spawn_request("b", "Help.")).await;
    run.spawn(spawn_request("c", "Dig in.")).await;
    reported(&mut run, "/b").await;
    run.until("/a/x to end", |e| {
        matches!(e, AgentEvent::AgentStateChanged { agent, state: AgentState::Ended, .. } if agent.as_str() == "/a/x")
    })
    .await;
    for agent in ["/a/g", "/c/d"] {
        run.until("it to stream", move |e| {
            matches!(inner(e, agent), Some(AgentEvent::TextDelta { .. }))
        })
        .await;
    }
    run.send(ClientCommand::EndAgent {
        agent: path("/a/g"),
    })
    .await;
    run.until("/a/g to end", |e| {
        matches!(e, AgentEvent::AgentStateChanged { agent, state: AgentState::Ended, .. } if agent.as_str() == "/a/g")
    })
    .await;
    run.send(ClientCommand::EndAgent { agent: path("/c") })
        .await;
    run.until("/c/d to end", |e| {
        matches!(e, AgentEvent::AgentStateChanged { agent, state: AgentState::Ended, .. } if agent.as_str() == "/c/d")
    })
    .await;
    tokio::time::sleep(Duration::from_secs(30)).await;
    let before = list(&mut run).await;
    let run = run.quit().await;
    assert!(
        run.records("/")
            .iter()
            .any(|e| matches!(e, LogEvent::AgentEnded { agent, .. } if agent.as_str() == "/a/g"))
    );
    assert!(
        run.records("/a")
            .iter()
            .any(|e| matches!(e, LogEvent::AgentEnded { agent, .. } if agent.as_str() == "/a/x"))
    );
    run.store.edit(&session_name("/"), |records| {
        for record in records.iter_mut() {
            if let LogEvent::AgentEnded { spent_usd, .. } = &mut record.event {
                *spent_usd = 0.0;
            }
        }
    });
    let sizes = [run.records("/a/g").len(), run.records("/a/x").len()];
    let spend_before = run.store.spend_lines();
    assert!(!spend_before.is_empty());

    let back = resume(run, todos(), vec![("/", vec![done("ok")])]).await;
    assert_eq!(
        back.run.store.spend_lines(),
        spend_before,
        "resume writes no charge"
    );
    assert_eq!(back.live, ["/a", "/b"]);
    assert_eq!(back.ended, ["/c", "/a/g", "/a/x", "/c/d"]);
    assert!(back.warnings.is_empty(), "{:?}", back.warnings);
    let mut run = back.run;
    let after = list(&mut run).await;
    assert_eq!(entry(&after, "/a/g").state, AgentState::Ended);
    assert_eq!(entry(&after, "/a/x").state, AgentState::Ended);
    let g = entry(&after, "/a/g");
    assert_eq!(g.spent_usd, logged_spend(&run, "/a/g"));
    assert!(g.spent_usd > entry(&after, "/a/x").spent_usd);
    assert_eq!(g.checklist.map(|c| c.total), Some(2));
    assert_eq!(entry(&after, "/a/x").checklist, None);
    let d = entry(&after, "/c/d");
    assert_eq!(
        (d.state, d.spent_usd),
        (AgentState::Ended, logged_spend(&run, "/c/d"))
    );
    assert_eq!(g.accounts.keys().collect::<Vec<_>>(), [ENV]);
    assert_eq!(g.accounts[ENV].usd, g.spent_usd);
    let unloaded_fill = |agents: &[AgentEntry]| -> Vec<AgentEntry> {
        without_clocks(agents)
            .into_iter()
            .map(|mut a| {
                if a.state == AgentState::Ended {
                    a.context = None;
                }
                a
            })
            .collect()
    };
    assert_eq!(
        unloaded_fill(&after),
        unloaded_fill(&before),
        "every spend and state as before the exit"
    );
    assert!(entry(&after, "/a/g").context.is_none());
    run.send(child_send("/a/g", "you-1", "Still there?")).await;
    assert_eq!(rejected(&mut run, "you-1").await, "/a/g has ended");
    run.say("one more").await;
    run.root_ended().await;
    let total: f64 = after.iter().map(|e| e.spent_usd).sum();
    let cost = run
        .seen
        .iter()
        .rev()
        .find_map(|e| match e {
            AgentEvent::TurnEnded {
                outcome:
                    TurnOutcome::Completed {
                        cost: Some(cost), ..
                    },
                ..
            } => Some(*cost),
            _ => None,
        })
        .unwrap();
    assert!(
        (cost.agents_usd.unwrap() - total).abs() < 1e-9,
        "{cost:?} {total}"
    );
    assert_eq!(cost.agents_lower_bound, after.iter().any(|e| e.lower_bound));
    let run = run.quit().await;
    assert_eq!(
        [run.records("/a/g").len(), run.records("/a/x").len()],
        sizes,
        "an ended agent's log gains no record"
    );

    crash(&run, "/a/g");
    run.store.remove(&session_name("/a/x"));
    let x_end = run
        .records("/a")
        .into_iter()
        .find_map(|e| match e {
            LogEvent::AgentEnded {
                agent, spent_usd, ..
            } if agent.as_str() == "/a/x" => Some(spent_usd),
            _ => None,
        })
        .unwrap();
    let back = resume(run, Setup::default(), Vec::new()).await;
    let mut run = back.run;
    let after = list(&mut run).await;
    let g = entry(&after, "/a/g");
    assert!(g.lower_bound, "an ended log that ends in an open turn");
    let x = entry(&after, "/a/x");
    assert_eq!((x.spent_usd, x.lower_bound), (x_end, true));
    let run = run.quit().await;

    run.store.edit(&session_name("/a"), |records| {
        if let LogEvent::SessionStarted {
            parent: Some(parent),
            ..
        } = &mut records[0].event
        {
            parent.session_id = "someone-else".into();
        }
    });
    let back = resume(run, Setup::default(), Vec::new()).await;
    assert_eq!(back.ended, ["/a", "/c", "/c/d"]);
    let mut run = back.run;
    let after = list(&mut run).await;
    assert!(
        entry(&after, "/a").lower_bound,
        "its children's spend isn't in the total"
    );
    let _ = run.quit().await;
}

pub(super) async fn interrupted_tree(name: &str) -> Run {
    let mut run = start(
        name,
        Setup::default(),
        vec![
            ("/", vec![done("noted")]),
            (
                "/lead",
                vec![
                    uses(&[spawn_call("s1", "w", "Work.")]),
                    done("waiting on w"),
                ],
            ),
            ("/lead/w", vec![hanging("working")]),
            ("/solo", vec![hanging("thinking")]),
        ],
    )
    .await;
    run.spawn(spawn_request("lead", "Lead.")).await;
    run.spawn(spawn_request("solo", "Think.")).await;
    run.until("/lead/w to stream", |e| {
        matches!(inner(e, "/lead/w"), Some(AgentEvent::TextDelta { .. }))
    })
    .await;
    run.until("/solo to stream", |e| {
        matches!(inner(e, "/solo"), Some(AgentEvent::TextDelta { .. }))
    })
    .await;
    idle(&mut run, "/lead").await;
    run.quit().await
}

fn crash(run: &Run, agent: &str) {
    run.store.edit(&session_name(agent), |records| {
        let end = records
            .iter()
            .rposition(|r| matches!(r.event, LogEvent::TurnEnded { .. }))
            .unwrap();
        records.truncate(end);
    });
}

#[tokio::test(start_paused = true)]
async fn a_restart_carries_on_cut_turns_from_self_mail_and_preserves_the_prefix() {
    for (agent, crashed) in [("/", false), ("/", true), ("/c", false), ("/c", true)] {
        let cut = |id| {
            vec![
                uses(&[call(id, "list_agents", json!({}))]),
                hanging("unfinished"),
            ]
        };
        let mut run = start(
            "restart-carry-on",
            Setup {
                prints: Some(Prints::Real),
                ..Setup::default()
            },
            vec![(agent, cut("c1"))],
        )
        .await;
        if agent == "/" {
            run.say("Work.").await;
        } else {
            run.spawn(spawn_request("c", "Work.")).await;
        }
        run.until("the second request", |e| {
            matches!(
                if agent == "/" {
                    Some(e)
                } else {
                    inner(e, agent)
                },
                Some(AgentEvent::TextDelta { .. })
            )
        })
        .await;
        let restarts = if agent == "/" { 2 } else { 1 };
        let mut notice_ids = Vec::new();
        for restart in 1..=restarts {
            let old = run.requests(agent).last().cloned().unwrap();
            run = run.quit().await;
            if crashed {
                crash(&run, agent);
            }
            if agent == "/" && restart == 1 && !crashed {
                sibling_mail(&run, "/", "/worker", "m90", "Placeholder mail.");
            }
            let highest = ["/", agent]
                .iter()
                .flat_map(|path| mail_ids(&run, path))
                .max()
                .unwrap_or(0);
            let continuing = restart < restarts;
            let steps = if continuing {
                cut("c2")
            } else if agent == "/" {
                vec![done("finished")]
            } else {
                vec![uses(&[report_call("r1", "finished")])]
            };
            let before = run.requests(agent).len();
            let mut back = resume(
                run,
                Setup {
                    restarted: true,
                    ..Setup::default()
                },
                vec![(agent, steps)],
            )
            .await;
            back.run
                .until("the carried-on turn", |e| {
                    let event = if agent == "/" {
                        Some(e)
                    } else {
                        inner(e, agent)
                    };
                    if continuing {
                        matches!(event, Some(AgentEvent::TextDelta { .. }))
                    } else {
                        matches!(
                            event,
                            Some(AgentEvent::TurnEnded {
                                outcome: TurnOutcome::Completed { .. },
                                ..
                            })
                        )
                    }
                })
                .await;
            let next = back.run.requests(agent)[before].clone();
            assert_eq!(next.prefix(), old.prefix(), "{agent}");
            let tail = old.messages.len() - 1;
            assert_eq!(next.messages[..tail], old.messages[..tail]);
            assert_eq!(next.messages[tail].role, old.messages[tail].role);
            assert!(
                next.messages[tail]
                    .content
                    .starts_with(&old.messages[tail].content)
            );
            let mut prefix = next.clone();
            prefix.messages.truncate(old.messages.len());
            prefix.messages[tail]
                .content
                .truncate(old.messages[tail].content.len());
            assert_eq!(
                prefix.body(),
                old.body(),
                "{agent}, restart {restart}, crashed {crashed}"
            );
            assert_eq!(prefix.beta(), old.beta());
            assert_eq!(next.all_text().matches(brief::RESTARTED).count(), restart);
            let mail = back
                .run
                .records(agent)
                .into_iter()
                .rev()
                .find_map(|e| match e {
                    LogEvent::InputAccepted {
                        from: Some(from),
                        text,
                        ..
                    } if text.contains(brief::RESTARTED) => Some(from),
                    _ => None,
                })
                .unwrap();
            assert_eq!(mail.agent, path(agent));
            assert!(mail.wake);
            let id = mail
                .mail
                .0
                .strip_prefix('m')
                .unwrap()
                .parse::<u64>()
                .unwrap();
            assert!(id > highest, "{id} <= {highest}");
            assert!(!notice_ids.contains(&id));
            notice_ids.push(id);
            assert_eq!(notices(&back.run, agent).len(), restart);
            let folded = crate::agents::fold::fold(
                &path(agent),
                &back.run.store.read(&session_name(agent)).unwrap(),
                &[],
            );
            assert!(!folded.peers.contains_key(&path(agent)));
            assert!(!back.run.records("/").iter().any(|e| matches!(e, LogEvent::InputAccepted { text, .. } if text.contains("was interrupted when Wisp exited"))));
            run = back.run;
        }
        run.quit().await;
    }
}

#[tokio::test(start_paused = true)]
async fn a_restart_wakes_a_child_with_no_kept_reply_but_not_the_main_agent() {
    for (restarted, crashed) in [(false, false), (false, true), (true, false), (true, true)] {
        let mut run = start(
            "restart-no-reply",
            Setup::default(),
            vec![
                ("/", vec![hanging("unfinished")]),
                ("/c", vec![hanging("unfinished")]),
            ],
        )
        .await;
        run.say("Work.").await;
        run.spawn(spawn_request("c", "Work c.")).await;
        streaming(&mut run, "/c").await;
        assert!(until(|| run.requests("/").len() == 1).await);
        let run = run.quit().await;
        if crashed {
            crash(&run, "/");
            crash(&run, "/c");
        }
        let mut back = resume(
            run,
            Setup {
                restarted,
                ..Setup::default()
            },
            vec![("/c", vec![uses(&[report_call("r1", "finished")])])],
        )
        .await;
        if restarted {
            back.run
                .until("the child's carried-on turn", |e| {
                    matches!(
                        inner(e, "/c"),
                        Some(AgentEvent::TurnEnded {
                            outcome: TurnOutcome::Completed { .. },
                            ..
                        })
                    )
                })
                .await;
        }
        tokio::time::sleep(Duration::from_secs(5)).await;
        assert_eq!(back.run.requests("/c").len(), if restarted { 2 } else { 1 });
        assert_eq!(back.run.requests("/").len(), 1);
        let run = back.run.quit().await;
        assert!(
            !notices(&run, "/")
                .iter()
                .any(|(_, text)| text.contains(brief::RESTARTED))
        );
        assert_eq!(
            notices(&run, "/c")
                .iter()
                .filter(|(_, text)| text.contains(brief::RESTARTED))
                .count(),
            usize::from(restarted)
        );
    }
}

const INTERRUPTED: &str = "/lead/w was interrupted when Wisp exited; message it to continue.";

#[tokio::test(start_paused = true)]
async fn an_interrupted_worker_gives_its_coordinator_one_waking_notice_and_the_root_one_that_does_not_wake()
 {
    for crashed in [false, true] {
        let run = interrupted_tree(&format!("resume-interrupted-{crashed}")).await;
        let ended = run
            .records("/lead/w")
            .into_iter()
            .rev()
            .find_map(|e| match e {
                LogEvent::SessionEnded { interrupted, .. } => Some(interrupted),
                _ => None,
            });
        assert_eq!(ended, Some(Some(1)));
        if crashed {
            crash(&run, "/lead/w");
        }
        let root_requests = run.requests("/").len();
        let back = resume(
            run,
            Setup::default(),
            vec![
                (
                    "/lead",
                    vec![uses(&[report_call("r1", "w was interrupted")])],
                ),
                ("/", vec![done("noted")]),
            ],
        )
        .await;
        let mut run = back.run;
        run.child_ended("/lead").await;
        tokio::time::sleep(Duration::from_secs(30)).await;
        let root = run.requests("/");
        assert_eq!(
            root.len(),
            root_requests + 1,
            "the root wakes only for the lead's report"
        );
        let carried = root.last().unwrap().last_user_texts().join("\n");
        assert!(carried.contains("/solo was interrupted"), "{carried}");
        assert!(carried.contains("w was interrupted"), "{carried}");
        let lead = run.requests("/lead").last().cloned().unwrap();
        assert!(
            lead.last_user_texts().join("\n").contains(INTERRUPTED),
            "{:?}",
            lead.last_user_texts()
        );
        let after = list(&mut run).await;
        let w = entry(&after, "/lead/w");
        let held = if crashed {
            HoldReason::RetryOffer
        } else {
            HoldReason::Stopped
        };
        assert_eq!(
            w.state,
            AgentState::Held { reason: held },
            "stopped, and its returned brief waits held"
        );
        assert!(w.lower_bound, "a stopped request's spend is a lower bound");
        let run = run.quit().await;
        let lead = notices(&run, "/lead");
        assert!(
            matches!(lead.as_slice(), [(true, text)] if text.contains(INTERRUPTED)),
            "{lead:?}"
        );
        let root = notices(&run, "/");
        assert!(
            matches!(root.as_slice(), [(false, text)] if text.contains("/solo was interrupted when Wisp exited; message it to continue.")),
            "{root:?}"
        );

        let back = resume(run, Setup::default(), Vec::new()).await;
        let run = back.run;
        tokio::time::sleep(Duration::from_secs(30)).await;
        let run = run.quit().await;
        assert_eq!(
            notices(&run, "/lead").len(),
            1,
            "a second resume sends none"
        );
        assert_eq!(notices(&run, "/").len(), 1);
    }
}

#[tokio::test(start_paused = true)]
async fn a_log_from_before_the_interrupted_field_resumes_stopped_with_no_notice() {
    let run = interrupted_tree("resume-older").await;
    for agent in ["/lead/w", "/solo"] {
        run.store.edit(&session_name(agent), |records| {
            for record in records.iter_mut() {
                if let LogEvent::SessionEnded { interrupted, .. } = &mut record.event {
                    *interrupted = None;
                }
            }
        });
    }
    let back = resume(run, Setup::default(), Vec::new()).await;
    let mut run = back.run;
    tokio::time::sleep(Duration::from_secs(30)).await;
    let after = list(&mut run).await;
    assert_eq!(
        entry(&after, "/lead/w").state,
        AgentState::Held {
            reason: HoldReason::Stopped
        }
    );
    let run = run.quit().await;
    assert!(notices(&run, "/lead").is_empty());
    assert!(notices(&run, "/").is_empty());
}

pub(super) fn cancel(agent: &str) -> ClientCommand {
    child(agent, ClientCommand::Cancel)
}

pub(super) async fn streaming(run: &mut Run, agent: &str) {
    let agent = agent.to_owned();
    run.until(&format!("{agent} to stream"), move |e| {
        matches!(inner(e, &agent), Some(AgentEvent::TextDelta { .. }))
    })
    .await;
}

async fn held_for(run: &mut Run, agent: &str, reason: HoldReason) {
    let agent = agent.to_owned();
    run.until(&format!("{agent} to hold its wake"), move |e| {
        matches!(inner(e, &agent), Some(AgentEvent::WakeHeld { reason: r, .. }) if *r == reason)
    })
    .await;
}

#[tokio::test(start_paused = true)]
async fn a_stopped_and_a_failed_child_stay_held_after_a_resume_until_mail_from_above() {
    let mut run = start(
        "resume-held",
        Setup::default(),
        vec![
            ("/", vec![done("noted"); 4]),
            ("/s", vec![hanging("busy")]),
            ("/f", vec![server_error_step()]),
            (
                "/p",
                vec![
                    uses(&[
                        send_call("p1", "../s", "Ready?"),
                        send_call("p2", "../f", "Ready?"),
                    ]),
                    uses(&[report_call("r1", "asked")]),
                ],
            ),
        ],
    )
    .await;
    run.spawn(spawn_request("s", "Work s.")).await;
    streaming(&mut run, "/s").await;
    run.send(cancel("/s")).await;
    run.child_ended("/s").await;
    run.spawn(spawn_request("f", "Work f.")).await;
    run.child_ended("/f").await;
    run.spawn(spawn_request("p", "Ask them.")).await;
    reported(&mut run, "/p").await;
    tokio::time::sleep(Duration::from_secs(30)).await;
    let run = run.quit().await;
    let before = counts(&run, &["/s", "/f"]);

    let back = resume(
        run,
        Setup::default(),
        vec![
            (
                "/",
                vec![
                    uses(&[
                        send_call("m1", "s", "Go on."),
                        send_call("m2", "f", "Try again."),
                    ]),
                    done("ok"),
                    done("ok"),
                    done("ok"),
                ],
            ),
            ("/s", vec![uses(&[report_call("r1", "s done")])]),
            ("/f", vec![uses(&[report_call("r1", "f done")])]),
        ],
    )
    .await;
    let mut run = back.run;
    tokio::time::sleep(Duration::from_secs(30)).await;
    assert_eq!(counts(&run, &["/s", "/f"]), before, "neither wakes");
    held_for(&mut run, "/f", HoldReason::RetryOffer).await;
    held_for(&mut run, "/s", HoldReason::Stopped).await;
    run.say("go").await;
    reported(&mut run, "/s").await;
    reported(&mut run, "/f").await;
    let s = run.requests("/s").last().unwrap().all_text();
    assert!(
        s.contains("Work s.") && s.contains("Ready?") && s.contains("Go on."),
        "{s}"
    );
    assert_eq!(s.matches("Ready?").count(), 1, "{s}");
    let f = run.requests("/f").last().unwrap().all_text();
    assert!(f.contains("Ready?") && f.contains("Try again."), "{f}");
    let _ = run.quit().await;
}

#[tokio::test(start_paused = true)]
async fn queued_spawns_queue_again_after_a_resume_and_one_stopped_while_queued_stays_held() {
    let setup = || Setup {
        limits: Limits {
            max_running: Some(1),
            ..Limits::default()
        },
        ..Setup::default()
    };
    let mut run = start(
        "resume-queued",
        setup(),
        vec![
            ("/", vec![done("noted"); 4]),
            ("/q1", vec![hanging("busy")]),
        ],
    )
    .await;
    for name in ["q1", "q2", "q3", "q4"] {
        run.spawn(spawn_request(name, &format!("Work {name}.")))
            .await;
    }
    streaming(&mut run, "/q1").await;
    run.until(
        "q4 to queue",
        |e| matches!(e, AgentEvent::AgentSpawned { agent, .. } if agent.path.as_str() == "/q4"),
    )
    .await;
    run.send(cancel("/q4")).await;
    run.until("q4 to stop", |e| {
        matches!(e, AgentEvent::AgentStateChanged { agent, state: AgentState::Idle { .. }, .. } if agent.as_str() == "/q4")
    })
    .await;
    let before = list(&mut run).await;
    assert_eq!(entry(&before, "/q2").running_ms, None);
    assert_eq!(entry(&before, "/q3").running_ms, None);
    let run = run.quit().await;
    assert!(run.requests("/q2").is_empty() && run.requests("/q3").is_empty());

    let gate = Arc::new(Semaphore::new(0));
    let back = resume(
        run,
        setup(),
        vec![
            (
                "/",
                vec![
                    uses(&[send_call("m1", "q4", "Go.")]),
                    done("ok"),
                    done("ok"),
                    done("ok"),
                    done("ok"),
                ],
            ),
            ("/q2", vec![gated(uses(&[report_call("r1", "q2")]), &gate)]),
            ("/q3", vec![gated(uses(&[report_call("r1", "q3")]), &gate)]),
            ("/q4", vec![uses(&[report_call("r1", "q4")])]),
        ],
    )
    .await;
    let mut run = back.run;
    tokio::time::sleep(Duration::from_secs(10)).await;
    let first = counts(&run, &["/q2", "/q3"]);
    assert_eq!(first.iter().sum::<usize>(), 1, "one runs: {first:?}");
    let waiting = if first[0] == 1 { "/q3" } else { "/q2" };
    let running = if first[0] == 1 { "/q2" } else { "/q3" };
    let after = list(&mut run).await;
    assert_eq!(
        entry(&after, waiting).state,
        AgentState::Queued {
            reason: QueueReason::Concurrency
        }
    );
    assert_eq!(entry(&after, waiting).running_ms, None);
    assert!(run.requests("/q1").len() == 1 && run.requests("/q4").is_empty());
    run.say("go").await;
    run.root_ended().await;
    gate.add_permits(1);
    reported(&mut run, running).await;
    gate.add_permits(1);
    reported(&mut run, waiting).await;
    reported(&mut run, "/q4").await;
    assert!(run.requests("/q4")[0].all_text().contains("Work q4."));
    let _ = run.quit().await;
}

pub(super) fn sibling_mail(run: &Run, to: &str, from: &str, id: &str, text: &str) {
    let mail = MailFrom {
        agent: path(from),
        kind: MailKind::Message,
        mail: MailId(id.to_owned()),
        wake: true,
        plugin: None,
    };
    let rendered = crate::agent::render_mail(&mail.agent, mail.kind, text);
    run.store.edit(&session_name(to), |records| {
        let at = records
            .iter()
            .rposition(|r| matches!(r.event, LogEvent::SessionEnded { .. }))
            .unwrap();
        let mut record = records[at].clone();
        record.event = LogEvent::InputAccepted {
            turn: None,
            id: InputId(format!("mail-{id}")),
            mode: wisp_protocol::DeliveryMode::Steer,
            text: rendered,
            images: Vec::new(),
            replaces: None,
            held: false,
            from: Some(mail),
        };
        records.insert(at, record);
        for (seq, record) in records.iter_mut().enumerate() {
            record.seq = u64::try_from(seq).unwrap();
        }
    });
}

#[tokio::test(start_paused = true)]
async fn a_child_whose_last_turn_completed_wakes_on_waiting_sibling_mail_after_the_settle() {
    let mut run = start(
        "resume-sibling-wake",
        Setup::default(),
        vec![
            ("/", vec![done("noted"); 3]),
            ("/c", vec![uses(&[report_call("r1", "c done")])]),
            ("/d", vec![uses(&[report_call("r1", "d done")])]),
        ],
    )
    .await;
    run.spawn(spawn_request("c", "Work c.")).await;
    run.spawn(spawn_request("d", "Work d.")).await;
    reported(&mut run, "/c").await;
    reported(&mut run, "/d").await;
    tokio::time::sleep(Duration::from_secs(30)).await;
    let run = run.quit().await;
    sibling_mail(&run, "/c", "/d", "m90", "One more thing.");
    sibling_mail(&run, "/d", "/c", "m91", "And you?");
    run.store.edit(&session_name("/d"), |records| {
        for record in records.iter_mut() {
            if let LogEvent::SessionEnded { turns, interrupted } = &mut record.event {
                *interrupted = Some(*turns);
            }
        }
    });
    let back = resume(
        run,
        Setup::default(),
        vec![("/c", vec![done("Noted.")]), ("/d", vec![done("Noted.")])],
    )
    .await;
    let mut run = back.run;
    tokio::time::sleep(Duration::from_millis(1_500)).await;
    assert_eq!(run.requests("/c").len(), 1, "not before the settle");
    run.child_ended("/c").await;
    held_for(&mut run, "/d", HoldReason::Stopped).await;
    assert_eq!(
        run.requests("/d").len(),
        1,
        "a completed turn the exit named still counts as stopped"
    );
    let c = run.requests("/c");
    assert_eq!(c.len(), 2);
    assert!(
        c[1].last_user_texts()
            .join("\n")
            .contains("One more thing.")
    );
    let run = run.quit().await;
    let ids = mail_ids(&run, "/c");
    assert!(ids.contains(&90));
}

async fn named_then_resumed_quietly(crashed: bool, restarted: bool) {
    let mut run = start(
        if crashed {
            "resume-named-crashed"
        } else {
            "resume-named-twice"
        },
        Setup::default(),
        vec![
            ("/", vec![done("noted"); 3]),
            ("/c", vec![uses(&[report_call("r1", "c done")])]),
            ("/d", vec![uses(&[report_call("r1", "d done")])]),
        ],
    )
    .await;
    run.spawn(spawn_request("c", "Work c.")).await;
    run.spawn(spawn_request("d", "Work d.")).await;
    reported(&mut run, "/c").await;
    reported(&mut run, "/d").await;
    tokio::time::sleep(Duration::from_secs(30)).await;
    let run = run.quit().await;
    run.store.edit(&session_name("/d"), |records| {
        for record in records.iter_mut() {
            if let LogEvent::SessionEnded { turns, interrupted } = &mut record.event {
                *interrupted = Some(*turns);
            }
        }
    });
    let back = resume(run, Setup::default(), vec![("/d", vec![done("Noted.")])]).await;
    let run = back.run;
    tokio::time::sleep(Duration::from_secs(30)).await;
    assert_eq!(run.requests("/d").len(), 1);
    let run = run.quit().await;
    sibling_mail(&run, "/d", "/c", "m90", "One more thing.");
    run.store.edit(&session_name("/d"), |records| {
        let last = records.last().map(|r| &r.event);
        assert!(
            matches!(
                last,
                Some(LogEvent::SessionEnded {
                    interrupted: None,
                    ..
                })
            ),
            "{last:?}"
        );
        if crashed {
            records.pop();
        }
    });
    let back = resume(
        run,
        Setup {
            restarted,
            ..Setup::default()
        },
        vec![("/d", vec![done("Noted.")])],
    )
    .await;
    let mut run = back.run;
    held_for(&mut run, "/d", HoldReason::Stopped).await;
    tokio::time::sleep(Duration::from_secs(30)).await;
    assert_eq!(
        run.requests("/d").len(),
        1,
        "a turn an earlier exit named stays stopped through a later resume"
    );
    run.quit().await;
}

#[tokio::test(start_paused = true)]
async fn a_child_an_earlier_exit_stopped_stays_held_after_a_second_resume() {
    for restarted in [false, true] {
        named_then_resumed_quietly(false, restarted).await;
    }
}

#[tokio::test(start_paused = true)]
async fn a_child_an_earlier_exit_stopped_stays_held_after_a_resumed_session_crashed() {
    for restarted in [false, true] {
        named_then_resumed_quietly(true, restarted).await;
    }
}

#[test]
fn a_root_the_exit_stopped_waits_for_you_and_its_mid_stream_mail_arrives_once() {
    on_alarm_stack(Timer::Paused, || async {
        root_stopped_mid_stream("resume-root-stopped", false).await;
    });
}

#[test]
fn a_root_the_exit_stopped_waits_for_you_with_a_section_shown_and_marked_at_every_point() {
    on_alarm_stack(Timer::Paused, || async {
        root_stopped_mid_stream("resume-root-stopped-shown", true).await;
    });
}

async fn root_stopped_mid_stream(name: &str, shown: bool) {
    let mut run = start(
        name,
        Setup {
            hooks: sidebar_hooks(shown),
            ..Setup::default()
        },
        vec![
            ("/", vec![hanging("thinking")]),
            ("/c", vec![uses(&[report_call("r1", "c finished the job")])]),
        ],
    )
    .await;
    if shown {
        run.send(sidebar_shown()).await;
    }
    run.say("think").await;
    run.until("the root to stream", |e| {
        matches!(e, AgentEvent::TextDelta { .. })
    })
    .await;
    run.spawn(spawn_request("c", "Work c.")).await;
    run.until("the report to arrive mid-stream", |e| {
        matches!(e, AgentEvent::InputAccepted { from: Some(_), .. })
    })
    .await;
    let run = run.quit().await;
    let ended = run.records("/").into_iter().rev().find_map(|e| match e {
        LogEvent::SessionEnded { interrupted, .. } => Some(interrupted),
        _ => None,
    });
    assert_eq!(ended, Some(Some(1)));
    let before = run.requests("/").len();
    let setup = Setup {
        hooks: sidebar_hooks(shown),
        ..Setup::default()
    };
    let back = resume(run, setup, vec![("/", vec![done("Got it.")])]).await;
    let mut run = back.run;
    if shown {
        run.send(sidebar_shown()).await;
    }
    tokio::time::sleep(Duration::from_secs(30)).await;
    assert_eq!(run.requests("/").len(), before, "the root waits for you");
    run.say("and now?").await;
    run.root_ended().await;
    let all = run.requests("/").last().unwrap().all_text();
    assert_eq!(all.matches("c finished the job").count(), 1, "{all}");
    let _ = run.quit().await;
}

#[tokio::test(start_paused = true)]
async fn two_siblings_at_the_peer_cap_stay_refused_after_a_resume_until_mail_from_above() {
    let notes: Vec<B> = (0..MAX_PEER_MESSAGES)
        .map(|i| quiet_call(&format!("n{i}"), "../b", "note"))
        .collect();
    let mut run = start(
        "resume-peer-cap",
        Setup::default(),
        vec![
            ("/", vec![done("noted"); 3]),
            ("/b", vec![hanging("busy")]),
            (
                "/a",
                vec![uses(&notes), uses(&[report_call("r1", "noted b")])],
            ),
        ],
    )
    .await;
    run.spawn(spawn_request("b", "Work b.")).await;
    streaming(&mut run, "/b").await;
    run.spawn(spawn_request("a", "Work a.")).await;
    reported(&mut run, "/a").await;
    let run = run.quit().await;
    let back = resume(
        run,
        Setup::default(),
        vec![
            (
                "/a",
                vec![
                    uses(&[quiet_call("again", "../b", "note")]),
                    done("refused"),
                    uses(&[quiet_call("after", "../b", "note")]),
                    done("sent"),
                ],
            ),
            (
                "/",
                vec![
                    uses(&[send_call("m1", "b", "Carry on.")]),
                    done("ok"),
                    done("ok"),
                ],
            ),
            ("/b", vec![uses(&[report_call("r1", "b done")])]),
        ],
    )
    .await;
    let mut run = back.run;
    run.send(child_send("/a", "you-1", "Once more.")).await;
    run.child_ended("/a").await;
    let (is_error, text) = result_of(run.requests("/a").last().unwrap(), "again");
    assert!(is_error, "{text}");
    assert!(text.contains("already has 16 messages from you"), "{text}");
    run.say("tell b").await;
    reported(&mut run, "/b").await;
    run.send(child_send("/a", "you-2", "Now?")).await;
    run.child_ended("/a").await;
    let (is_error, text) = result_of(run.requests("/a").last().unwrap(), "after");
    assert!(!is_error, "mail from above reset the cap: {text}");
    let _ = run.quit().await;
}

#[tokio::test(start_paused = true)]
async fn an_asker_whose_sibling_the_exit_interrupted_is_told_when_its_next_turn_ends_unanswered() {
    let mut run = start(
        "resume-asked",
        Setup::default(),
        vec![
            ("/", vec![done("noted"); 3]),
            ("/b", vec![hanging("busy")]),
            (
                "/a",
                vec![
                    uses(&[send_call("a1", "../b", "Which API?")]),
                    done("waiting for b"),
                ],
            ),
        ],
    )
    .await;
    run.spawn(spawn_request("b", "Work b.")).await;
    streaming(&mut run, "/b").await;
    run.spawn(spawn_request("a", "Work a.")).await;
    run.child_ended("/a").await;
    idle(&mut run, "/a").await;
    let run = run.quit().await;
    let back = resume(
        run,
        Setup::default(),
        vec![
            ("/", vec![done("ok"); 3]),
            (
                "/b",
                vec![done("working"), uses(&[report_call("r1", "b done")])],
            ),
            (
                "/a",
                vec![
                    done("still waiting"),
                    uses(&[report_call("r1", "no answer")]),
                ],
            ),
        ],
    )
    .await;
    let mut run = back.run;
    let listed = list(&mut run).await;
    assert_eq!(
        entry(&listed, "/a").state,
        AgentState::Idle {
            outcome: Some(IdleOutcome::Asked)
        }
    );
    tokio::time::sleep(Duration::from_secs(30)).await;
    assert_eq!(counts(&run, &["/a", "/b"]), [2, 1]);
    run.send(child_send("/b", "you-1", "Carry on.")).await;
    reported(&mut run, "/b").await;
    reported(&mut run, "/a").await;
    let a = run.requests("/a");
    let woken = a[2].last_user_texts().join("\n");
    assert!(
        woken.contains("/b ended its turn without answering your message."),
        "{woken}"
    );
    let b = run.requests("/b")[1].all_text();
    assert!(b.contains("Which API?"), "{b}");
    let _ = run.quit().await;
}

#[tokio::test(start_paused = true)]
async fn a_child_interrupted_mid_task_still_owes_and_reports_on_its_next_turn() {
    let run = interrupted_tree("resume-owes").await;
    let back = resume(
        run,
        Setup::default(),
        vec![
            (
                "/solo",
                vec![done("thinking again"), uses(&[report_call("r1", "solved")])],
            ),
            ("/", vec![done("ok"); 2]),
        ],
    )
    .await;
    let mut run = back.run;
    run.send(child_send("/solo", "you-1", "Carry on.")).await;
    reported(&mut run, "/solo").await;
    let solo = run.requests("/solo");
    assert_eq!(solo.len(), 3, "nudged once, then reported");
    assert!(
        solo[2]
            .last_user_texts()
            .join("\n")
            .contains(crate::agents::NUDGE)
    );
    let _ = run.quit().await;
}

const REVIEWER: &str = "[agents.presets.rev]\ndescription = \"Reviews\"\neffort = \"low\"\ntools_off = [\"show_diff\"]\n";

fn with_presets(text: &str) -> Setup {
    Setup {
        plugins: vec!["show_diff"],
        presets: presets(text),
        ..Setup::default()
    }
}

pub(super) fn environment_of(request: &Sent) -> Option<String> {
    request
        .last_user_texts()
        .into_iter()
        .find(|t| t.starts_with("<environment>"))
}

#[tokio::test(start_paused = true)]
async fn a_child_keeps_its_logged_configuration_when_its_preset_changes_and_new_spawns_use_the_new_one()
 {
    let mut run = start(
        "resume-presets",
        with_presets(&format!(
            "{REVIEWER}fallback = [{{endpoint = 'old-spare'}}]\n"
        )),
        vec![
            ("/", vec![done("hi"), done("noted")]),
            ("/r", vec![uses(&[report_call("r1", "reviewed")])]),
        ],
    )
    .await;
    run.say("hi").await;
    run.root_ended().await;
    run.spawn(SpawnRequest {
        preset: Some("rev".into()),
        ..spawn_request("r", "Review.")
    })
    .await;
    reported(&mut run, "/r").await;
    tokio::time::sleep(Duration::from_secs(30)).await;
    let run = run.quit().await;
    let r_before = run.requests("/r").last().cloned().unwrap();
    let root_before = run.requests("/").last().cloned().unwrap();
    assert_eq!(r_before.session.effort, Effort::Low);
    assert!(!r_before.tool_names().contains(&"show_diff".to_owned()));
    let first_env = environment_of(&run.requests("/")[0]).unwrap();
    assert!(
        first_env.contains("Presets for spawn_agent: rev (Reviews)"),
        "{first_env}"
    );

    let edited = "[agents.presets.rev]\ndescription = \"Reviews twice\"\neffort = \"medium\"\nfallback = [{endpoint = 'new-spare'}]\n";
    let back = resume(
        run,
        with_presets(edited),
        vec![
            ("/r", vec![done("again")]),
            ("/s", vec![uses(&[report_call("r1", "s reviewed")])]),
            ("/", vec![done("ok"), done("ok"), done("ok")]),
        ],
    )
    .await;
    assert_eq!(back.warnings.len(), 1, "{:?}", back.warnings);
    assert!(
        back.warnings[0].starts_with("preset rev has changed; /r keeps the configuration"),
        "{:?}",
        back.warnings
    );
    assert_eq!(
        back.changed,
        [changed_preset("rev", PresetChange::Edited, &["/r"])]
    );
    let mut run = back.run;
    assert_eq!(
        entry(&list(&mut run).await, "/r").preset_change,
        Some(PresetChange::Edited)
    );
    run.send(child_send("/r", "you-1", "Once more.")).await;
    run.child_ended("/r").await;
    let r_after = run.requests("/r").last().cloned().unwrap();
    assert_eq!(r_after.prefix(), r_before.prefix());
    assert_eq!(r_after.tools, r_before.tools);
    assert!(r_after.messages.starts_with(&r_before.messages));
    run.spawn(SpawnRequest {
        preset: Some("rev".into()),
        ..spawn_request("s", "Review again.")
    })
    .await;
    reported(&mut run, "/s").await;
    let agents = list(&mut run).await;
    assert_eq!(
        (
            entry(&agents, "/r").preset_change,
            entry(&agents, "/s").preset_change
        ),
        (Some(PresetChange::Edited), None)
    );
    let s = &run.requests("/s")[0];
    assert_eq!(s.session.effort, Effort::Medium);
    assert!(s.tool_names().contains(&"show_diff".to_owned()));
    for (path, account) in [("/r", "old-spare"), ("/s", "new-spare")] {
        let saved = run
            .records(path)
            .into_iter()
            .find_map(|event| match event {
                LogEvent::SessionStarted {
                    parent: Some(parent),
                    ..
                } => parent.config.preset_values,
                _ => None,
            })
            .unwrap();
        assert_eq!(saved.fallback[0].endpoint, account);
    }
    run.say("status?").await;
    run.root_ended().await;
    let root_after = run
        .requests("/")
        .into_iter()
        .rev()
        .find(|r| r.last_user_texts().iter().any(|t| t == "status?"))
        .unwrap();
    assert_eq!(root_after.tools, root_before.tools);
    assert_eq!(root_after.prefix(), root_before.prefix());
    let env = environment_of(&root_after).expect("the environment is sent again");
    assert!(
        env.contains("Presets for spawn_agent: rev (Reviews twice)"),
        "{env}"
    );
    let run = run.quit().await;

    let back = resume(run, with_presets(""), Vec::new()).await;
    assert_eq!(back.warnings.len(), 1, "{:?}", back.warnings);
    assert!(
        back.warnings[0].starts_with("preset rev was removed; /r, /s keep the configuration"),
        "{:?}",
        back.warnings
    );
    assert_eq!(
        back.changed,
        [changed_preset("rev", PresetChange::Removed, &["/r", "/s"])]
    );
    let mut run = back.run;
    let agents = list(&mut run).await;
    assert_eq!(
        (
            entry(&agents, "/r").preset_change,
            entry(&agents, "/s").preset_change
        ),
        (Some(PresetChange::Removed), Some(PresetChange::Removed))
    );
    let run = run.quit().await;
    let disabled = "[agents.presets.rev]\ndescription = \"Reviews\"\nmodel = \"claude-ghost-1\"\n";
    let back = resume(run, with_presets(disabled), Vec::new()).await;
    assert_eq!(back.warnings.len(), 1, "{:?}", back.warnings);
    assert!(
        back.warnings[0].starts_with("preset rev is disabled;"),
        "{:?}",
        back.warnings
    );
    assert_eq!(
        back.changed,
        [changed_preset("rev", PresetChange::Disabled, &["/r", "/s"])]
    );
    let _ = back.run.quit().await;
}

pub(super) async fn end_agent(run: &mut Run, agent: &str) {
    run.send(ClientCommand::EndAgent { agent: path(agent) })
        .await;
    run.until("the agent to end", |e| {
        matches!(
            e,
            AgentEvent::AgentStateChanged { agent: a, state: AgentState::Ended, .. }
                if a.as_str() == agent
        )
    })
    .await;
}

fn changed_preset(
    name: &str,
    change: PresetChange,
    agents: &[&str],
) -> crate::agents::ChangedPreset {
    crate::agents::ChangedPreset {
        name: name.into(),
        change,
        agents: agents.iter().map(|a| AgentPath::new(*a)).collect(),
    }
}

#[tokio::test(start_paused = true)]
async fn an_ended_agent_is_never_marked_for_a_changed_preset() {
    let mut run = start(
        "resume-preset-ended",
        with_presets(REVIEWER),
        vec![
            ("/", vec![done("noted"); 4]),
            ("/r", vec![uses(&[report_call("r1", "reviewed")])]),
            ("/q", vec![uses(&[report_call("r1", "reviewed")])]),
            ("/x", vec![uses(&[report_call("r1", "reviewed")])]),
        ],
    )
    .await;
    for name in ["r", "q", "x"] {
        run.spawn(SpawnRequest {
            preset: Some("rev".into()),
            ..spawn_request(name, "Review.")
        })
        .await;
        reported(&mut run, &format!("/{name}")).await;
    }
    end_agent(&mut run, "/x").await;
    tokio::time::sleep(Duration::from_secs(30)).await;
    let run = run.quit().await;

    let edited = "[agents.presets.rev]\ndescription = \"Reviews twice\"\neffort = \"medium\"\n";
    let back = resume(run, with_presets(edited), Vec::new()).await;
    assert_eq!(back.ended, ["/x"]);
    assert_eq!(
        back.changed,
        [changed_preset("rev", PresetChange::Edited, &["/q", "/r"])]
    );
    let mut run = back.run;
    end_agent(&mut run, "/q").await;
    let agents = list(&mut run).await;
    assert_eq!(
        (
            entry(&agents, "/r").preset_change,
            entry(&agents, "/q").preset_change,
            entry(&agents, "/x").preset_change
        ),
        (Some(PresetChange::Edited), None, None)
    );
    let _ = run.quit().await;
}

#[tokio::test(start_paused = true)]
async fn a_child_that_cannot_be_loaded_is_ended_for_the_run_with_one_warning_and_the_rest_resumes()
{
    let mut run = start(
        "resume-unloadable",
        Setup::default(),
        vec![
            ("/", vec![done("noted"); 4]),
            ("/gone", vec![uses(&[report_call("r1", "x")])]),
            ("/moved", vec![uses(&[report_call("r1", "x")])]),
            ("/busy", vec![uses(&[report_call("r1", "x")])]),
            ("/fine", vec![uses(&[report_call("r1", "x")])]),
        ],
    )
    .await;
    for name in ["gone", "moved", "busy", "fine"] {
        run.spawn(spawn_request(name, "Work.")).await;
        reported(&mut run, &format!("/{name}")).await;
    }
    tokio::time::sleep(Duration::from_secs(30)).await;
    let run = run.quit().await;
    run.store.remove(&session_name("/gone"));
    run.store.edit(&session_name("/moved"), |records| {
        if let LogEvent::SessionStarted {
            parent: Some(parent),
            ..
        } = &mut records[0].event
        {
            parent.session_id = "someone-else".into();
        }
    });
    let sizes = [run.records("/moved").len(), run.records("/busy").len()];
    let back = resume(
        run,
        Setup {
            busy: vec![session_name("/busy")],
            ..Setup::default()
        },
        Vec::new(),
    )
    .await;
    assert_eq!(back.live, ["/fine"]);
    assert_eq!(back.ended, ["/busy", "/gone", "/moved"]);
    assert_eq!(back.warnings.len(), 3, "{:?}", back.warnings);
    assert!(
        back.warnings[0].starts_with(
            "/busy wasn't resumed: its log can't be opened (session s-busy is open in another Wisp)"
        ),
        "{:?}",
        back.warnings
    );
    assert!(
        back.warnings[1].starts_with("/gone wasn't resumed: its log can't be opened"),
        "{:?}",
        back.warnings
    );
    assert_eq!(
        back.warnings[2],
        "/moved wasn't resumed: its log names another parent; it stays ended for this run"
    );
    let mut run = back.run;
    let after = list(&mut run).await;
    let gone = entry(&after, "/gone");
    assert_eq!(
        (gone.state, gone.spent_usd, gone.lower_bound),
        (AgentState::Ended, 0.0, true)
    );
    assert_eq!(
        entry(&after, "/busy").spent_usd,
        logged_spend(&run, "/busy")
    );
    run.send(child_send("/moved", "you-1", "Hello?")).await;
    assert_eq!(rejected(&mut run, "you-1").await, "/moved has ended");
    let run = run.quit().await;
    assert_eq!(
        [run.records("/moved").len(), run.records("/busy").len()],
        sizes,
        "a log that wasn't loaded gets no record"
    );
}

#[tokio::test(start_paused = true)]
async fn a_crash_leaves_the_childs_spend_and_the_trees_total_a_lower_bound() {
    let mut run = start(
        "resume-crash-cost",
        Setup::default(),
        vec![
            ("/", vec![done("noted"); 3]),
            ("/c", vec![uses(&[report_call("r1", "c done")])]),
        ],
    )
    .await;
    run.spawn(spawn_request("c", "Work.")).await;
    reported(&mut run, "/c").await;
    tokio::time::sleep(Duration::from_secs(30)).await;
    let run = run.quit().await;
    crash(&run, "/c");
    let back = resume(run, Setup::default(), vec![("/", vec![done("ok")])]).await;
    let mut run = back.run;
    let after = list(&mut run).await;
    assert!(entry(&after, "/c").lower_bound);
    assert!(entry(&after, "/c").spent_usd > 0.0);
    run.say("cost?").await;
    let ended = run.root_ended().await;
    let TurnOutcome::Completed {
        cost: Some(cost), ..
    } = ended
    else {
        panic!("{ended:?}")
    };
    assert!(cost.agents_lower_bound, "{cost:?}");
    let _ = run.quit().await;
}

#[tokio::test(start_paused = true)]
async fn a_child_whose_log_names_web_search_resumes_with_it_and_a_vanished_tool_it_never_offered_warns_nothing()
 {
    let setup = || Setup {
        plugins: vec!["web_search"],
        ..Setup::default()
    };
    let mut run = start(
        "resume-catalog",
        setup(),
        vec![
            ("/", vec![done("noted"), done("noted")]),
            ("/c", vec![uses(&[report_call("r1", "searched")])]),
        ],
    )
    .await;
    run.spawn(spawn_request("c", "Search.")).await;
    reported(&mut run, "/c").await;
    tokio::time::sleep(Duration::from_secs(30)).await;
    let run = run.quit().await;
    let before = run.requests("/c").last().cloned().unwrap();
    assert!(before.server_names().is_empty());
    assert!(before.tool_names().contains(&"web_search".to_owned()));
    run.store.edit(&session_name("/c"), |records| {
        if let LogEvent::SessionStarted { tools, .. } = &mut records[0].event {
            tools.push("hammer".into());
        }
    });
    let back = resume(run, setup(), vec![("/c", vec![done("again")])]).await;
    assert_eq!(back.live, ["/c"]);
    assert!(back.warnings.is_empty(), "{:?}", back.warnings);
    let mut run = back.run;
    run.send(child_send("/c", "you-1", "Once more.")).await;
    run.child_ended("/c").await;
    let after = run.requests("/c").last().cloned().unwrap();
    assert_eq!(after.server_names(), before.server_names());
    assert_eq!(after.tools, before.tools);
    assert_eq!(after.prefix(), before.prefix());
    assert!(after.messages.starts_with(&before.messages));
    let _ = run.quit().await;
}

#[tokio::test(start_paused = true)]
async fn a_resumed_tree_keeps_one_shared_child_prefix_and_the_roots_and_every_history_only_grows() {
    let setup = || Setup {
        plugins: vec!["show_diff"],
        ..Setup::default()
    };
    let mut run = start(
        "resume-prefix",
        setup(),
        vec![
            ("/", vec![done("hi"), done("noted"), done("noted")]),
            ("/one", vec![uses(&[report_call("r1", "one")])]),
            ("/two", vec![uses(&[report_call("r2", "two")])]),
        ],
    )
    .await;
    run.say("hello").await;
    run.root_ended().await;
    let other = run.env.dir("other");
    run.spawn(spawn_request("one", "First.")).await;
    run.spawn(SpawnRequest {
        cwd: Some(other.display().to_string()),
        ..spawn_request("two", "Second.")
    })
    .await;
    reported(&mut run, "/one").await;
    reported(&mut run, "/two").await;
    tokio::time::sleep(Duration::from_secs(30)).await;
    let run = run.quit().await;
    let before: Vec<Sent> = ["/", "/one", "/two"]
        .iter()
        .map(|p| run.requests(p).last().cloned().unwrap())
        .collect();

    let back = resume(
        run,
        setup(),
        vec![
            ("/", vec![done("ok"), done("ok"), done("ok")]),
            ("/one", vec![done("again")]),
            ("/two", vec![done("again")]),
        ],
    )
    .await;
    assert!(back.warnings.is_empty(), "{:?}", back.warnings);
    let mut run = back.run;
    run.say("still there?").await;
    run.root_ended().await;
    for (n, child) in ["/one", "/two"].iter().enumerate() {
        run.send(child_send(child, &format!("you-{n}"), "Once more."))
            .await;
        run.child_ended(child).await;
    }
    let after: Vec<Sent> = ["/", "/one", "/two"]
        .iter()
        .map(|p| run.requests(p).last().cloned().unwrap())
        .collect();
    for (old, new) in before.iter().zip(&after) {
        assert_eq!(new.session, old.session);
        assert_eq!(
            environment_of(new),
            None,
            "an unchanged resume sends no new environment"
        );
        assert_eq!(new.prefix(), old.prefix());
        assert_eq!(new.tools, old.tools);
        assert!(new.messages.starts_with(&old.messages));
        assert!(new.messages.len() > old.messages.len());
    }
    assert_eq!(after[1].prefix(), after[2].prefix());
    assert_ne!(after[0].prefix(), after[1].prefix());
    let _ = run.quit().await;
}

#[tokio::test(start_paused = true)]
async fn a_child_moved_to_another_provider_sends_its_history_converted_and_resumes_there() {
    use crate::agent::testing::{B, step};
    use crate::provider::openai::build_body;

    let setup = || Setup {
        endpoints: vec![
            endpoint(ENV),
            openai("gpt", wisp_protocol::Billing::Metered),
        ],
        digests: true,
        ..Setup::default()
    };
    let mut run = start(
        "resume-child-on-another-provider",
        setup(),
        vec![
            ("/", vec![done("noted"); 3]),
            (
                "/w",
                vec![step(
                    &[
                        B::Think("placeholder reasoning"),
                        B::Search("search1", "prefix fixture"),
                        B::Found("search1"),
                        B::Call("r1", REPORT, json!({"text": "An earlier report."})),
                    ],
                    StopReason::ToolUse,
                )],
            ),
            (
                "gpt:/w",
                vec![done("A later reply."), done("Another reply.")],
            ),
        ],
    )
    .await;
    run.spawn(SpawnRequest {
        keep_warm: true,
        ..spawn_request("w", "A brief before the move.")
    })
    .await;
    reported(&mut run, "/w").await;
    run.child_ended("/w").await;
    tokio::time::sleep(Duration::from_secs(5)).await;
    let agents = list(&mut run).await;
    assert_eq!(
        entry(&agents, "/w").keep_warm,
        Some(wisp_protocol::KeepWarm {
            refreshes: 0,
            limit: 8,
            paused: false,
        })
    );
    let request = run.request();
    run.send(child(
        "/w",
        ClientCommand::ChangeSettings {
            request,
            changes: ClientCommand::shortcut_changes(Some("gpt".into()), None, Some(Effort::Low)),
            save: false,
        },
    ))
    .await;
    run.until("the accepted switch", |e| {
        matches!(inner(e, "/w"), Some(AgentEvent::SettingsChanged { answers: Some(r), endpoint, .. }) if *r == request && endpoint == "gpt")
    })
    .await;
    run.send(child_send("/w", "on-gpt", "A message after the move."))
        .await;
    run.until("the turn on OpenAI to start", |e| {
        matches!(
            inner(e, "/w"),
            Some(AgentEvent::TurnStarted { turn: 2, .. })
        )
    })
    .await;
    run.child_ended("/w").await;
    tokio::time::sleep(Duration::from_secs(5)).await;
    let agents = list(&mut run).await;
    assert_eq!(
        entry(&agents, "/w").keep_warm,
        Some(wisp_protocol::KeepWarm {
            refreshes: 0,
            limit: 8,
            paused: true,
        })
    );
    run.send(child_send(
        "/w",
        "again-on-gpt",
        "One more message before reopening.",
    ))
    .await;
    run.child_ended("/w").await;
    let run = run.quit().await;
    let sent = run.requests("gpt:/w");
    let [first, before] = sent.as_slice() else {
        panic!("expected two OpenAI requests")
    };
    let first_body = build_body(&first.request()).unwrap();
    let text = serde_json::to_string(&first_body["input"]).unwrap();
    assert!(text.contains("A brief before the move."), "{text}");
    assert!(text.contains("[anthropic content]"), "{text}");
    assert!(text.contains("Page search1"), "{text}");
    assert!(text.contains("An earlier report."), "{text}");
    assert!(!text.contains("placeholder reasoning"), "{text}");
    assert!(!text.contains("enc-search1"), "{text}");

    let back = resume(run, setup(), vec![("gpt:/w", vec![done("Still here.")])]).await;
    assert_eq!(back.live, ["/w"], "{:?}", back.warnings);
    assert!(back.warnings.is_empty(), "{:?}", back.warnings);
    let mut run = back.run;
    let agents = list(&mut run).await;
    let worker = entry(&agents, "/w");
    assert_eq!(worker.endpoint.as_deref(), Some("gpt"));
    assert_eq!(worker.provider.as_deref(), Some("openai"));
    assert_eq!((worker.model.as_str(), worker.effort), (GPT, Effort::Low));
    run.send(child_send("/w", "reopened", "A message after the reopen."))
        .await;
    run.child_ended("/w").await;
    let sent = run.requests("gpt:/w");
    let after = sent.last().unwrap();
    grows_from(after, before, "the reopened child's history");
    let body = build_body(&before.request()).unwrap();
    let input = body["input"].as_array().unwrap();
    let resumed = build_body(&after.request()).unwrap();
    let extended = resumed["input"].as_array().unwrap();
    assert!(extended.len() > input.len());
    assert_eq!(
        serde_json::to_vec(&extended[..input.len()]).unwrap(),
        serde_json::to_vec(input).unwrap(),
    );
    let _ = run.quit().await;
}

#[tokio::test(start_paused = true)]
async fn a_failed_child_queued_for_a_task_from_above_at_the_exit_runs_it_after_a_resume() {
    let setup = || Setup {
        limits: Limits {
            max_running: Some(1),
            ..Limits::default()
        },
        ..Setup::default()
    };
    let mut run = start(
        "resume-queued-task",
        setup(),
        vec![
            ("/", vec![done("noted"); 3]),
            ("/f", vec![server_error_step()]),
            ("/h", vec![hanging("busy")]),
        ],
    )
    .await;
    run.spawn(spawn_request("f", "Work f.")).await;
    run.child_ended("/f").await;
    tokio::time::sleep(Duration::from_secs(30)).await;
    run.spawn(spawn_request("h", "Hold the permit.")).await;
    streaming(&mut run, "/h").await;
    run.providers.get("/").clear();
    run.providers.script(
        "/",
        vec![uses(&[send_call("m1", "f", "Try again.")]), done("sent")],
    );
    run.say("retry f").await;
    run.until("f to queue for its task", |e| {
        matches!(
            e,
            AgentEvent::AgentStateChanged { agent, state: AgentState::Queued { reason: QueueReason::Concurrency }, .. }
                if agent.as_str() == "/f"
        )
    })
    .await;
    let run = run.quit().await;
    assert_eq!(run.requests("/f").len(), 1);

    let back = resume(
        run,
        setup(),
        vec![
            ("/f", vec![uses(&[report_call("r1", "f done")])]),
            ("/", vec![done("ok"); 3]),
        ],
    )
    .await;
    let mut run = back.run;
    reported(&mut run, "/f").await;
    let f = run.requests("/f");
    assert_eq!(f.len(), 2);
    assert!(f[1].all_text().contains("Try again."));
    assert!(
        run.requests("/h").len() == 1,
        "the interrupted one stays held"
    );
    let _ = run.quit().await;
}

#[tokio::test(start_paused = true)]
async fn a_resumed_child_moves_by_the_choices_in_its_spawn_record() {
    use super::guarantee::{failing, hold_limit};
    use crate::plan_usage::PlanUsageCache;
    let now = 1_800_000_000;
    let clock = Arc::new(ShiftedClock::at(Timestamp::from_second(now).unwrap(), None));
    let setup = |cache: &PlanUsageCache| Setup {
        endpoints: ["first", "second", "third"].map(subscription).into(),
        clock: Some(clock.clone()),
        plan_usage: cache.clone(),
        preset_config: Some(
            "[agents.presets.worker]\ndescription = 'Works'\nendpoint = 'first'\nmodel = 'claude-opus-5-5'\neffort = 'high'\nfallback = [{endpoint = 'first', model = 'claude-opus-5-5'}, {endpoint = 'second', model = 'claude-opus-5-5', effort = 'high'}, {endpoint = 'third'}]\n",
        ),
        ..Setup::default()
    };
    let worker = |name| SpawnRequest {
        preset: Some("worker".into()),
        ..spawn_request(name, "Work.")
    };
    let step_of = |agent: &'static str| {
        move |e: &AgentEvent| matches!(inner(e, agent), Some(AgentEvent::StepStarted { .. }))
    };
    let gate = Arc::new(Semaphore::new(0));
    let cache = PlanUsageCache::default();
    let mut run = start(
        "resume-moved",
        setup(&cache),
        vec![
            ("first:/a", failing(&gate, now, now + 3600)),
            ("third:/a", vec![uses(&[report_call("r1", "a done")])]),
            ("third:/b", vec![uses(&[report_call("r2", "b done")])]),
            ("first:/", vec![done("noted"); 4]),
        ],
    )
    .await;
    run.spawn(worker("a")).await;
    run.until("a's step", step_of("/a")).await;
    for name in ["first", "second"] {
        hold_limit(&run, &cache, name, (now, now + 7200));
    }
    gate.add_permits(1);
    reported(&mut run, "/a").await;
    run.spawn(worker("b")).await;
    reported(&mut run, "/b").await;
    let run = run.quit().await;
    assert!(run.requests("second:/a").is_empty());

    let gate = Arc::new(Semaphore::new(0));
    let cache = PlanUsageCache::default();
    let back = resume(
        run,
        setup(&cache),
        vec![
            (
                "first:/",
                vec![
                    uses(&[send_call("m1", "b", "Again.")]),
                    done("sent"),
                    done("noted"),
                ],
            ),
            ("third:/b", failing(&gate, now, now + 3600)),
            ("second:/b", vec![uses(&[report_call("r3", "b again")])]),
        ],
    )
    .await;
    let mut run = back.run;
    let agents = list(&mut run).await;
    for path in ["/a", "/b"] {
        assert_eq!(entry(&agents, path).endpoint.as_deref(), Some("third"));
    }
    hold_limit(&run, &cache, "first", (now, now + 7200));
    run.say("Ask b again.").await;
    run.until("b's step", step_of("/b")).await;
    hold_limit(&run, &cache, "third", (now, now + 3600));
    gate.add_permits(1);
    reported(&mut run, "/b").await;
    let run = run.quit().await;
    let failed = run.requests("third:/b");
    let retried = run.requests("second:/b");
    assert_eq!(retried.len(), 1);
    assert_eq!(
        format!("{:?}", retried[0].messages),
        format!("{:?}", failed.last().unwrap().messages)
    );
    assert!(run.requests("first:/b").is_empty());
    let mail = run.mail_to("/");
    let moved: Vec<&str> = mail
        .iter()
        .map(|(_, _, text)| wisp_protocol::mail_body(text))
        .filter(|text| text.starts_with("moved to "))
        .collect();
    assert_eq!(moved.len(), 2, "{mail:?}");
    assert!(
        moved[0].starts_with("moved to third with claude-opus-5-5 "),
        "{}",
        moved[0]
    );
    assert!(
        moved[0].ends_with(": first reached its usage limit, resets in 2h00m."),
        "{}",
        moved[0]
    );
    assert!(
        moved[1].starts_with("moved to second with claude-opus-5-5 high "),
        "{}",
        moved[1]
    );
    assert!(
        moved[1].ends_with(": third reached its usage limit, resets in 1h00m."),
        "{}",
        moved[1]
    );
}

#[tokio::test(start_paused = true)]
async fn a_move_dropped_at_exit_is_an_interrupted_turn_on_resume() {
    use super::guarantee::{MOVING, failing, hold_limit};
    use crate::plan_usage::PlanUsageCache;
    let now = 1_800_000_000;
    let clock = Arc::new(ShiftedClock::at(Timestamp::from_second(now).unwrap(), None));
    let setup = |cache: &PlanUsageCache, restarted| Setup {
        endpoints: vec![subscription("first"), subscription("second")],
        clock: Some(clock.clone()),
        plan_usage: cache.clone(),
        preset_config: Some(MOVING),
        restarted,
        ..Setup::default()
    };
    for restarted in [false, true] {
        let gate = Arc::new(Semaphore::new(0));
        let cache = PlanUsageCache::default();
        let mut run = start(
            "resume-move-dropped",
            setup(&cache, false),
            vec![
                ("first:/worker", failing(&gate, now, now + 3600)),
                ("first:/", vec![done("noted")]),
            ],
        )
        .await;
        run.spawn(SpawnRequest {
            preset: Some("worker".into()),
            ..spawn_request("worker", "Work.")
        })
        .await;
        run.until("the child's step", |e| {
            matches!(inner(e, "/worker"), Some(AgentEvent::StepStarted { .. }))
        })
        .await;
        run.send(ClientCommand::GetReady { on: true }).await;
        hold_limit(&run, &cache, "first", (now, now + 3600));
        gate.add_permits(1);
        run.until("the move's switch", |e| {
            matches!(
                inner(e, "/worker"),
                Some(AgentEvent::SettingsChanged { .. })
            )
        })
        .await;
        let run = run.quit().await;
        assert!(run.requests("second:/worker").is_empty());
        assert_eq!(run.mail_to("/"), []);

        assert_dropped_retry_resumes(run, setup(&PlanUsageCache::default(), restarted)).await;
    }
}

#[tokio::test(start_paused = true)]
async fn a_manual_child_retry_dropped_at_exit_is_an_interrupted_turn_on_resume() {
    use super::endpoints::child_retry;
    use super::restart::restart;
    for restarted in [false, true] {
        let setup = |restarted| Setup {
            endpoints: vec![api("first"), api("second")],
            restarted,
            ..Setup::default()
        };
        let mut run = start(
            "resume-manual-retry-dropped",
            setup(false),
            vec![
                ("first:/worker", vec![refused_request()]),
                ("first:/", vec![done("noted")]),
            ],
        )
        .await;
        run.spawn(spawn_request("worker", "Work.")).await;
        let failed = run.child_ended("/worker").await;
        assert!(matches!(failed, TurnOutcome::Error { offer: Some(o), .. } if o.switch));
        run.send(ClientCommand::GetReady { on: true }).await;
        restart(&mut run, wisp_protocol::RestartState::Ready).await;
        let request = child_retry(&mut run, "/worker", "second").await;
        run.until("the manual Retry's switch", |e| {
            matches!(
                inner(e, "/worker"),
                Some(AgentEvent::SettingsChanged { .. })
            )
        })
        .await;
        let run = run.quit().await;
        assert!(run.seen.iter().any(|e| matches!(
            inner(e, "/worker"),
            Some(AgentEvent::Rejected { reason, request: Some(r), .. })
                if *r == request && reason == "the session ended"
        )));
        assert!(run.requests("second:/worker").is_empty());
        assert!(matches!(
            run.records("/worker").last(),
            Some(LogEvent::SessionEnded {
                turns: 1,
                interrupted: Some(1)
            })
        ));
        assert_dropped_retry_resumes(run, setup(restarted)).await;
    }
}

async fn assert_dropped_retry_resumes(run: Run, setup: Setup) {
    let restarted = setup.restarted;
    let prior_mail = run.mail_to("/").len();
    let records = run.store.read(&session_name("/worker")).unwrap();
    let folded = crate::agents::fold::fold(&path("/worker"), &records, &[]);
    assert_eq!(folded.outcome, Some(IdleOutcome::Stopped));
    let root = if restarted {
        vec![done("noted")]
    } else {
        vec![
            uses(&[send_call("m1", "worker", "Again.")]),
            done("sent"),
            done("noted"),
        ]
    };
    let back = resume(
        run,
        setup,
        vec![
            ("second:/worker", vec![uses(&[report_call("r1", "done")])]),
            ("first:/", root),
        ],
    )
    .await;
    let mut run = back.run;
    if !restarted {
        let agents = list(&mut run).await;
        assert_eq!(
            entry(&agents, "/worker").state,
            AgentState::Held {
                reason: HoldReason::RetryOffer,
            }
        );
        assert!(run.requests("second:/worker").is_empty());
        run.say("Ask the worker again.").await;
    }
    reported(&mut run, "/worker").await;
    let run = run.quit().await;
    let retried = run.requests("second:/worker");
    assert_eq!(retried.len(), 1, "{restarted}");
    assert_eq!(retried[0].all_text().contains(brief::RESTARTED), restarted);
    let notices: Vec<String> = run
        .mail_to("/")
        .into_iter()
        .skip(prior_mail)
        .filter(|(kind, ..)| *kind == MailKind::Notice)
        .map(|(_, wake, text)| format!("{wake} {}", wisp_protocol::mail_body(&text)))
        .collect();
    let told = format!("false {}", brief::interrupted_text(&path("/worker")));
    let expected = if restarted { vec![] } else { vec![told] };
    assert_eq!(notices, expected);
}

#[tokio::test(start_paused = true)]
async fn a_spawn_that_overrode_its_preset_gives_no_warning_until_the_preset_itself_changes() {
    let mut run = start(
        "resume-override",
        with_presets(REVIEWER),
        vec![
            ("/", vec![done("noted"); 2]),
            ("/r", vec![uses(&[report_call("r1", "reviewed")])]),
        ],
    )
    .await;
    run.spawn(SpawnRequest {
        preset: Some("rev".into()),
        effort: Some(Effort::Medium),
        ..spawn_request("r", "Review.")
    })
    .await;
    reported(&mut run, "/r").await;
    tokio::time::sleep(Duration::from_secs(30)).await;
    let run = run.quit().await;
    assert_eq!(run.requests("/r")[0].session.effort, Effort::Medium);

    let back = resume(run, with_presets(REVIEWER), Vec::new()).await;
    assert!(back.warnings.is_empty(), "{:?}", back.warnings);
    let run = back.run.quit().await;
    let described = REVIEWER.replace("\"Reviews\"", "\"Reviews code\"");
    let back = resume(run, with_presets(&described), Vec::new()).await;
    assert!(back.warnings.is_empty(), "{:?}", back.warnings);
    let run = back.run.quit().await;
    let kernels = Kernels::default();
    let marked = Setup {
        kernels: kernels.clone(),
        ..with_presets(&format!("{REVIEWER}worker = false\n"))
    };
    let back = resume(run, marked, Vec::new()).await;
    assert!(back.warnings.is_empty(), "{:?}", back.warnings);
    assert!(!service_of(&kernels).tree().entries[&path("/r")].worker);
    let run = back.run.quit().await;
    let edited = REVIEWER.replace("\"low\"", "\"high\"");
    let back = resume(run, with_presets(&edited), Vec::new()).await;
    assert_eq!(back.warnings.len(), 1, "{:?}", back.warnings);
    assert!(back.warnings[0].starts_with("preset rev has changed; /r keeps"));
    let _ = back.run.quit().await;
}

#[tokio::test(start_paused = true)]
async fn a_rewind_after_a_resume_to_a_turn_from_before_the_exit_sends_the_new_presets_line() {
    let mut run = start(
        "resume-rewind",
        with_presets(REVIEWER),
        vec![
            ("/", vec![done("hi"), done("noted")]),
            ("/r", vec![uses(&[report_call("r1", "reviewed")])]),
        ],
    )
    .await;
    run.say("hi").await;
    run.root_ended().await;
    run.spawn(SpawnRequest {
        preset: Some("rev".into()),
        ..spawn_request("r", "Review.")
    })
    .await;
    reported(&mut run, "/r").await;
    tokio::time::sleep(Duration::from_secs(30)).await;
    let run = run.quit().await;
    let turns = run
        .records("/")
        .into_iter()
        .filter(|e| matches!(e, LogEvent::TurnStarted { .. }))
        .count();
    assert_eq!(turns, 2);

    let edited = "[agents.presets.rev]\ndescription = \"Reviews twice\"\n";
    let back = resume(
        run,
        with_presets(edited),
        vec![("/", vec![done("ok"), done("ok")])],
    )
    .await;
    let mut run = back.run;
    let request = run.request();
    run.send(ClientCommand::Rewind {
        to: wisp_protocol::RewindPoint::Turn { turn: 2 },
        request,
    })
    .await;
    run.until("the rewind", |e| matches!(e, AgentEvent::Rewound { .. }))
        .await;
    run.say("again").await;
    run.root_ended().await;
    let last = run.requests("/").last().cloned().unwrap();
    let env = environment_of(&last).expect("the environment is sent again");
    assert!(
        env.contains("Presets for spawn_agent: rev (Reviews twice)"),
        "{env}"
    );
    let _ = run.quit().await;
}

#[tokio::test(start_paused = true)]
async fn a_tree_resumed_with_other_tools_gives_them_to_the_root_and_its_child_keeps_its_own() {
    let mut run = start(
        "resume-other-tools",
        Setup::default(),
        vec![
            ("/", vec![done("hi"), done("ok")]),
            ("/one", vec![uses(&[report_call("r1", "one")])]),
        ],
    )
    .await;
    run.say("hello").await;
    run.root_ended().await;
    run.spawn(spawn_request("one", "First.")).await;
    reported(&mut run, "/one").await;
    tokio::time::sleep(Duration::from_secs(30)).await;
    let run = run.quit().await;
    let root_before = run.requests("/").last().cloned().unwrap();
    let child_before = run.requests("/one").last().cloned().unwrap();
    let names = |r: &Sent| r.tools.iter().map(|t| t.name.clone()).collect::<Vec<_>>();
    assert!(!names(&root_before).contains(&"show_diff".to_owned()));

    let later = Setup {
        plugins: vec!["show_diff"],
        ..Setup::default()
    };
    let back = resume(
        run,
        later,
        vec![("/", vec![done("ok")]), ("/one", vec![done("again")])],
    )
    .await;
    let mut run = back.run;
    run.say("still there?").await;
    run.root_ended().await;
    run.send(child_send("/one", "you-1", "Once more.")).await;
    run.child_ended("/one").await;
    let root_after = run.requests("/").last().cloned().unwrap();
    let child_after = run.requests("/one").last().cloned().unwrap();
    assert!(
        names(&root_after).contains(&"show_diff".to_owned()),
        "{:?}",
        names(&root_after)
    );
    assert_eq!(child_after.tools, child_before.tools);
    assert_eq!(child_after.prefix(), child_before.prefix());
    assert!(child_after.messages.starts_with(&child_before.messages));
    let _ = run.quit().await;
}

fn root_flags() -> Vec<crate::settings::Given> {
    use wisp_protocol::settings::{SettingValue, Target, find};
    [
        ("--model", "model", "claude-sonnet-5-5"),
        ("--effort", "effort", "low"),
        ("--cache-ttl", "cache_ttl", "5m"),
        ("--instructions", "instructions.source", "none"),
        ("--instructions-extra", "instructions.extra", "autonomous"),
    ]
    .into_iter()
    .map(|(origin, name, value)| crate::settings::Given {
        target: Target::Name(find(name).unwrap()),
        value: SettingValue::Text(value.into()),
        origin: origin.into(),
    })
    .collect()
}

async fn resumed_child(name: &str, flags: Vec<crate::settings::Given>) -> (Sent, Sent) {
    let setup = || Setup {
        project_instructions: true,
        endpoints: vec![FakeEndpoint {
            table: Table::Only(&[]),
            ..endpoint(ENV)
        }],
        ..Setup::default()
    };
    let mut run = start(
        name,
        setup(),
        vec![
            ("/", vec![done("hi")]),
            ("/one", vec![uses(&[report_call("r1", "one")])]),
        ],
    )
    .await;
    fs::write(run.env.dir("work").join("AGENTS.md"), "Project rules.\n").unwrap();
    run.say("hello").await;
    run.root_ended().await;
    run.spawn(spawn_request("one", "First.")).await;
    reported(&mut run, "/one").await;
    tokio::time::sleep(Duration::from_secs(30)).await;
    let run = run.quit().await;
    let back = resume(
        run,
        Setup { flags, ..setup() },
        vec![
            ("/", vec![done("noted"), done("ok")]),
            ("/one", vec![done("again")]),
        ],
    )
    .await;
    let mut run = back.run;
    run.say("still there?").await;
    run.root_ended().await;
    run.send(child_send("/one", "you-1", "Once more.")).await;
    run.child_ended("/one").await;
    let root = run.requests("/").last().cloned().unwrap();
    let child = run.requests("/one").last().cloned().unwrap();
    let _ = run.quit().await;
    (root, child)
}

#[tokio::test(start_paused = true)]
async fn a_tree_resumed_with_flags_gives_them_to_the_root_and_never_to_its_children() {
    let (plain_root, plain) = resumed_child("resume-flags-plain", Vec::new()).await;
    let (root, child) = resumed_child("resume-flags", root_flags()).await;
    assert_eq!(root.session.model, "claude-sonnet-5-5");
    assert_eq!(root.session.effort, Effort::Low);
    assert_eq!(root.session.cache_ttl, CacheTtl::FiveMinutes);
    let autonomous = crate::instructions::Extra::Autonomous.text();
    assert!(
        root.session
            .instructions
            .as_deref()
            .is_some_and(|b| b.contains(autonomous))
    );
    assert_ne!(root.session, plain_root.session);
    assert_eq!(child.session, plain.session);
    assert_eq!(child.prefix(), plain.prefix());
    assert_eq!(child.tools, plain.tools);
    assert_eq!(child.messages, plain.messages);
    assert_eq!(child.session.model, MODEL);
}

fn hook_setup(calls: &wisp_plugin_testkit::hooks::Calls) -> Setup {
    use wisp_plugin_testkit::hooks as kit;
    let body = "\n[[hooks]]\nname = \"start\"\npoint = \"session-start\"\n\n[[hooks]]\nname = \"turn\"\npoint = \"before-turn\"\n";
    let make: crate::plugin::Make = kit::instantiate(kit::none(), calls.clone());
    Setup {
        hooks: vec![(kit::manifest("hooked", body), make)],
        ..Setup::default()
    }
}

#[tokio::test(start_paused = true)]
async fn a_child_resumed_with_its_tree_runs_hooks_on_its_own_instance() {
    let first = wisp_plugin_testkit::hooks::Calls::default();
    let mut run = start(
        "resume-hooks",
        hook_setup(&first),
        vec![
            ("/", vec![done("noted"), done("noted")]),
            ("/a", vec![uses(&[report_call("r1", "a did it")])]),
        ],
    )
    .await;
    run.spawn(spawn_request("a", "Do it.")).await;
    reported(&mut run, "/a").await;
    tokio::time::sleep(Duration::from_secs(30)).await;
    let run = run.quit().await;
    let resumed = wisp_plugin_testkit::hooks::Calls::default();
    let back = resume(
        run,
        hook_setup(&resumed),
        vec![
            (
                "/",
                vec![
                    uses(&[send_call("m1", "a", "Do it again.")]),
                    done("ok"),
                    done("ok"),
                ],
            ),
            ("/a", vec![uses(&[report_call("r2", "a did it again")])]),
        ],
    )
    .await;
    assert_eq!(back.live, ["/a"]);
    let mut run = back.run;
    run.say("again").await;
    reported(&mut run, "/a").await;
    let run = run.quit().await;
    let starts = resumed.of("start");
    let started: std::collections::BTreeSet<usize> = starts.iter().map(|c| c.instance).collect();
    assert_eq!(starts.len(), 2, "{starts:?}");
    assert_eq!(started.len(), 2, "{starts:?}");
    let turned: std::collections::BTreeSet<usize> =
        resumed.of("turn").iter().map(|c| c.instance).collect();
    assert_eq!(turned, started);
    let child_starts = run
        .records("/a")
        .into_iter()
        .filter(|e| matches!(e, LogEvent::HookRan { hook, point, .. } if hook == "hooked/start" && point == "session-start"))
        .count();
    assert_eq!(child_starts, 2);
}

#[tokio::test(start_paused = true)]
async fn a_child_logged_before_the_tools_were_grouped_resumes_with_exactly_its_logged_list() {
    let setup = || Setup {
        plugins: vec!["show_diff"],
        without: vec!["list_agents", "send_message", "stop_agent"],
        ..Setup::default()
    };
    let mut run = start(
        "resume-pre-grouping",
        setup(),
        vec![
            ("/", vec![done("noted"), done("noted")]),
            ("/c", vec![uses(&[report_call("r1", "done")])]),
        ],
    )
    .await;
    run.spawn(spawn_request("c", "Work.")).await;
    reported(&mut run, "/c").await;
    tokio::time::sleep(Duration::from_secs(30)).await;
    let run = run.quit().await;
    let before = run.requests("/c").last().cloned().unwrap();
    assert_eq!(before.tool_names(), ["report", "show_diff", "spawn_agent"]);
    let back = resume(run, setup(), vec![("/c", vec![done("again")])]).await;
    assert_eq!(back.live, ["/c"]);
    assert!(back.warnings.is_empty(), "{:?}", back.warnings);
    let mut run = back.run;
    run.send(child_send("/c", "you-1", "Once more.")).await;
    run.child_ended("/c").await;
    let after = run.requests("/c").last().cloned().unwrap();
    assert_eq!(after.tool_names(), before.tool_names());
    assert_eq!(after.prefix(), before.prefix());
    assert_eq!(environment_of(&after), None);
    assert!(after.messages.starts_with(&before.messages));
    let _ = run.quit().await;
}

#[tokio::test(start_paused = true)]
async fn a_root_started_without_the_agent_tools_gets_them_on_resume_and_sends_its_presets_line_once()
 {
    let old = Setup {
        agents: false,
        ..with_presets(REVIEWER)
    };
    let mut run = start(
        "resume-old-default",
        old,
        vec![("/", vec![done("hi"), done("again")])],
    )
    .await;
    run.say("hello").await;
    run.root_ended().await;
    run.say("more").await;
    run.root_ended().await;
    let run = run.quit().await;
    let before = run.requests("/").last().cloned().unwrap();
    assert!(!before.tool_names().contains(&"spawn_agent".to_owned()));
    let opening = before.first_user_texts().join("\n");
    assert!(!opening.contains("Presets for spawn_agent"), "{opening}");

    let mut new = with_presets(REVIEWER);
    new.plugins.push("write_todos");
    let back = resume(
        run,
        new,
        vec![("/", vec![done("ok"), done("ok"), done("ok")])],
    )
    .await;
    let mut run = back.run;
    for text in ["still there?", "and now?", "last one"] {
        run.say(text).await;
        run.root_ended().await;
    }
    let after = run.requests("/");
    let after = &after[after.len() - 3..];
    assert_eq!(
        after[0].tool_names(),
        [
            "list_agents",
            "rename_agent",
            "send_message",
            "show_diff",
            "spawn_agent",
            "stop_agent",
            "write_todos"
        ]
    );
    assert!(after[0].messages.starts_with(&before.messages));
    let env = environment_of(&after[0]).expect("the environment is sent with the new tools");
    assert!(
        env.contains("Presets for spawn_agent: rev (Reviews)"),
        "{env}"
    );
    for pair in after.windows(2) {
        assert_eq!(pair[1].prefix(), pair[0].prefix());
        assert!(pair[1].messages.starts_with(&pair[0].messages));
        assert_eq!(environment_of(&pair[1]), None);
    }
    let _ = run.quit().await;
}

fn restamp(run: &Run, agent: &str, base: i64) {
    run.store.edit(&session_name(agent), |records| {
        for (i, record) in records.iter_mut().enumerate() {
            let at = Timestamp::from_second(base + i64::try_from(i).unwrap()).unwrap();
            record.at = crate::session::rfc3339(at);
        }
    });
}

fn logged_at(run: &Run, agent: &str, want: impl Fn(&LogEvent) -> bool) -> u64 {
    let at = run
        .store
        .read(&session_name(agent))
        .unwrap()
        .into_iter()
        .rev()
        .find(|r| want(&r.event))
        .unwrap()
        .at;
    u64::try_from(at.parse::<Timestamp>().unwrap().as_millisecond()).unwrap()
}

#[tokio::test(start_paused = true)]
async fn since_ms_after_a_resume_comes_from_the_record_that_settled_each_agent() {
    const RESUMED: i64 = 1_900_000_000_000;
    let mut run = start(
        "resume-since",
        Setup::default(),
        vec![
            ("/", vec![done("noted"); 6]),
            (
                "/idle",
                vec![uses(&[report_call("r1", "idle done")]), done("bye")],
            ),
            ("/cut", vec![hanging("cutting")]),
            ("/gone", vec![hanging("going")]),
            (
                "/lost",
                vec![uses(&[report_call("r2", "lost done")]), done("bye")],
            ),
        ],
    )
    .await;
    for name in ["idle", "cut", "gone", "lost"] {
        run.spawn(spawn_request(name, "Work.")).await;
    }
    reported(&mut run, "/idle").await;
    reported(&mut run, "/lost").await;
    streaming(&mut run, "/gone").await;
    run.send(ClientCommand::EndAgent {
        agent: path("/gone"),
    })
    .await;
    run.until("/gone to end", |e| {
        matches!(e, AgentEvent::AgentStateChanged { agent, state: AgentState::Ended, .. } if agent.as_str() == "/gone")
    })
    .await;
    streaming(&mut run, "/cut").await;
    idle(&mut run, "/idle").await;
    let run = run.quit().await;
    restamp(&run, "/", 1_000_000_000);
    restamp(&run, "/idle", 1_100_000_000);
    restamp(&run, "/cut", 1_200_000_000);
    run.store.remove(&session_name("/lost"));
    let idle_at = logged_at(&run, "/idle", |e| matches!(e, LogEvent::TurnEnded { .. }));
    let cut_at = logged_at(&run, "/cut", |e| {
        matches!(
            e,
            LogEvent::SessionEnded {
                interrupted: Some(_),
                ..
            }
        )
    });
    let gone_at = logged_at(
        &run,
        "/",
        |e| matches!(e, LogEvent::AgentEnded { agent, .. } if agent.as_str() == "/gone"),
    );
    let kernels = Kernels::default();
    let clock = Arc::new(ShiftedClock::at(Timestamp::UNIX_EPOCH, None));
    clock.set(Timestamp::from_millisecond(RESUMED).unwrap());
    let back = resume(
        run,
        Setup {
            clock: Some(clock),
            kernels: kernels.clone(),
            ..Setup::default()
        },
        Vec::new(),
    )
    .await;
    assert_eq!(back.live, ["/cut", "/idle"]);
    assert_eq!(back.ended, ["/gone", "/lost"]);
    let service = service_of(&kernels);
    let since = |agent| since_ms(&service, agent);
    assert_eq!(since("/idle"), idle_at);
    assert_eq!(since("/cut"), cut_at);
    assert_eq!(since("/gone"), gone_at);
    assert_eq!(since("/lost"), RESUMED as u64);
    let announced = service.publish_spend(&path("/idle"), 1.0, false);
    assert!(
        matches!(
            announced,
            Some(AgentEvent::AgentStateChanged {
                state: AgentState::Idle { .. },
                ..
            })
        ),
        "{announced:?}"
    );
    assert_eq!(
        since("/idle"),
        idle_at,
        "a spend-only announce keeps the time"
    );
    let mut run = back.run;
    let agents = list(&mut run).await;
    let idle = agents.into_iter().find(|a| a.path == path("/idle"));
    assert_eq!(idle.and_then(|a| a.since_ms), Some(idle_at));
    let _ = run.quit().await;
}

fn on_two() -> Setup {
    Setup {
        endpoints: vec![endpoint(ENV), endpoint("b")],
        ..Setup::default()
    }
}

async fn reported_on_b(name: &str) -> Run {
    let mut run = start(
        name,
        on_two(),
        vec![
            ("b:/r", vec![uses(&[report_call("r1", "on b")])]),
            ("/", vec![done("noted")]),
        ],
    )
    .await;
    run.spawn(SpawnRequest {
        endpoint: Some("b".into()),
        ..spawn_request("r", "Review.")
    })
    .await;
    reported(&mut run, "/r").await;
    tokio::time::sleep(Duration::from_secs(30)).await;
    run.quit().await
}

#[tokio::test(start_paused = true)]
async fn a_child_moved_in_its_chat_keeps_its_place_after_a_resume() {
    use wisp_protocol::settings::{SettingChange, SettingValue};
    let setup = || Setup {
        endpoints: vec![subscription("sub"), api("api")],
        ..Setup::default()
    };
    let mut run = start(
        "resume-moved-child",
        setup(),
        vec![
            ("sub:/w", vec![uses(&[report_call("r1", "done")])]),
            ("api:/w", vec![done("moved")]),
            ("sub:/", vec![done("noted")]),
        ],
    )
    .await;
    run.spawn(spawn_request("w", "Work.")).await;
    run.child_ended("/w").await;
    let text = |value: &str| SettingValue::Text(value.into());
    let changes = vec![
        SettingChange::named("endpoint", text("api")).unwrap(),
        SettingChange::named("model", text("claude-sonnet-5-5")).unwrap(),
        SettingChange::named("effort", text("low")).unwrap(),
    ];
    let request = run.request();
    let save = false;
    run.send(child(
        "/w",
        ClientCommand::ChangeSettings {
            request,
            changes,
            save,
        },
    ))
    .await;
    run.send(child_send("/w", "you-1", "Go on.")).await;
    run.child_ended("/w").await;
    tokio::time::sleep(Duration::from_secs(30)).await;
    let run = run.quit().await;
    let before = run.requests("api:/w").last().cloned().unwrap();
    assert_eq!(before.session.model, "claude-sonnet-5-5");
    let back = resume(
        run,
        setup(),
        vec![("api:/w", vec![done("again")]), ("sub:/", vec![done("ok")])],
    )
    .await;
    let mut run = back.run;
    run.send(child_send("/w", "you-2", "Once more.")).await;
    run.child_ended("/w").await;
    let agents = list(&mut run).await;
    let w = entry(&agents, "/w");
    assert_eq!(
        (w.endpoint.as_deref(), w.model.as_str(), w.effort),
        (Some("api"), "claude-sonnet-5-5", Effort::Low)
    );
    let after = run.requests("api:/w").last().cloned().unwrap();
    assert_eq!(after.prefix(), before.prefix());
    assert!(after.messages.starts_with(&before.messages));
    assert_eq!(run.requests("sub:/w").len(), 1);
    let _ = run.quit().await;
}

#[tokio::test(start_paused = true)]
async fn a_resumed_child_keeps_its_endpoint() {
    let run = reported_on_b("resume-endpoint-kept").await;
    let back = resume(
        run,
        on_two(),
        vec![("b:/r", vec![done("again")]), ("/", vec![done("ok")])],
    )
    .await;
    assert!(back.warnings.is_empty(), "{:?}", back.warnings);
    assert_eq!(back.live, ["/r"]);
    let mut run = back.run;
    let agents = list(&mut run).await;
    let r = entry(&agents, "/r");
    assert_eq!(
        (r.endpoint.as_deref(), r.provider.as_deref(), r.priced),
        (Some("b"), Some("anthropic"), true)
    );
    run.send(child_send("/r", "you-1", "Once more.")).await;
    run.child_ended("/r").await;
    let run = run.quit().await;
    let sent = run.requests("b:/r");
    assert_eq!(sent.len(), 2);
    assert!(sent[1].messages.starts_with(&sent[0].messages));
    assert_eq!(sent[1].prefix(), sent[0].prefix());
    assert!(run.requests("/r").is_empty());
}

#[tokio::test(start_paused = true)]
async fn a_child_whose_endpoint_is_gone_or_unavailable_falls_back_with_the_right_warning() {
    let cases: [(&[(&'static str, &'static str)], String); 2] = [
        (
            &[],
            format!(
                "/r ran on account b, which isn't configured now; it continues on the keys-file account with model {MODEL}"
            ),
        ),
        (
            &[("b", "no key in /opt/vault/b.env")],
            format!(
                "/r ran on account b, which is unavailable now (no key in /opt/vault/b.env); it continues on the keys-file account with model {MODEL}"
            ),
        ),
    ];
    for (n, (unavailable, warning)) in cases.into_iter().enumerate() {
        let run = reported_on_b(&format!("resume-endpoint-fallback-{n}")).await;
        let back = resume(
            run,
            Setup {
                unavailable: unavailable.to_vec(),
                ..Setup::default()
            },
            vec![("/r", vec![done("again")]), ("/", vec![done("ok")])],
        )
        .await;
        assert_eq!(back.warnings, [warning]);
        assert_eq!(back.live, ["/r"]);
        let mut run = back.run;
        let agents = list(&mut run).await;
        assert_eq!(entry(&agents, "/r").endpoint.as_deref(), Some(ENV));
        run.send(child_send("/r", "you-1", "Once more.")).await;
        run.child_ended("/r").await;
        let run = run.quit().await;
        assert_eq!(run.requests("/r").len(), 1);
        assert_eq!(run.requests("b:/r").len(), 1);
    }
}

pub(super) fn not_resumed(agent: &str, cause: &str) -> String {
    format!("{agent} wasn't resumed this run ({cause}); it stays ended until a later resume.")
}

#[tokio::test(start_paused = true)]
async fn an_unloaded_child_tells_its_parent_once_per_resume() {
    let mut run = start(
        "resume-unloaded-notice",
        Setup::default(),
        vec![
            ("/", vec![done("noted"); 3]),
            (
                "/lead",
                vec![
                    uses(&[
                        spawn_call("s1", "w1", "Work long."),
                        spawn_call("s2", "w2", "Work short."),
                    ]),
                    done("waiting"),
                    done("still waiting"),
                ],
            ),
            ("/lead/w1", vec![hanging("working")]),
            ("/lead/w2", vec![uses(&[report_call("r1", "done")])]),
            ("/solo", vec![hanging("thinking")]),
        ],
    )
    .await;
    run.spawn(spawn_request("lead", "Lead.")).await;
    run.spawn(spawn_request("solo", "Think.")).await;
    reported(&mut run, "/lead/w2").await;
    run.until("/lead/w1 to stream", |e| {
        matches!(inner(e, "/lead/w1"), Some(AgentEvent::TextDelta { .. }))
    })
    .await;
    run.until("/solo to stream", |e| {
        matches!(inner(e, "/solo"), Some(AgentEvent::TextDelta { .. }))
    })
    .await;
    tokio::time::sleep(Duration::from_secs(30)).await;
    let run = run.quit().await;
    run.store.remove(&session_name("/lead/w2"));
    let root_requests = run.requests("/").len();
    let lead_requests = run.requests("/lead").len();
    let busy = vec![session_name("/lead/w1"), session_name("/solo")];
    let back = resume(
        run,
        Setup {
            busy: busy.clone(),
            ..Setup::default()
        },
        vec![
            ("/lead", vec![uses(&[report_call("r1", "w1 is busy")])]),
            ("/", vec![done("noted")]),
        ],
    )
    .await;
    assert_eq!(back.live, ["/lead"]);
    let mut run = back.run;
    run.child_ended("/lead").await;
    tokio::time::sleep(Duration::from_secs(30)).await;
    let run = run.quit().await;
    assert_eq!(run.requests("/lead").len(), lead_requests + 1);
    assert_eq!(
        run.requests("/").len(),
        root_requests + 1,
        "the root wakes only for the lead's report, never for a notice"
    );
    let unreadable = "its log can't be opened or read";
    let lead = notices(&run, "/lead");
    assert_eq!(lead.len(), 2, "{lead:?}");
    for want in [
        (true, not_resumed("/lead/w1", unreadable)),
        (false, not_resumed("/lead/w2", unreadable)),
    ] {
        assert!(
            lead.iter()
                .any(|(wake, text)| *wake == want.0 && text.contains(&want.1)),
            "{want:?} not in {lead:?}"
        );
    }
    let root = notices(&run, "/");
    assert!(
        root.iter()
            .any(|(wake, text)| !*wake && text.contains(&not_resumed("/solo", unreadable))),
        "{root:?}"
    );
    for (_, text) in lead.iter().chain(&root) {
        assert!(!text.contains("another Wisp"), "{text}");
        assert!(!text.contains("s-lead"), "{text}");
    }
    let before = notices(&run, "/lead").len();
    let back = resume(
        run,
        Setup {
            busy,
            ..Setup::default()
        },
        vec![("/lead", vec![done("noted")])],
    )
    .await;
    let mut run = back.run;
    run.child_ended("/lead").await;
    let run = run.quit().await;
    assert_eq!(
        notices(&run, "/lead").len(),
        before + 2,
        "each resume repeats the notice once while its cause lasts"
    );
}

pub(super) fn set_provider(run: &Run, parent: &str, agent: &str, provider: Option<&str>) {
    let agent = path(agent);
    run.store.edit(&session_name(parent), |records| {
        for record in records.iter_mut() {
            if let LogEvent::AgentSpawned {
                agent: a,
                provider: p,
                ..
            } = &mut record.event
                && *a == agent
            {
                *p = provider.map(str::to_owned);
            }
        }
    });
}

#[tokio::test(start_paused = true)]
async fn a_childs_logged_provider_decides_where_it_resumes() {
    let narrow = FakeEndpoint {
        table: Table::Only(&["claude-sonnet-5-5"]),
        ..endpoint(ENV)
    };
    let mut run = start(
        "resume-logged-provider",
        Setup {
            endpoints: vec![narrow, endpoint("b")],
            ..Setup::default()
        },
        vec![
            ("/", vec![done("noted"); 4]),
            ("b:/r", vec![uses(&[report_call("r1", "r")])]),
            ("b:/u", vec![uses(&[report_call("r1", "u")])]),
            ("/f", vec![uses(&[report_call("r1", "f")])]),
            ("/n", vec![uses(&[report_call("r1", "n")])]),
        ],
    )
    .await;
    run.spawn(SpawnRequest {
        endpoint: Some("b".into()),
        model: Some("claude-sonnet-5-5".into()),
        ..spawn_request("r", "Work.")
    })
    .await;
    reported(&mut run, "/r").await;
    run.spawn(SpawnRequest {
        endpoint: Some("b".into()),
        ..spawn_request("u", "Work.")
    })
    .await;
    reported(&mut run, "/u").await;
    run.spawn(spawn_request("f", "Work.")).await;
    reported(&mut run, "/f").await;
    run.spawn(spawn_request("n", "Work.")).await;
    reported(&mut run, "/n").await;
    tokio::time::sleep(Duration::from_secs(30)).await;
    let run = run.quit().await;
    set_provider(&run, "/", "/r", None);
    set_provider(&run, "/", "/u", None);
    set_provider(&run, "/", "/f", Some("future"));
    set_provider(&run, "/", "/n", None);
    run.store.edit(&session_name("/n"), |records| {
        for record in records.iter_mut() {
            match &mut record.event {
                LogEvent::SessionStarted { endpoint_name, .. }
                | LogEvent::TurnStarted { endpoint_name, .. } => *endpoint_name = None,
                _ => {}
            }
        }
    });
    let sizes = [run.records("/u").len(), run.records("/f").len()];
    let back = resume(
        run,
        Setup {
            endpoints: vec![narrow],
            ..Setup::default()
        },
        Vec::new(),
    )
    .await;
    assert_eq!(back.live, ["/r"]);
    assert_eq!(back.ended, ["/f", "/n", "/u"]);
    assert_eq!(
        back.warnings,
        [
            "/f wasn't resumed: it ran on a provider this Wisp doesn't know (a newer Wisp wrote its log); it stays ended for this run".to_owned(),
            "/n wasn't resumed: its log names no account, and the keys-file account can't take its history; it stays ended for this run".to_owned(),
            "/r ran on account b, which isn't configured now; it continues on the keys-file account with model claude-sonnet-5-5".to_owned(),
            "/u wasn't resumed: it ran on account b, which isn't configured now, and the keys-file account doesn't serve its model claude-opus-5-5; it stays ended for this run".to_owned(),
        ]
        .into_iter()
        .map(|w| w.replace("claude-opus-5-5", MODEL))
        .collect::<Vec<_>>()
    );
    let mut run = back.run;
    let agents = list(&mut run).await;
    let f = entry(&agents, "/f");
    assert_eq!(
        (f.endpoint.as_deref(), f.provider.as_deref()),
        (Some(ENV), None)
    );
    let u = entry(&agents, "/u");
    assert_eq!(
        (u.endpoint.as_deref(), u.provider.as_deref()),
        (Some("b"), None)
    );
    tokio::time::sleep(Duration::from_secs(30)).await;
    let run = run.quit().await;
    assert_eq!([run.records("/u").len(), run.records("/f").len()], sizes);
    let root = notices(&run, "/");
    for want in [
        not_resumed("/u", "its account b isn't configured"),
        not_resumed("/f", "its log names a provider this Wisp doesn't know"),
        not_resumed("/n", "its log names no account"),
    ] {
        assert!(
            root.iter()
                .any(|(wake, text)| !*wake && text.contains(&want)),
            "{want} not in {root:?}"
        );
    }
}

#[tokio::test(start_paused = true)]
async fn a_changed_preset_endpoint_warns_on_resume() {
    let preset = |endpoint: &'static str| -> &'static str {
        match endpoint {
            "b" => "[agents.presets.reviewer]\ndescription = \"Reviews\"\nendpoint = \"b\"\n",
            _ => "[agents.presets.reviewer]\ndescription = \"Reviews\"\nendpoint = \"env\"\n",
        }
    };
    let mut run = start(
        "resume-preset-endpoint",
        Setup {
            preset_config: Some(preset("b")),
            ..on_two()
        },
        vec![
            ("b:/r", vec![uses(&[report_call("r1", "on b")])]),
            ("/", vec![done("noted")]),
        ],
    )
    .await;
    run.spawn(SpawnRequest {
        preset: Some("reviewer".into()),
        ..spawn_request("r", "Review.")
    })
    .await;
    reported(&mut run, "/r").await;
    tokio::time::sleep(Duration::from_secs(30)).await;
    let run = run.quit().await;
    let back = resume(
        run,
        Setup {
            preset_config: Some(preset("b")),
            ..on_two()
        },
        Vec::new(),
    )
    .await;
    assert!(back.warnings.is_empty(), "{:?}", back.warnings);
    let run = back.run.quit().await;
    let back = resume(
        run,
        Setup {
            preset_config: Some(preset(ENV)),
            ..on_two()
        },
        Vec::new(),
    )
    .await;
    assert_eq!(
        back.warnings,
        [
            "preset reviewer has changed; /r keeps the configuration it was spawned with, and new spawns use the preset as it is now"
        ]
    );
    let mut run = back.run;
    let agents = list(&mut run).await;
    assert_eq!(entry(&agents, "/r").endpoint.as_deref(), Some("b"));
    let _ = run.quit().await;
}

#[tokio::test(start_paused = true)]
async fn a_child_from_an_unpriced_endpoint_shows_it_and_is_told_when_it_is_metered_now() {
    let unpriced = || {
        vec![
            endpoint(ENV),
            FakeEndpoint {
                priced: false,
                ..endpoint("b")
            },
        ]
    };
    let mut run = start(
        "resume-unpriced",
        Setup {
            endpoints: unpriced(),
            ..Setup::default()
        },
        vec![
            ("b:/r", vec![uses(&[report_call("r1", "r")])]),
            ("b:/e", vec![uses(&[report_call("r1", "e")])]),
            ("/", vec![done("noted"); 2]),
        ],
    )
    .await;
    for name in ["r", "e"] {
        run.spawn(SpawnRequest {
            endpoint: Some("b".into()),
            ..spawn_request(name, "Work.")
        })
        .await;
        reported(&mut run, &format!("/{name}")).await;
    }
    tokio::time::sleep(Duration::from_secs(30)).await;
    let run = run.quit().await;
    let busy = vec![session_name("/e")];
    let back = resume(
        run,
        Setup {
            endpoints: unpriced(),
            busy: busy.clone(),
            ..Setup::default()
        },
        Vec::new(),
    )
    .await;
    assert_eq!(back.ended, ["/e"]);
    let mut run = back.run;
    let agents = list(&mut run).await;
    for agent in ["/e", "/r"] {
        let shown = entry(&agents, agent);
        assert_eq!(
            (shown.endpoint.as_deref(), shown.priced),
            (Some("b"), false),
            "{agent}"
        );
    }
    let run = run.quit().await;
    let back = resume(
        run,
        Setup {
            busy,
            ..Setup::default()
        },
        Vec::new(),
    )
    .await;
    assert!(
        back.warnings.contains(&format!(
            "/r ran on account b, which isn't configured now; it continues on the keys-file account with model {MODEL}; it is metered now"
        )),
        "{:?}",
        back.warnings
    );
    let mut run = back.run;
    let agents = list(&mut run).await;
    let r = entry(&agents, "/r");
    assert_eq!((r.endpoint.as_deref(), r.priced), (Some(ENV), true));
    let e = entry(&agents, "/e");
    assert_eq!((e.endpoint.as_deref(), e.priced), (Some("b"), true));
    let _ = run.quit().await;
}

#[tokio::test(start_paused = true)]
async fn an_ended_childs_split_survives_its_log() {
    let billings = [
        wisp_protocol::Billing::AnthropicPlan,
        wisp_protocol::Billing::Metered,
    ];
    for (billing, unreadable) in billings.into_iter().flat_map(|b| [(b, false), (b, true)]) {
        let mut child = super::limits::priced(uses(&[report_call("r", "done")]), 1000, 20);
        for event in &mut child.script {
            if let Ok(
                ProviderEvent::MessageStart { usage, .. }
                | ProviderEvent::MessageDelta { usage, .. },
            ) = event
            {
                usage.billing = billing;
            }
        }
        let mut run = start(
            &format!("ended-split-{billing:?}-{unreadable}"),
            Setup::default(),
            vec![("/child", vec![child]), ("/", vec![done("noted")])],
        )
        .await;
        run.spawn(spawn_request("child", "report")).await;
        reported(&mut run, "/child").await;
        run.send(ClientCommand::EndAgent {
            agent: path("/child"),
        })
        .await;
        run.until("/child to end", |e| {
            matches!(e, AgentEvent::AgentStateChanged { agent, state: AgentState::Ended, .. } if agent.as_str() == "/child")
        })
        .await;
        let run = run.quit().await;
        let spent = logged_spend(&run, "/child");
        assert!(spent > 0.0);
        let ended = run.records("/").into_iter().find_map(|e| match e {
            LogEvent::AgentEnded {
                spent_usd,
                plan_usd,
                billing,
                ..
            } => Some((spent_usd, plan_usd, billing)),
            _ => None,
        });
        let (written, shown) = if billing.is_metered() {
            (Some(0.0), None)
        } else {
            (Some(spent), Some(spent))
        };
        assert_eq!(ended, Some((spent, written, billing)));
        if unreadable {
            run.store.edit(&session_name("/child"), |records| {
                records.retain(|r| !matches!(r.event, LogEvent::SessionStarted { .. }));
            });
        } else {
            run.store.remove(&session_name("/child"));
        }
        let back = resume(run, Setup::default(), Vec::new()).await;
        assert_eq!(back.ended, ["/child"]);
        let mut run = back.run;
        let after = list(&mut run).await;
        let child = entry(&after, "/child");
        assert_eq!(
            (child.spent_usd, child.plan_usd, child.billing),
            (spent, shown, billing)
        );
        assert!(child.accounts.is_empty());
        let _ = run.quit().await;
    }
}

fn waits_on_its_process(note: bool) -> Vec<Step> {
    let mut steps = vec![uses(&[background("b1", &hold())])];
    if note {
        steps.push(uses(&[quiet_call("q1", "..", "Placeholder note")]));
    }
    steps.push(done("Placeholder wait"));
    steps
}

#[cfg(unix)]
#[tokio::test]
async fn a_restart_wakes_process_news_once_without_notifying_the_parent() {
    for case in [
        "waiting",
        "reported",
        "cancelled",
        "earlier-cut",
        "earlier-news",
    ] {
        let setup = || Setup {
            shell: true,
            ..Setup::default()
        };
        let stopped = matches!(case, "cancelled" | "earlier-cut");
        let steps = if stopped {
            vec![uses(&[background("b1", &hold())]), hanging("unfinished")]
        } else if case == "reported" {
            vec![
                uses(&[background("b1", &hold())]),
                uses(&[report_call("r1", "finished")]),
            ]
        } else {
            waits_on_its_process(true)
        };
        let mut run = start("restart-process-news", setup(), vec![("/c", steps)]).await;
        run.spawn(spawn_request("c", "Work.")).await;
        if stopped {
            streaming(&mut run, "/c").await;
            if case == "cancelled" {
                run.send(cancel("/c")).await;
                assert!(matches!(
                    run.child_ended("/c").await,
                    TurnOutcome::Cancelled { .. }
                ));
            }
        } else if case == "reported" {
            reported(&mut run, "/c").await;
        } else {
            run.until(
                "waiting on the process",
                idle_as("/c", IdleOutcome::Waiting),
            )
            .await;
        }
        let mut run = run.quit().await;
        if matches!(case, "earlier-cut" | "earlier-news") {
            let back = resume(run, setup(), vec![]).await;
            tokio::time::sleep(Duration::from_millis(100)).await;
            run = back.run.quit().await;
        }
        let before = run.requests("/c").len();
        let parent_notices = notices(&run, "/").len();
        let wake = matches!(case, "waiting" | "reported");
        let mut back = resume(
            run,
            Setup {
                restarted: true,
                ..setup()
            },
            vec![("/c", vec![uses(&[report_call("r1", "finished")])])],
        )
        .await;
        if wake {
            back.run
                .until("the process-news turn", |e| {
                    matches!(
                        inner(e, "/c"),
                        Some(AgentEvent::TurnEnded {
                            outcome: TurnOutcome::Completed { .. },
                            ..
                        })
                    )
                })
                .await;
            let next = back.run.requests("/c")[before].all_text();
            assert_eq!(next.matches(brief::RESTARTED).count(), 1, "{case}");
            assert!(next.contains("<processes>"), "{case}: {next}");
            assert!(next.contains("stopped"), "{case}: {next}");
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
        assert_eq!(
            back.run.requests("/c").len(),
            before + usize::from(wake),
            "{case}"
        );
        let run = back.run.quit().await;
        assert_eq!(notices(&run, "/").len(), parent_notices, "{case}");
        assert_eq!(
            notices(&run, "/c")
                .iter()
                .filter(|(_, text)| text.contains(brief::RESTARTED))
                .count(),
            usize::from(wake),
            "{case}"
        );
        let back = resume(
            run,
            Setup {
                restarted: true,
                ..setup()
            },
            vec![],
        )
        .await;
        tokio::time::sleep(Duration::from_millis(100)).await;
        assert_eq!(
            back.run.requests("/c").len(),
            before + usize::from(wake),
            "process news is heard at most once: {case}"
        );
        back.run.quit().await;
    }
}

fn unwoken(agent: &str) -> String {
    format!(
        "{agent} won't be woken by its processes from before Wisp restarted; message it to continue."
    )
}

#[cfg(unix)]
#[tokio::test]
async fn children_waiting_on_their_processes_at_the_exit_tell_their_parents_at_the_resume() {
    let shell = || Setup {
        shell: true,
        ..Setup::default()
    };
    let gate = Arc::new(Semaphore::new(0));
    let spawns = [
        spawn_call("s1", "a", "Work a."),
        spawn_call("s2", "b", "Work b."),
        spawn_call("s3", "c", "Work c."),
    ];
    let mut run = start(
        "resume-unwoken",
        shell(),
        vec![
            (
                "/lead",
                vec![
                    uses(&spawns),
                    gated(done("waiting on them"), &gate),
                    done("noted"),
                ],
            ),
            ("/lead/a", waits_on_its_process(true)),
            (
                "/lead/b",
                vec![
                    uses(&[background("b1", &hold())]),
                    uses(&[send_call("n1", "..", "Placeholder waking note")]),
                    done("Placeholder wait"),
                ],
            ),
            (
                "/lead/c",
                vec![
                    uses(&[background("b1", &wait_for("go"))]),
                    uses(&[quiet_call("q1", "..", "Placeholder note")]),
                    done("Placeholder wait"),
                ],
            ),
            ("/solo", waits_on_its_process(true)),
            ("/", vec![done("noted")]),
        ],
    )
    .await;
    run.spawn(spawn_request("lead", "Lead.")).await;
    run.spawn(spawn_request("solo", "Work solo.")).await;
    for agent in ["/lead/a", "/lead/b", "/lead/c", "/solo"] {
        run.until("the wait", idle_as(agent, IdleOutcome::Waiting))
            .await;
    }
    gate.add_permits(1);
    assert!(until(|| run.requests("/lead").len() >= 3).await);
    within("the lead to settle", async {
        while !matches!(
            entry(&list(&mut run).await, "/lead").state,
            AgentState::Idle { .. }
        ) {
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await;
    std::fs::write(run.env.dir("work").join("go"), "").unwrap();
    run.until("c's process to end", |e| {
        matches!(inner(e, "/lead/c"), Some(AgentEvent::ProcessEnded { .. }))
    })
    .await;
    let run = run.quit().await;
    let back = resume(run, shell(), vec![("/lead", vec![done("noted"); 3])]).await;
    let mut run = back.run;
    run.child_ended("/lead").await;
    let after = list(&mut run).await;
    for agent in ["/lead/a", "/lead/b", "/lead/c", "/solo"] {
        let waiting = AgentState::Idle {
            outcome: Some(IdleOutcome::Waiting),
        };
        assert_eq!(entry(&after, agent).state, waiting, "{agent}");
    }
    let run = run.quit().await;
    let lead = notices(&run, "/lead");
    for agent in ["/lead/a", "/lead/b", "/lead/c"] {
        let told: Vec<&(bool, String)> = lead
            .iter()
            .filter(|(_, text)| text.contains(&unwoken(agent)))
            .collect();
        assert!(matches!(told.as_slice(), [(true, _)]), "{agent}: {lead:?}");
    }
    assert_eq!(lead.len(), 3, "{lead:?}");
    let root = notices(&run, "/");
    assert!(
        matches!(root.as_slice(), [(false, text)] if text.contains(&unwoken("/solo"))),
        "{root:?}"
    );
}

#[cfg(unix)]
#[tokio::test(start_paused = true)]
async fn a_child_that_asked_with_the_process_notice_off_resumes_asked_and_tells_no_one() {
    let off = || Setup {
        shell: true,
        flags: vec![crate::plugin::testing::hook_flag(
            "shell",
            "processes",
            false,
        )],
        ..Setup::default()
    };
    let mut run = start(
        "resume-notice-off",
        off(),
        vec![
            (
                "/asker",
                vec![
                    uses(&[background("b1", &hold())]),
                    uses(&[send_call("a1", "..", "Placeholder question?")]),
                    done("Placeholder asked"),
                ],
            ),
            ("/", vec![done("noted")]),
        ],
    )
    .await;
    run.spawn(spawn_request("asker", "Ask.")).await;
    run.until("the ask", idle_as("/asker", IdleOutcome::Asked))
        .await;
    run.root_ended().await;
    let run = run.quit().await;
    let mut run = resume(run, off(), Vec::new()).await.run;
    let asked = AgentState::Idle {
        outcome: Some(IdleOutcome::Asked),
    };
    assert_eq!(entry(&list(&mut run).await, "/asker").state, asked);
    tokio::time::sleep(Duration::from_secs(30)).await;
    let run = run.quit().await;
    assert!(notices(&run, "/").is_empty(), "{:?}", notices(&run, "/"));
}

#[tokio::test(start_paused = true)]
async fn a_resumed_tree_keeps_its_renames() {
    let mut run = start(
        "resume-renames",
        Setup::default(),
        vec![
            ("/", vec![done("ok"); 4]),
            (
                "/lead",
                vec![
                    uses(&[
                        spawn_call("s1", "w", "Work."),
                        spawn_call("s2", "old", "Old."),
                    ]),
                    done("waiting"),
                    done("renamed"),
                ],
            ),
            ("/lead/w", vec![hanging("working")]),
            ("/lead/old", vec![hanging("idle")]),
            (
                "/solo",
                vec![uses(&[report_call("r2", "solo done")]), done("x")],
            ),
        ],
    )
    .await;
    run.spawn(spawn_request("lead", "Lead.")).await;
    run.spawn(spawn_request("solo", "Solo.")).await;
    streaming(&mut run, "/lead/w").await;
    streaming(&mut run, "/lead/old").await;
    end_agent(&mut run, "/lead/old").await;
    run.child_ended("/lead").await;
    run.child_ended("/solo").await;
    for (agent, name) in [("/lead", "boss"), ("/solo", "single")] {
        super::rename::rename(&mut run, agent, name).await;
    }
    run.send(child_send("/boss", "c1", "Go on.")).await;
    run.child_ended("/boss").await;
    tokio::time::sleep(Duration::from_secs(10)).await;
    let before = run.requests("/lead").last().cloned().unwrap();
    let run = run.quit().await;
    let setup = Setup::default();
    let kernels = setup.kernels.clone();
    let scripts = vec![
        ("/", vec![done("ok"); 4]),
        (
            "/boss",
            vec![uses(&[report_call("r1", "resumed")]), done("status")],
        ),
        ("/single", vec![done("again")]),
    ];
    let mut back = resume(run, setup, scripts).await;
    assert_eq!(back.live, ["/boss", "/single", "/boss/w"]);
    assert_eq!(back.ended, ["/boss/old"]);
    assert!(service_of(&kernels).tree().entries[&path("/boss/w")].owes);
    let run = &mut back.run;
    let reason = super::refusals::refused(run, spawn_request("lead", "Again.")).await;
    assert!(
        reason.contains("already has an agent named lead"),
        "{reason}"
    );
    run.child_ended("/boss").await;
    run.send(child_send("/lead", "c2", "Status?")).await;
    run.child_ended("/boss").await;
    for next in run.requests("/boss") {
        grows_from(&next, &before, "the renamed agent");
        assert_eq!(environment_of(&next), None);
    }
    run.send(child_send("/solo", "c3", "Again?")).await;
    run.child_ended("/single").await;
    let texts = run.requests("/single")[0].last_user_texts();
    let blocks: Vec<&String> = texts
        .iter()
        .filter(|t| t.starts_with("<environment>"))
        .collect();
    assert_eq!(blocks.len(), 1, "{texts:?}");
    assert!(blocks[0].contains("Agent: /single\n"), "{}", blocks[0]);
    end_agent(run, "/boss/w").await;
    let again = resume(back.run.quit().await, Setup::default(), Vec::new()).await;
    assert_eq!(again.live, ["/boss", "/single"]);
    assert_eq!(again.ended, ["/boss/old", "/boss/w"]);
    let _ = again.run.quit().await;
}

#[test]
fn kept_fixtures_cover_the_ended_logs_costs_rows_and_links() {
    use super::super::load::{FIXTURES, ended_log, kept_version};
    use crate::kit::ended_lines;
    use crate::session::{Line, read_lines};
    let facts: Vec<_> = FIXTURES
        .iter()
        .zip([0, 1, 1, 0])
        .map(|(text, damaged)| {
            let lines = read_lines(text.as_bytes()).lines;
            assert_eq!(
                lines
                    .iter()
                    .filter(|l| !matches!(l, Line::Record(_)))
                    .count(),
                damaged
            );
            ended_log(&ended_lines(text.as_bytes()).unwrap())
        })
        .collect();
    let expected = [
        (
            (25.56, false),
            Some(10.52),
            vec![
                ("subscription", 20.76, Some(10.52), false),
                ("api", 4.8, Some(0.0), false),
            ],
        ),
        (
            (5.15, true),
            Some(1.28),
            vec![("api", 0.03, Some(0.0), false)],
        ),
        (
            (20.47, true),
            Some(2.24),
            vec![
                ("unpriced", 0.0, Some(0.0), true),
                ("turn-bound", 10.24, Some(0.0), true),
                ("step-bound", 0.01, Some(0.0), true),
                ("retry-bound", 0.02, Some(0.0), true),
                ("refresh-bound", 0.04, Some(0.0), true),
                ("side-bound", 0.08, Some(0.0), true),
                ("name-bound", 0.16, Some(0.0), true),
                ("voice-bound", 0.32, Some(0.0), true),
                ("compacted-bound", 0.64, Some(0.32), true),
                ("prepared-bound", 1.28, Some(0.64), true),
                ("failed-bound", 2.56, Some(1.28), true),
                ("between-bound", 5.12, Some(0.0), true),
            ],
        ),
        (
            (819.2, true),
            None,
            vec![
                ("usage-split", 655.36, Some(655.36), false),
                ("unknown-split", 163.84, None, true),
            ],
        ),
    ];
    let close = |actual: f64, expected: f64| {
        assert!((actual - expected).abs() < 1e-9, "{actual} != {expected}")
    };
    let part = |actual: Option<f64>, expected: Option<f64>| {
        assert_eq!(actual.is_some(), expected.is_some());
        if let (Some(actual), Some(expected)) = (actual, expected) {
            close(actual, expected);
        }
    };
    assert_eq!(facts.len(), expected.len());
    for (log, ((usd, lower), plan, splits)) in facts.iter().zip(expected) {
        let ((actual, bound), subscription, billing, accounts) = log.spend.as_ref().unwrap();
        close(*actual, usd);
        assert_eq!(*bound, lower);
        part(*subscription, plan);
        assert_eq!(*billing, wisp_protocol::Billing::Mixed);
        assert_eq!(accounts.len(), splits.len());
        for (name, usd, plan, lower) in splits {
            let account = accounts[name];
            close(account.usd, usd);
            part(account.plan_usd, plan);
            assert_eq!(account.lower_bound, lower, "{name}");
        }
    }
    let whole = &facts[0];
    assert_eq!(
        whole.row.last_report.as_deref(),
        Some("No-report placeholder")
    );
    assert_eq!(whole.row.running_ms, Some(66));
    assert_eq!(whole.row.checklist[0].text, "Written placeholder");
    assert_eq!(whole.links.spawned.len(), 1);
    assert_eq!(whole.links.spawned[0].path, path("/helper/child"));
    assert_eq!(
        whole.links.renames,
        [(path("/helper/child"), path("/helper/renamed"))]
    );
    assert_eq!(whole.links.ended[&path("/helper/renamed")].spent_usd, 1.0);
    assert_eq!(whole.links.mail, 42);
    let broken = &facts[1];
    assert_eq!(broken.row.last_report, None);
    assert_eq!(broken.row.running_ms, Some(11));
    assert_eq!(broken.row.checklist[0].text, "Inherited placeholder");
    assert_eq!(broken.links.spawned.len(), 1);
    assert_eq!(
        facts[2].row.last_report.as_deref(),
        Some("Report placeholder")
    );
    assert_eq!(facts[2].row.running_ms, Some(66));
    assert_eq!(facts[2].links.spawned.len(), 1);
    assert_eq!(
        facts[3].row.last_report.as_deref(),
        Some("Typed-report placeholder")
    );
    assert_eq!(facts[3].row.running_ms, Some(33));
    assert_eq!(
        kept_version().unwrap(),
        crate::session::sha256_bytes_hex(&serde_json::to_vec(&facts).unwrap())
    );
}

#[tokio::test(start_paused = true)]
async fn kept_ended_rows_skip_their_logs_and_keep_the_tree_names_and_mail() {
    use super::super::load::{keep, read_kept};
    let mut run = start(
        "resume-kept",
        Setup::default(),
        vec![
            ("/", vec![done("noted"); 4]),
            (
                "/finished",
                vec![
                    uses(&[spawn_call("s", "kid", "Placeholder task")]),
                    done("waiting"),
                    uses(&[report_call("r", "Finished placeholder")]),
                ],
            ),
            (
                "/finished/kid",
                vec![uses(&[report_call("r", "Child placeholder")])],
            ),
            (
                "/later",
                vec![uses(&[report_call("r", "Later placeholder")])],
            ),
        ],
    )
    .await;
    run.spawn(spawn_request("finished", "Placeholder task"))
        .await;
    run.spawn(spawn_request("later", "Placeholder task")).await;
    reported(&mut run, "/finished/kid").await;
    reported(&mut run, "/finished").await;
    reported(&mut run, "/later").await;
    super::rename::rename(&mut run, "/finished", "past").await;
    end_agent(&mut run, "/past").await;
    let ran = durations(&run, "/finished");
    let mut back = resume(run.quit().await, Setup::default(), Vec::new()).await;
    assert_eq!(back.ended, ["/past", "/past/kid"]);
    let before = list(&mut back.run).await;
    let entry = |rows: &[AgentEntry], name: &str| {
        rows.iter()
            .find(|a| a.path.as_str() == name)
            .unwrap()
            .clone()
    };
    assert_eq!(
        entry(&before, "/past").last_report.as_deref(),
        Some("Finished placeholder")
    );
    assert_eq!(entry(&before, "/past").running_ms, Some(ran));
    let logs = Logs {
        store: back.run.store.clone(),
        busy: Vec::new(),
        sessions: None,
        kept: back.run.env.root.join("ended"),
    };
    let root = session_name("/");
    let mut facts = read_kept(&logs, &root);
    assert_eq!(facts.len(), 2);
    facts
        .get_mut(&session_name("/finished"))
        .unwrap()
        .1
        .row
        .last_report = Some("Kept placeholder".to_owned());
    keep(&logs, &root, &facts);
    end_agent(&mut back.run, "/later").await;
    let highest = ["/", "/finished", "/finished/kid", "/later"]
        .iter()
        .flat_map(|p| mail_ids(&back.run, p))
        .max()
        .unwrap();
    let mut back = resume(
        back.run.quit().await,
        Setup::default(),
        vec![(
            "/fresh",
            vec![uses(&[report_call("r", "Fresh placeholder")])],
        )],
    )
    .await;
    let after = list(&mut back.run).await;
    let mut changed = entry(&before, "/past");
    changed.last_report = Some("Kept placeholder".to_owned());
    assert_eq!(entry(&after, "/past"), changed);
    assert_eq!(entry(&after, "/past/kid"), entry(&before, "/past/kid"));
    assert_eq!(entry(&after, "/later").state, AgentState::Ended);
    assert_eq!(read_kept(&logs, &root).len(), 3);
    let refusal = super::refusals::refused(&mut back.run, spawn_request("finished", "Again")).await;
    assert!(
        refusal.contains("already has an agent named finished"),
        "{refusal}"
    );
    back.run
        .spawn(spawn_request("fresh", "Placeholder task"))
        .await;
    reported(&mut back.run, "/fresh").await;
    assert_eq!(mail_ids(&back.run, "/fresh")[0], highest + 1);
    let _ = back.run.quit().await;
}

#[test]
fn jsonl_ended_facts_survive_a_reopen_and_fall_back_per_stale_or_bad_line() {
    use super::super::load::{FIXTURES, Kept, ended_facts, ended_log, keep, read_kept};
    use crate::kit::{JsonlLogs, ended_lines};
    use std::io::Write;
    let env = ScratchDir::new("core", "kept-jsonl");
    let logs = JsonlLogs {
        on: true,
        dir: Some(env.dir("sessions")),
    };
    let store = logs.store().unwrap();
    let child = ended_log(&ended_lines(FIXTURES[0].as_bytes()).unwrap())
        .links
        .spawned
        .remove(0);
    let mut other = child.clone();
    other.session_id = crate::session::new_session_id();
    for spawned in [&child, &other] {
        fs::write(store.path(&spawned.session_id), FIXTURES[0]).unwrap();
    }
    let root = crate::session::new_session_id();
    let mut facts = Kept::new();
    for spawned in [&child, &other] {
        let (stamp, log) = ended_facts(&logs, spawned, &facts);
        let mut log = log.unwrap();
        log.row.last_report = Some("Kept placeholder".to_owned());
        facts.insert(spawned.session_id.clone(), (stamp.unwrap(), log));
    }
    keep(&logs, &root, &facts);
    let reopened = JsonlLogs {
        on: true,
        dir: logs.dir.clone(),
    };
    let kept = read_kept(&reopened, &root);
    let report = |spawned: &crate::agents::fold::SpawnedRecord, kept: &Kept| {
        ended_facts(&reopened, spawned, kept)
            .1
            .unwrap()
            .row
            .last_report
            .unwrap()
    };
    assert_eq!(report(&child, &kept), "Kept placeholder");
    fs::OpenOptions::new()
        .append(true)
        .open(store.path(&child.session_id))
        .unwrap()
        .write_all(b"\n")
        .unwrap();
    assert_eq!(report(&child, &kept), "No-report placeholder");
    assert_eq!(report(&other, &kept), "Kept placeholder");
    facts.get_mut(&child.session_id).unwrap().0 = reopened.stamp(&child.session_id).unwrap();
    keep(&logs, &root, &facts);
    assert_eq!(
        report(&child, &read_kept(&reopened, &root)),
        "Kept placeholder"
    );
    let text = logs.ended(&root).unwrap();
    let bad = text
        .lines()
        .map(|line| {
            if line.starts_with(&format!("[\"{}\",", child.session_id)) {
                "damaged placeholder"
            } else {
                line
            }
        })
        .collect::<Vec<_>>()
        .join("\n");
    logs.keep_ended(&root, &bad);
    let kept = read_kept(&reopened, &root);
    assert_eq!(report(&child, &kept), "No-report placeholder");
    assert_eq!(report(&other, &kept), "Kept placeholder");
    logs.keep_ended(
        &root,
        &format!("another version\n{}", text.split_once('\n').unwrap().1),
    );
    let kept = read_kept(&reopened, &root);
    assert!(kept.is_empty());
    assert_eq!(report(&other, &kept), "No-report placeholder");
    fs::remove_file(env.root.join("ended").join(format!("{root}.jsonl"))).unwrap();
    assert!(read_kept(&reopened, &root).is_empty());
    let off = JsonlLogs {
        on: false,
        dir: logs.dir.clone(),
    };
    keep(&off, &root, &facts);
    assert!(off.ended(&root).is_none());
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        logs.keep_ended(&root, &text);
        let folder = env.root.join("ended");
        assert_eq!(
            fs::metadata(&folder).unwrap().permissions().mode() & 0o777,
            0o700
        );
        assert_eq!(
            fs::metadata(folder.join(format!("{root}.jsonl")))
                .unwrap()
                .permissions()
                .mode()
                & 0o777,
            0o600
        );
    }
    logs.keep_ended("../invalid", &text);
    assert!(logs.ended("../invalid").is_none());
}
