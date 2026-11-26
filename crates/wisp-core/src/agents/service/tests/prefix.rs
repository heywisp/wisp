use wisp_plugin_api::tree::TreeEvent;
use wisp_protocol::HookOutcome;
use wisp_protocol::settings::{SettingChange, SettingValue};

use super::hooks::{Heards, agent_hooks, heard_until, listener};
use super::limits::{SUMMARY, priced};
use super::resume::{child_send, environment_of, list, reported};
use super::*;
use crate::plugin::hooks::points::AGENT_POINTS;

#[tokio::test(start_paused = true)]
async fn same_choice_siblings_send_identical_tools_and_system_and_spawning_never_changes_the_roots()
{
    siblings_share_one_prefix(Setup::default()).await;
}

#[tokio::test(start_paused = true)]
async fn siblings_on_a_subscription_with_its_own_model_list_share_one_prefix_too() {
    let listed = FakeEndpoint {
        list: Some(crate::models::list::tests::sample),
        ..openai(ENV, wisp_protocol::Billing::ChatGptPlan)
    };
    siblings_share_one_prefix(Setup {
        endpoints: vec![listed],
        root_model: GPT,
        ..Setup::default()
    })
    .await;
}

#[tokio::test(start_paused = true)]
async fn siblings_on_a_claude_account_with_its_own_model_list_share_one_prefix_too() {
    let listed = FakeEndpoint {
        list: Some(crate::models::list::tests::claude_sample),
        ..endpoint(ENV)
    };
    siblings_share_one_prefix(Setup {
        endpoints: vec![listed],
        ..Setup::default()
    })
    .await;
}

async fn siblings_share_one_prefix(setup: Setup) {
    let mut run = start(
        "prefix-siblings",
        Setup {
            plugins: if cfg!(unix) {
                vec!["show_diff", "view_file", "bash"]
            } else {
                vec!["show_diff"]
            },
            presets: presets(
                "[agents.presets.reviewer]\ndescription = \"Reviews a change\"\neffort = \"high\"\n",
            ),
            ..setup
        },
        vec![
            (
                "/",
                vec![
                    done("before"),
                    done("woken"),
                    done("after"),
                    done("more"),
                    done("more"),
                    done("more"),
                ],
            ),
            ("/one", vec![uses(&[report_call("r1", "one")]), done("x")]),
            ("/two", vec![uses(&[report_call("r2", "two")]), done("x")]),
            (
                "/three",
                vec![uses(&[report_call("r3", "three")]), done("x")],
            ),
            (
                "/four",
                vec![
                    uses(&[call("r4", REPORT, json!({"text": "four\nin detail"}))]),
                    done("x"),
                ],
            ),
        ],
    ).await;
    run.say("hello").await;
    run.root_ended().await;
    let other = run.env.dir("other");
    run.spawn(spawn_request("one", "First task.")).await;
    run.spawn(SpawnRequest {
        cwd: Some(other.display().to_string()),
        todos: vec!["Check".into()],
        ..spawn_request("two", "A different task.")
    })
    .await;
    run.spawn(SpawnRequest {
        preset: Some("reviewer".into()),
        ..spawn_request("three", "Third.")
    })
    .await;
    run.spawn(spawn_request("four", "Fourth.")).await;
    for child in ["/one", "/two", "/three", "/four"] {
        run.child_ended(child).await;
    }
    run.root_ended().await;
    run.say("again").await;
    run.root_ended().await;
    let one = run.requests("/one")[0].prefix();
    let names = run.requests("/one")[0].tool_names();
    assert!(names.contains(&"show_diff".to_owned()), "{names:?}");
    assert!(
        !cfg!(unix) || names.contains(&"view_file".to_owned()),
        "{names:?}"
    );
    for child in ["/two", "/three", "/four"] {
        assert_eq!(run.requests(child)[0].prefix(), one, "{child}");
    }
    let two = run.requests("/two")[0].first_user_texts();
    assert!(two[0].contains("Working directory: ~/other"), "{two:?}");
    let root = run.requests("/");
    assert!(root.len() >= 3, "{}", root.len());
    let first = root[0].prefix();
    for (n, request) in root.iter().enumerate() {
        assert_eq!(request.prefix(), first, "root request {n}");
    }
    assert_ne!(first, one, "a child never shares the root's prefix");
    let root_env = root[0].first_user_texts();
    assert!(
        root_env[0].contains("Presets for spawn_agent: reviewer (Reviews a change)\n"),
        "{root_env:?}"
    );
    for pair in root.windows(2) {
        assert!(
            pair[1].messages.starts_with(&pair[0].messages),
            "the root's history only grows"
        );
    }
    let _ = run.quit().await;
}

#[tokio::test(start_paused = true)]
async fn a_rename_appends_and_never_edits_a_request() {
    let gate = Arc::new(Semaphore::new(0));
    let mut run = start(
        "prefix-rename",
        Setup::default(),
        vec![
            (
                "/",
                vec![
                    done("one"),
                    done("two"),
                    uses(&[rename_call("n1", "/lead/leaf", "tip")]),
                    done("three"),
                ],
            ),
            (
                "/mid",
                vec![
                    uses(&[spawn_call("s1", "leaf", "Go deeper.")]),
                    uses(&[report_call("r1", "mid done")]),
                    done("x"),
                    done("read"),
                    done("again"),
                ],
            ),
            (
                "/mid/leaf",
                vec![
                    gated(uses(&[report_call("r2", "leaf done")]), &gate),
                    done("x"),
                    done("again"),
                    done("tip"),
                ],
            ),
        ],
    )
    .await;
    run.spawn(spawn_request("mid", "Middle.")).await;
    run.child_ended("/mid").await;
    gate.add_permits(1);
    run.child_ended("/mid/leaf").await;
    run.child_ended("/mid").await;
    run.root_ended().await;
    run.say("first").await;
    run.root_ended().await;
    let before = |run: &Run, agent: &str| run.requests(agent).last().cloned().unwrap();
    let (mid, leaf) = (before(&run, "/mid"), before(&run, "/mid/leaf"));
    super::rename::rename(&mut run, "/mid", "lead").await;
    for (agent, id) in [("/lead", "c1"), ("/lead/leaf", "c2")] {
        run.send(child_send(agent, id, "Again?")).await;
        run.child_ended(agent).await;
    }
    run.say("second").await;
    run.root_ended().await;
    for (agent, earlier, lines) in [
        ("/mid", &mid, "Agent: /lead\nParent: main\n"),
        ("/mid/leaf", &leaf, "Agent: /lead/leaf\nParent: /lead\n"),
    ] {
        let next = before(&run, agent);
        grows_from(&next, earlier, agent);
        let environment = environment_of(&next).unwrap_or_default();
        assert!(environment.contains(lines), "{agent}: {environment}");
    }
    let root = run.requests("/");
    let [.., earlier, next] = root.as_slice() else {
        panic!("{}", root.len())
    };
    grows_from(next, earlier, "main");
    assert!(
        next.all_text()
            .contains("/mid was renamed to /lead by the user.")
    );
    let leaf = before(&run, "/mid/leaf");
    run.send(child_send("/lead/tip", "c3", "Once more?")).await;
    run.child_ended("/lead/tip").await;
    let next = before(&run, "/mid/leaf");
    grows_from(&next, &leaf, "tip");
    let environment = environment_of(&next).unwrap_or_default();
    assert!(
        environment.contains("Agent: /lead/tip\nParent: /lead\n"),
        "{environment}"
    );
    let _ = run.quit().await;
}

async fn answered_by(run: &mut Run, agent: &str, request: RequestId) -> AgentEvent {
    let event = run
        .until(&format!("{agent}'s answer"), |e| match inner(e, agent) {
            Some(AgentEvent::Compacted { answers, .. }) => *answers == Some(request),
            Some(AgentEvent::Rewound { answers, .. }) => *answers == Some(request),
            Some(AgentEvent::RewindPoints { answers, .. }) => *answers == request,
            _ => false,
        })
        .await;
    inner(&event, agent).cloned().unwrap()
}

#[tokio::test(start_paused = true)]
async fn a_childs_own_commands_change_only_its_own_requests() {
    let mut run = start(
        "prefix-child-commands",
        Setup::default(),
        vec![
            ("/", vec![done("noted"); 5]),
            (
                "/one",
                vec![
                    uses(&[report_call("r1", "one")]),
                    done("x"),
                    priced(done(SUMMARY), 5_000, 300),
                    done("two"),
                    done("three"),
                    done("instead"),
                ],
            ),
            (
                "/two",
                vec![uses(&[report_call("r2", "two")]), done("x"), done("again")],
            ),
            (
                "/three",
                vec![uses(&[report_call("r3", "three")]), done("x")],
            ),
        ],
    )
    .await;
    run.say("Hello.").await;
    run.root_ended().await;
    run.spawn(spawn_request("one", "First task.")).await;
    run.spawn(spawn_request("two", "Second task.")).await;
    run.child_ended("/one").await;
    run.child_ended("/two").await;
    let (one, two) = (run.requests("/one"), run.requests("/two"));
    let request = run.request();
    let changes = vec![
        SettingChange::named("model", SettingValue::Text("claude-sonnet-5-5".into())).unwrap(),
        SettingChange::named("effort", SettingValue::Text("low".into())).unwrap(),
    ];
    let save = false;
    run.send(child(
        "/one",
        ClientCommand::ChangeSettings {
            request,
            changes,
            save,
        },
    ))
    .await;
    let request = run.request();
    run.send(child(
        "/one",
        ClientCommand::Compact {
            focus: None,
            request,
        },
    ))
    .await;
    answered_by(&mut run, "/one", request).await;
    let compaction = &run.requests("/one")[one.len()];
    grows_from(compaction, one.last().unwrap(), "the compaction");
    for (n, text) in ["Go on.", "And on."].into_iter().enumerate() {
        run.send(child_send("/one", &format!("o{n}"), text)).await;
        run.child_ended("/one").await;
    }
    let moved = run.requests("/one");
    let request = run.request();
    run.send(child("/one", ClientCommand::RewindPoints { request }))
        .await;
    let AgentEvent::RewindPoints { points, .. } = answered_by(&mut run, "/one", request).await
    else {
        unreachable!()
    };
    assert_eq!(points[0].label, "\u{21bb} a message from main");
    let to = wisp_protocol::RewindPoint::Turn { turn: 3 };
    let request = run.request();
    run.send(child("/one", ClientCommand::Rewind { to, request }))
        .await;
    answered_by(&mut run, "/one", request).await;
    run.send(child_send("/one", "o2", "Instead.")).await;
    run.child_ended("/one").await;
    let last = run.requests("/one").last().cloned().unwrap();
    grows_from(&last, &moved[moved.len() - 2], "after the rewind");
    assert!(!last.all_text().contains("And on."), "{}", last.all_text());
    assert_eq!(last.session.model, "claude-sonnet-5-5");
    run.send(child_send("/two", "t0", "Again.")).await;
    run.child_ended("/two").await;
    grows_from(
        &run.requests("/two")[two.len()],
        two.last().unwrap(),
        "/two",
    );
    run.spawn(spawn_request("three", "Third task.")).await;
    run.child_ended("/three").await;
    assert_eq!(run.requests("/three")[0].prefix(), two[0].prefix());
    run.say("Still there?").await;
    tokio::time::sleep(Duration::from_secs(30)).await;
    let root = run.requests("/");
    assert!(root.len() >= 2, "{}", root.len());
    for pair in root.windows(2) {
        grows_from(&pair[1], &pair[0], "the root");
    }
    let client = |r: &Sent| {
        let tools = crate::tool::canonical_specs(&r.tools);
        (
            r.session.system.clone(),
            r.session.instructions.clone(),
            tools,
        )
    };
    assert_eq!(client(&last), client(&two[0]));
    let _ = run.quit().await;
}

#[tokio::test(start_paused = true)]
async fn a_default_grandchild_reuses_its_parents_exact_prefix_and_the_depth_limit_changes_nothing()
{
    let mut run = start(
        "prefix-grandchild",
        Setup::default(),
        vec![
            (
                "/mid",
                vec![
                    uses(&[spawn_call("s1", "leaf", "Go one deeper.")]),
                    uses(&[report_call("r1", "mid done")]),
                    done("x"),
                ],
            ),
            (
                "/mid/leaf",
                vec![
                    uses(&[spawn_call("s2", "deeper", "Too deep.")]),
                    uses(&[report_call("r2", "leaf done")]),
                    done("x"),
                ],
            ),
            ("/", vec![done("ok")]),
        ],
    )
    .await;
    run.spawn(spawn_request("mid", "Middle.")).await;
    run.child_ended("/mid/leaf").await;
    run.child_ended("/mid").await;
    let mid = run.requests("/mid");
    let leaf = run.requests("/mid/leaf");
    assert_eq!(leaf[0].prefix(), mid[0].prefix());
    let env = leaf[0].first_user_texts();
    assert!(
        env[0].contains("Spawning: you are at the depth limit (2), so spawn_agent refuses\n"),
        "{env:?}"
    );
    let refusal = leaf[1].last_user_texts().join("\n");
    assert!(
        refusal.contains("the depth limit (2) is reached"),
        "{refusal}"
    );
    let run = run.quit().await;
    assert!(run.records("/mid/leaf").iter().any(|e| matches!(
        e,
        LogEvent::AgentRefused {
            action: AgentAction::Spawn,
            ..
        }
    )));
    assert!(run.requests("/mid/leaf/deeper").is_empty());
}

#[tokio::test(start_paused = true)]
async fn a_preset_supplies_choices_and_a_spawns_own_fields_override_it() {
    let mut run = start(
        "prefix-preset",
        Setup {
            plugins: vec!["show_diff"],
            presets: presets(
                "[agents.presets.reviewer]\ndescription = \"Reviews\"\neffort = \"low\"\ntools_off = [\"show_diff\"]\nspawn = false\n",
            ),
            ..Setup::default()
        },
        vec![("/r", vec![hanging("x")]), ("/s", vec![hanging("x")])],
    ).await;
    run.spawn(SpawnRequest {
        preset: Some("reviewer".into()),
        ..spawn_request("r", "Review.")
    })
    .await;
    run.spawn(SpawnRequest {
        preset: Some("reviewer".into()),
        effort: Some(Effort::Medium),
        spawn: Some(true),
        ..spawn_request("s", "Review again.")
    })
    .await;
    let mut configs = BTreeMap::new();
    for _ in 0..2 {
        let event = run
            .until("a spawn", |e| matches!(e, AgentEvent::AgentSpawned { .. }))
            .await;
        if let AgentEvent::AgentSpawned { agent, config, .. } = event {
            configs.insert(agent.path.to_string(), (agent.effort, config));
        }
    }
    assert_eq!(
        configs["/r"],
        (
            Effort::Low,
            SpawnChoices {
                preset: Some("reviewer".into()),
                tools_off: vec!["show_diff".into()],
                spawn: false,
                instructions: None,
                preset_values: configs["/r"].1.preset_values.clone(),
                keep_warm: false,
                choices: Vec::new(),
            }
        )
    );
    let logged = configs["/r"]
        .1
        .preset_values
        .as_ref()
        .map(|p| p.name.as_str());
    assert_eq!(logged, Some("reviewer"));
    assert_eq!(configs["/s"].0, Effort::Medium);
    assert!(configs["/s"].1.spawn);
    run.until("s to stream", |e| {
        matches!(inner(e, "/s"), Some(AgentEvent::TextDelta { .. }))
    })
    .await;
    run.until("r to stream", |e| {
        matches!(inner(e, "/r"), Some(AgentEvent::TextDelta { .. }))
    })
    .await;
    let r = &run.requests("/r")[0];
    assert_eq!(r.session.effort, Effort::Low);
    assert_eq!(r.tool_names(), ["list_agents", "report", "send_message"]);
    let s = &run.requests("/s")[0];
    assert_eq!(s.session.effort, Effort::Medium);
    assert_eq!(
        s.tool_names(),
        [
            "list_agents",
            "report",
            "send_message",
            "spawn_agent",
            "stop_agent"
        ]
    );
    let brief = r.first_user_texts();
    assert_eq!(
        brief[1], "<wisp:message from=\"/\" kind=\"task\">\nReview.\n</wisp:message>",
        "{brief:?}"
    );
    let _ = run.quit().await;
}

fn default_leaders() -> Vec<&'static str> {
    let mut leaders = vec![
        "ask_user",
        "list_accounts",
        "list_settings",
        "manage",
        "remember",
        "rename_chat",
        "search",
        "show_code",
        "show_diff",
        "spawn_agent",
        "web_search",
        "write_todos",
    ];
    if cfg!(unix) {
        leaders.extend(["ask_secret", "bash", "view_file"]);
    }
    leaders.sort_unstable();
    let (registry, _) = crate::plugin::Registry::new(
        wisp_plugins::bundled(),
        &crate::plugin::PluginConfig::default(),
    );
    assert_eq!(
        registry.default_on(),
        leaders.iter().map(|n| (*n).to_owned()).collect()
    );
    leaders
}

fn with_unix(mut names: Vec<&'static str>, unix: &[&'static str]) -> Vec<String> {
    if cfg!(unix) {
        names.extend(unix);
    }
    names.sort_unstable();
    names.into_iter().map(str::to_owned).collect()
}

#[tokio::test(start_paused = true)]
async fn a_default_root_sends_the_default_leaders_and_followers_and_its_children_get_them_with_report()
 {
    let mut run = start(
        "prefix-defaults",
        Setup {
            agents: false,
            plugins: default_leaders(),
            ..Setup::default()
        },
        vec![
            (
                "/",
                vec![
                    done("hi"),
                    done("a"),
                    done("b"),
                    done("c"),
                    done("d"),
                    done("e"),
                ],
            ),
            ("/one", vec![uses(&[report_call("r1", "one")]), done("x")]),
            ("/two", vec![uses(&[report_call("r2", "two")]), done("x")]),
            ("/lone", vec![uses(&[report_call("r3", "lone")]), done("x")]),
        ],
    )
    .await;
    run.say("hello").await;
    run.root_ended().await;
    run.spawn(spawn_request("one", "First.")).await;
    run.spawn(spawn_request("two", "Second.")).await;
    run.spawn(SpawnRequest {
        spawn: Some(false),
        ..spawn_request("lone", "Alone.")
    })
    .await;
    for child in ["/one", "/two", "/lone"] {
        run.child_ended(child).await;
    }
    run.root_ended().await;
    let root = run.requests("/");
    let agents = [
        "list_agents",
        "send_message",
        "spawn_agent",
        "stop_agent",
        "write_todos",
    ];
    let mut root_tools = agents.to_vec();
    root_tools.extend([
        "ask_user",
        "rename_agent",
        "list_accounts",
        "list_settings",
        "remember",
        "search",
        "show_code",
        "show_diff",
        "use",
        "web_search",
    ]);
    assert_eq!(
        root[0].tool_names(),
        with_unix(root_tools, &["ask_secret", "bash", "process", "view_file"])
    );
    assert!(root[0].server_names().is_empty());
    for (n, request) in root.iter().enumerate() {
        assert_eq!(request.prefix(), root[0].prefix(), "root request {n}");
        assert_eq!(
            request.server_names(),
            root[0].server_names(),
            "root request {n}"
        );
    }
    let mut child_tools = agents.to_vec();
    child_tools.extend([
        "list_accounts",
        "list_settings",
        "remember",
        "report",
        "search",
        "show_code",
        "show_diff",
        "use",
        "web_search",
    ]);
    let one = &run.requests("/one")[0];
    assert_eq!(
        one.tool_names(),
        with_unix(child_tools, &["bash", "process", "view_file"])
    );
    assert!(one.server_names().is_empty());
    assert_eq!(run.requests("/two")[0].prefix(), one.prefix());
    let lone = &run.requests("/lone")[0];
    assert_eq!(
        lone.tool_names(),
        with_unix(
            vec![
                "list_agents",
                "list_accounts",
                "list_settings",
                "remember",
                "report",
                "search",
                "send_message",
                "show_code",
                "show_diff",
                "use",
                "web_search",
                "write_todos"
            ],
            &["bash", "process", "view_file"]
        )
    );
    let _ = run.quit().await;
}

fn without_ran(text: &str) -> String {
    const ROW: &str = " \u{b7} ran ";
    let mut out = String::with_capacity(text.len());
    let mut rest = text;
    while let Some(at) = rest.find(ROW) {
        out.push_str(&rest[..at + ROW.len()]);
        rest = &rest[at + ROW.len()..];
        let end = rest
            .find(|c: char| !(c.is_ascii_digit() || "hms".contains(c)))
            .unwrap_or(rest.len());
        if rest.starts_with(|c: char| c.is_ascii_digit()) && rest[end..].starts_with(" \u{b7} ") {
            out.push_str("<ran>");
            rest = &rest[end..];
        }
    }
    out.push_str(rest);
    out
}

const OBSERVED: [&str; 3] = ["/", "/a", "/b"];

async fn observed_tree(
    name: &str,
    hooks: Vec<(String, crate::plugin::Make)>,
    heard: Option<&Heards>,
) -> (
    BTreeMap<&'static str, (usize, String)>,
    Run,
    tokio::time::Instant,
) {
    let kernels = Kernels::default();
    let mut run = start(
        name,
        Setup {
            hooks,
            kernels: kernels.clone(),
            ..Setup::default()
        },
        vec![
            (
                "/",
                vec![
                    uses(&[spawn_call("s1", "a", "Look around.")]),
                    uses(&[spawn_call("s2", "b", "Work long.")]),
                    done("spawned"),
                    uses(&[stop_call("t1", "b", false)]),
                    uses(&[stop_call("t2", "b", true)]),
                    done("ended b"),
                    done("noted"),
                    done("noted"),
                ],
            ),
            (
                "/a",
                vec![
                    uses(&[call("l1", "list_agents", json!({}))]),
                    uses(&[send_call("m1", "../b", "Hello b.")]),
                    uses(&[report_call("r1", "a done")]),
                    done("bye"),
                ],
            ),
            ("/b", vec![hanging("working")]),
        ],
    )
    .await;
    run.say("go").await;
    run.root_ended().await;
    reported(&mut run, "/a").await;
    run.root_ended().await;
    let last = tokio::time::Instant::now();
    if let Some(heard) = heard {
        heard_until(heard, "the end of /b", |e| {
            e.iter().any(|e| matches!(e, TreeEvent::AgentEnded(_)))
        })
        .await;
    }
    tokio::time::sleep(Duration::from_secs(30)).await;
    let agents = list(&mut run).await;
    let root_session = service_of(&kernels).tree().entries[&AgentPath::root()]
        .session_id
        .clone();
    let run = run.quit().await;
    let root = run.env.root.display().to_string();
    let requests = OBSERVED
        .into_iter()
        .map(|path| {
            let requests: Vec<_> = run
                .requests(path)
                .into_iter()
                .map(|r| {
                    (
                        r.text(),
                        r.beta(),
                        r.server_names(),
                        r.session,
                        r.tools,
                        r.deferred,
                        r.messages,
                    )
                })
                .collect();
            let mut text = format!("{requests:?}").replace(&root, "<root>");
            text = text.replace(&root_session, "<session />");
            for agent in &agents {
                text = text.replace(&agent.session_id, &format!("<session {}>", agent.path));
            }
            (path, (requests.len(), without_ran(&text)))
        })
        .collect();
    (requests, run, last)
}

#[tokio::test(start_paused = true)]
async fn observing_plugins_change_no_request_byte() {
    let (without, _, _) = observed_tree("prefix-unobserved", Vec::new(), None).await;
    let heard = Heards::default();
    let never = Arc::new(Semaphore::new(0));
    let watcher = listener(&heard, &agent_hooks(""), vec![("agent-state", never)]);
    let (with, run, last) = observed_tree("prefix-observed", vec![watcher], Some(&heard)).await;
    for path in OBSERVED {
        assert!(with[path].0 > 0, "{path}");
        assert_eq!(with[path], without[path], "{path}");
    }
    let root_session = run
        .records("/")
        .into_iter()
        .find_map(|e| match e {
            LogEvent::SessionStarted { session_id, .. } => Some(session_id),
            _ => None,
        })
        .unwrap();
    let all = heard.lock().unwrap().clone();
    for point in AGENT_POINTS {
        assert!(all.iter().any(|h| h.hook == point), "{point}: {all:#?}");
    }
    for h in &all {
        let me =
            h.me.as_ref()
                .map(|m| (m.path.as_str(), m.session_id.as_str()));
        assert_eq!(me, Some(("/", root_session.as_str())), "{h:?}");
    }
    assert!(all.iter().any(|h| h.tree > 0), "{all:#?}");
    let first = all.iter().find(|h| h.hook == "agent-state").unwrap();
    assert!(
        first.at < last,
        "the gated hook was entered before the last turn ended"
    );
    let runs: Vec<(String, String, HookOutcome)> = run
        .records("/")
        .into_iter()
        .filter_map(|e| match e {
            LogEvent::HookRan {
                hook,
                point,
                turn: None,
                step: None,
                call_id: None,
                outcome,
                ..
            } if AGENT_POINTS.contains(&point.as_str()) => Some((hook, point, outcome)),
            _ => None,
        })
        .collect();
    for point in AGENT_POINTS {
        assert!(
            runs.iter().any(|(_, p, _)| p == point),
            "{point}: {runs:#?}"
        );
    }
    let states: Vec<&HookOutcome> = runs
        .iter()
        .filter(|(_, p, _)| p == "agent-state")
        .map(|(_, _, o)| o)
        .collect();
    assert!(!states.is_empty());
    assert!(
        states.iter().all(|o| **o == HookOutcome::TimedOut),
        "{states:?}"
    );
}

#[tokio::test(start_paused = true)]
async fn children_of_one_preset_share_their_prefix_on_any_endpoint() {
    let mut run = start(
        "prefix-endpoints",
        Setup {
            endpoints: vec![endpoint(ENV), endpoint("b")],
            preset_config: Some(
                "[agents.presets.reviewer]\ndescription = \"Reviews a change\"\nendpoint = \"b\"\n",
            ),
            ..Setup::default()
        },
        vec![
            (
                "/",
                vec![done("before"), done("woken"), done("more"), done("more")],
            ),
            ("b:/one", vec![uses(&[report_call("r1", "one")])]),
            ("b:/two", vec![uses(&[report_call("r2", "two")])]),
            ("/three", vec![uses(&[report_call("r3", "three")])]),
        ],
    )
    .await;
    run.say("hello").await;
    run.root_ended().await;
    for name in ["one", "two"] {
        run.spawn(SpawnRequest {
            preset: Some("reviewer".into()),
            ..spawn_request(name, "Review.")
        })
        .await;
    }
    run.spawn(SpawnRequest {
        preset: Some("reviewer".into()),
        endpoint: Some(ENV.into()),
        ..spawn_request("three", "Review.")
    })
    .await;
    for child in ["/one", "/two", "/three"] {
        run.child_ended(child).await;
    }
    let run = run.quit().await;
    let one = &run.requests("b:/one")[0];
    let two = &run.requests("b:/two")[0];
    let three = &run.requests("/three")[0];
    assert_eq!(one.prefix(), two.prefix());
    assert_eq!(one.tools, two.tools);
    assert_eq!(one.prefix(), three.prefix());
    assert_eq!(one.tools, three.tools);
    assert_eq!(one.session.system, three.session.system);
    let root = run.requests("/");
    let first = root[0].prefix();
    for (n, request) in root.iter().enumerate() {
        assert_eq!(request.prefix(), first, "root request {n}");
    }
}

#[tokio::test(start_paused = true)]
async fn the_agent_tool_specs_are_the_same_whatever_the_endpoints() {
    let mut specs = Vec::new();
    for endpoints in [
        vec![endpoint(ENV)],
        vec![endpoint(ENV), endpoint("gwalpha"), endpoint("gwbeta")],
    ] {
        let mut run = start(
            "prefix-endpoint-specs",
            Setup {
                endpoints,
                ..Setup::default()
            },
            vec![("/", vec![done("hi")])],
        )
        .await;
        run.say("hello").await;
        run.root_ended().await;
        let run = run.quit().await;
        let tools = run.requests("/")[0].tools.clone();
        let names: Vec<&str> = tools.iter().map(|t| t.name.as_str()).collect();
        assert!(names.contains(&SPAWN_AGENT), "{names:?}");
        assert!(names.contains(&"list_agents"), "{names:?}");
        specs.push(serde_json::to_string(&tools).unwrap());
    }
    assert_eq!(specs[0], specs[1]);
    for name in ["gwalpha", "gwbeta"] {
        assert!(!specs[1].contains(name), "{name}");
    }
}

#[cfg(unix)]
#[tokio::test]
async fn later_children_ignore_main_chat_off_names_but_keep_starting_restrictions() {
    let dynamic = "mcp__local__z_echo";
    let saved_dynamic = "mcp__local__a-first";
    for by_agent in [false, true] {
        let env = ScratchDir::new("wisp-agents-test-", "prefix-chat-off-mcp");
        let service = crate::mcp::Service::new(
            BTreeMap::from([(
                "local".to_owned(),
                crate::config::McpServer {
                    keys: Vec::new(),
                    command: crate::test_guard::mcp_server(),
                    args: vec!["text".to_owned()],
                },
            )]),
            env.root.clone(),
            Some(env.root.join("mcp")),
        );
        service.connect("local").await.unwrap();
        let tool_starts = ToolStarts::default();
        let mut saved = crate::config::SavedSettings::default();
        saved.insert(
            wisp_protocol::settings::SitePath::global(
                wisp_protocol::settings::find("turned_off").unwrap(),
            ),
            SettingValue::List(vec!["show_diff".to_owned(), saved_dynamic.to_owned()]),
            1,
        );
        let mut root = vec![
            done("Begin."),
            done("Before."),
            done("Next."),
            done("After."),
        ];
        if by_agent {
            let calls = ["bash", dynamic].into_iter().enumerate().map(|(n, name)| call(
                &format!("m{n}"), "use", json!({
                    "name": "manage", "input": {"action": "off", "kind": "tool", "name": name}
                }),
            )).collect::<Vec<_>>();
            root.splice(2..2, [uses(&calls), done("Off.")]);
        }
        let mut run = start(
            "prefix-chat-off",
            Setup {
                plugins: vec!["bash", "show_diff", "search", "manage", "view_file"],
                full_screen: true,
                documented: true,
                root_model: "claude-opus-4-7",
                saved: Some(saved),
                mcp: service,
                tool_starts: tool_starts.clone(),
                ..Setup::default()
            },
            vec![
                ("/", root),
                (
                    "/before",
                    vec![uses(&[report_call("r1", "Before.")]), done("End.")],
                ),
                (
                    "/after",
                    vec![uses(&[report_call("r2", "After.")]), done("End.")],
                ),
            ],
        )
        .await;
        run.say("Begin.").await;
        run.root_ended().await;
        run.spawn(spawn_request("before", "Work.")).await;
        run.child_ended("/before").await;
        run.root_ended().await;
        if by_agent {
            run.say("Turn bash and the echo tool off.").await;
            run.root_ended().await;
        } else {
            let request = run.request();
            run.send(ClientCommand::ChangeSettings {
                request,
                changes: vec![
                    SettingChange::named(
                        "turned_off",
                        SettingValue::List(
                            ["bash", "show_diff", dynamic, saved_dynamic]
                                .map(str::to_owned)
                                .to_vec(),
                        ),
                    )
                    .unwrap(),
                ],
                save: false,
            })
            .await;
            run.until("the list change", |event| matches!(event, AgentEvent::SettingsChanged { answers: Some(answer), .. } if *answer == request)).await;
        }
        run.say("Next message.").await;
        run.root_ended().await;
        assert!(run.seen.iter().any(|event| matches!(event,
            AgentEvent::ToolsChanged { removed, .. } if removed.contains(&"bash".to_owned()) && removed.contains(&"process".to_owned())
        )), "{by_agent}: {:?}", run.seen);
        if by_agent {
            let result = result_of(&run.requests("/")[3], "m1");
            assert!(
                !result.0 && result.1.contains(&wisp_tool_text::manage::now_off(dynamic)),
                "{result:?}"
            );
        }
        run.spawn(spawn_request("after", "Work.")).await;
        run.child_ended("/after").await;
        run.root_ended().await;
        for child in ["/before", "/after"] {
            let starts = tool_starts.lock().unwrap();
            let names = &starts[child];
            for name in ["bash", "process", "view_file", dynamic] {
                assert!(
                    names.contains(&name.to_owned()),
                    "{by_agent} {child}: {names:?}"
                );
            }
            for name in ["show_diff", saved_dynamic] {
                assert!(
                    !names.contains(&name.to_owned()),
                    "{by_agent} {child}: {names:?}"
                );
            }
        }
        let _ = run.quit().await;
    }
}
