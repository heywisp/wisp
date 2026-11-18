use wisp_plugin_api::commands::CommandReply;
use wisp_protocol::settings::{ENABLED_KEY, INSTALLED_KEY};
use wisp_protocol::{CommandOutcome, CommandRefusal, RejectKind};

use super::*;
use crate::plugin::PluginSwitches;
use crate::plugin::testing::{hook_flag, plugin_flag, plugin_switch, plugins_changed};
use crate::settings::host::StartLayers;
use crate::test_guard::until_event;

async fn catalogued(agent: Agent, sources: Vec<(String, Make)>) -> Agent {
    started_as(agent, sources, Start::Root(PluginSwitches::default())).await
}

enum Start {
    Root(PluginSwitches),
    Child(PluginSwitches),
}

async fn started_as(agent: Agent, sources: Vec<(String, Make)>, start: Start) -> Agent {
    let mut all = crate::plugin::testing::turn_line_sources();
    all.extend(sources);
    let config = PluginConfig {
        switches: match &start {
            Start::Root(switches) => switches.clone(),
            Start::Child(_) => PluginSwitches::default(),
        },
        ..PluginConfig::default()
    };
    let (registry, problems) = Registry::from_sources(all, &config);
    assert!(problems.is_empty(), "{problems:?}");
    let host = Arc::new(KernelHost::new());
    let (started, agent) = match &start {
        Start::Root(_) => (registry.start(&BTreeSet::new(), host).await, agent),
        Start::Child(switches) => (
            registry.start_child(&BTreeSet::new(), switches, host).await,
            agent.with_plugin_switches(switches),
        ),
    };
    agent
        .with_catalog(started.catalog)
        .with_hooks(started.hooks, started.host)
}

fn flagged(agent: Agent, flags: Vec<crate::settings::Given>) -> Agent {
    agent.with_start_layers(StartLayers {
        flags,
        ..StartLayers::default()
    })
}

fn named() -> kit::Run {
    Arc::new(|_, name, _| Box::pin(async move { CommandReply::Note(name) }))
}

fn commanded(body: &str, calls: &Calls) -> (String, Make) {
    let make: Make = kit::commanding(kit::none(), named(), calls.clone());
    let body = body.to_owned() + &kit::command("go", "");
    (kit::manifest_in("0.2.0", "aa", &body), make)
}

fn go(request: u64) -> ClientCommand {
    ClientCommand::RunPluginCommand {
        name: "aa:go".to_owned(),
        args: wisp_protocol::Secret::new(String::new()),
        request: RequestId(request),
    }
}

struct Seen {
    events: mpsc::Receiver<AgentEvent>,
    all: Vec<AgentEvent>,
}

impl Seen {
    fn new(events: mpsc::Receiver<AgentEvent>) -> Self {
        Self {
            events,
            all: Vec::new(),
        }
    }

    async fn until(&mut self, pick: impl Fn(&AgentEvent) -> bool) -> usize {
        if let Some(at) = self.all.iter().position(&pick) {
            return at;
        }
        until_event(&mut self.events, &mut self.all, "the event", pick).await;
        self.all.len() - 1
    }

    async fn answered(&mut self, request: u64) -> usize {
        self.until(
            |e| matches!(e, AgentEvent::SettingsChanged { answers: Some(r), .. } if r.0 == request),
        )
        .await
    }

    async fn ended(&mut self, turns: usize) -> usize {
        loop {
            let at = self
                .until(|e| matches!(e, AgentEvent::TurnEnded { .. }))
                .await;
            let count = self
                .all
                .iter()
                .filter(|e| matches!(e, AgentEvent::TurnEnded { .. }))
                .count();
            if count >= turns {
                return at;
            }
            until_event(&mut self.events, &mut self.all, "another event", |_| true).await;
        }
    }

    async fn command(&mut self, request: u64) -> CommandOutcome {
        let at = self
            .until(|e| matches!(e, AgentEvent::CommandDone { answers, .. } if answers.0 == request))
            .await;
        let AgentEvent::CommandDone { outcome, .. } = &self.all[at] else {
            unreachable!()
        };
        outcome.clone()
    }

    async fn close(mut self, commands: mpsc::Sender<ClientCommand>) -> Vec<AgentEvent> {
        drop(commands);
        while let Some(e) = self.events.recv().await {
            self.all.push(e);
        }
        self.all
    }
}

fn catalogs(seen: &[AgentEvent]) -> Vec<(usize, bool, bool)> {
    seen.iter()
        .enumerate()
        .filter_map(|(at, e)| match e {
            AgentEvent::CatalogChanged { catalog } => {
                let entry = catalog.plugins.iter().find(|p| p.id == "aa")?;
                Some((at, entry.installed, entry.enabled))
            }
            _ => None,
        })
        .collect()
}

fn plugin_off() -> impl Fn(&CommandOutcome) -> bool {
    |outcome| {
        matches!(
            outcome,
            CommandOutcome::Refused {
                kind: CommandRefusal::PluginOff,
                ..
            }
        )
    }
}

#[tokio::test]
async fn a_plugin_off_at_start_runs_no_start_hook_is_announced_off_and_refuses_its_commands() {
    let calls = Calls::default();
    let agent = flagged(
        plain(Fake::new(Vec::new())),
        vec![plugin_flag("aa", ENABLED_KEY, false)],
    );
    let agent = catalogued(
        agent,
        vec![commanded(&hook_body("s", "session-start", ""), &calls)],
    )
    .await;
    let (commands, events, _task) = spawn(agent);
    let mut seen = Seen::new(events);
    commands.send(go(1)).await.unwrap();
    assert!(plugin_off()(&seen.command(1).await), "{:#?}", seen.all);
    let seen = seen.close(commands).await;
    assert_eq!(catalogs(&seen), [(catalogs(&seen)[0].0, true, false)]);
    assert_eq!(calls.count("s"), 0);
    assert!(calls.commands().is_empty());
}

#[tokio::test]
async fn a_session_only_switch_off_replays_on_resume_before_the_start_hooks() {
    let calls = Calls::default();
    let store = MemoryStore::default();
    let body = hook_body("s", "session-start", "");
    let agent = catalogued(
        logged(Fake::new(vec![ends("one")]), &store),
        vec![commanded(&body, &calls)],
    )
    .await;
    let (commands, events, _task) = spawn(agent);
    let mut seen = Seen::new(events);
    commands
        .send(plugin_switch(1, "aa", ENABLED_KEY, false))
        .await
        .unwrap();
    seen.answered(1).await;
    commands.send(ClientCommand::send("a", "a")).await.unwrap();
    seen.ended(1).await;
    seen.close(commands).await;
    assert_eq!(calls.count("s"), 1);
    let records = records(&store);
    let resumed = catalogued(
        logged(Fake::new(Vec::new()), &store)
            .resume(&records, 0)
            .unwrap(),
        vec![commanded(&body, &calls)],
    )
    .await;
    let (commands, events, _task) = spawn(resumed);
    let mut seen = Seen::new(events);
    commands.send(go(2)).await.unwrap();
    assert!(plugin_off()(&seen.command(2).await), "{:#?}", seen.all);
    let seen = seen.close(commands).await;
    let announced = catalogs(&seen);
    assert_eq!(announced, [(announced[0].0, true, false)]);
    assert_eq!(calls.count("s"), 1);
}

fn blocks_all() -> HookScript {
    Arc::new(|_, _, _| Box::pin(async { Reply::Block("no".into()) }))
}

#[tokio::test]
async fn a_blocking_input_hook_lets_the_next_message_through_once_its_plugin_or_hook_is_off() {
    for off in [
        plugin_switch(1, "aa", ENABLED_KEY, false),
        plugin_switch(1, "aa", INSTALLED_KEY, false),
        switch(1, "aa", "u", false),
    ] {
        let calls = Calls::default();
        let provider = Fake::new(vec![ends("one")]);
        let agent = with_sections(
            plain(provider.clone()),
            vec![source(
                "aa",
                &hook_body("u", "user-input", ""),
                blocks_all(),
                &calls,
            )],
        )
        .await;
        let (commands, events, _task) = spawn(agent);
        let mut seen = Seen::new(events);
        commands
            .send(ClientCommand::send("m1", "first"))
            .await
            .unwrap();
        seen.until(|e| {
            matches!(
                e,
                AgentEvent::Rejected {
                    kind: Some(RejectKind::InputBlocked),
                    ..
                }
            )
        })
        .await;
        commands.send(off).await.unwrap();
        seen.answered(1).await;
        commands
            .send(ClientCommand::send("m2", "second"))
            .await
            .unwrap();
        seen.ended(1).await;
        seen.close(commands).await;
        assert_eq!(calls.count("u"), 1);
        let requests = provider.sent();
        assert_eq!(requests.len(), 1);
        assert!(texts(&requests[0].messages[0]).contains(&"second"));
    }
}

fn slow_input(gate: Arc<Notify>, started: Arc<Notify>) -> HookScript {
    Arc::new(move |_, _, event| {
        let gate = gate.clone();
        let started = started.clone();
        let input = matches!(event, Event::UserInput(_));
        Box::pin(async move {
            if input {
                started.notify_one();
                gate.notified().await;
            }
            Reply::None
        })
    })
}

#[tokio::test]
async fn a_switch_sent_during_a_slow_input_hook_leaves_that_turns_hooks_and_applies_at_the_next() {
    let calls = Calls::default();
    let (gate, started) = (Arc::new(Notify::new()), Arc::new(Notify::new()));
    let body = hook_body("u", "user-input", "timeout_ms = 60000\n")
        + &hook_body("b", "before-turn", "")
        + &hook_body("a", "after-turn", "");
    let provider = Fake::new(vec![ends("one"), ends("two")]);
    let agent = with_sections(
        plain(provider),
        vec![source(
            "aa",
            &body,
            slow_input(gate.clone(), started.clone()),
            &calls,
        )],
    )
    .await;
    let (commands, events, _task) = spawn(agent);
    let mut seen = Seen::new(events);
    commands
        .send(ClientCommand::send("m1", "first"))
        .await
        .unwrap();
    within("the hook's start", started.notified()).await;
    commands
        .send(plugin_switch(1, "aa", ENABLED_KEY, false))
        .await
        .unwrap();
    settle().await;
    gate.notify_one();
    seen.ended(1).await;
    assert_eq!(
        (calls.count("u"), calls.count("b"), calls.count("a")),
        (1, 1, 1)
    );
    commands
        .send(ClientCommand::send("m2", "second"))
        .await
        .unwrap();
    seen.ended(2).await;
    seen.close(commands).await;
    assert_eq!(
        (calls.count("u"), calls.count("b"), calls.count("a")),
        (1, 1, 1)
    );
}

#[tokio::test]
async fn one_catalog_change_goes_out_at_the_next_turns_start_and_none_at_the_change() {
    let calls = Calls::default();
    let provider = Fake::new(vec![ends("one")]);
    let agent = catalogued(
        plain(provider),
        vec![source(
            "aa",
            &hook_body("x", "before-turn", ""),
            kit::none(),
            &calls,
        )],
    )
    .await;
    let (commands, events, _task) = spawn(agent);
    let mut seen = Seen::new(events);
    seen.until(|e| matches!(e, AgentEvent::CatalogChanged { .. }))
        .await;
    commands
        .send(plugin_switch(1, "aa", ENABLED_KEY, false))
        .await
        .unwrap();
    let answer = seen.answered(1).await;
    commands.send(ClientCommand::send("m", "go")).await.unwrap();
    let started = seen
        .until(|e| matches!(e, AgentEvent::TurnStarted { .. }))
        .await;
    seen.ended(1).await;
    let seen = seen.close(commands).await;
    let announced = catalogs(&seen);
    assert_eq!(announced.len(), 2, "{announced:?}");
    assert_eq!((announced[0].1, announced[0].2), (true, true));
    let (at, installed, enabled) = announced[1];
    assert!(answer < at && at < started, "{answer} {at} {started}");
    assert_eq!((installed, enabled), (true, false));
    assert_eq!(calls.count("x"), 0);
}

#[tokio::test]
async fn a_plugin_switched_off_just_before_the_session_closes_runs_no_session_end_hook() {
    let calls = Calls::default();
    let agent = with_sections(
        plain(Fake::new(Vec::new())),
        vec![source(
            "aa",
            &hook_body("bye", "session-end", ""),
            kit::none(),
            &calls,
        )],
    )
    .await;
    let (commands, events, _task) = spawn(agent);
    let mut seen = Seen::new(events);
    commands
        .send(plugin_switch(1, "aa", ENABLED_KEY, false))
        .await
        .unwrap();
    seen.answered(1).await;
    seen.close(commands).await;
    assert_eq!(calls.count("bye"), 0);
}

#[tokio::test]
async fn a_dependent_hook_goes_with_the_plugin_it_requires_and_comes_back_with_it() {
    let calls = Calls::default();
    let provider = Fake::new(vec![ends("one"), ends("two"), ends("three")]);
    let sources = vec![
        source(
            "aa",
            &hook_body("x", "before-turn", ""),
            kit::append("X\n"),
            &calls,
        ),
        source(
            "bb",
            &hook_body("y", "before-turn", "requires = [\"aa/x\"]\n"),
            kit::append("Y\n"),
            &calls,
        ),
    ];
    let agent = with_sections(plain(provider.clone()), sources).await;
    let (commands, events, _task) = spawn(agent);
    let mut seen = Seen::new(events);
    commands.send(ClientCommand::send("m1", "a")).await.unwrap();
    seen.ended(1).await;
    commands
        .send(plugin_switch(1, "aa", ENABLED_KEY, false))
        .await
        .unwrap();
    seen.answered(1).await;
    commands.send(ClientCommand::send("m2", "b")).await.unwrap();
    seen.ended(2).await;
    commands
        .send(plugin_switch(2, "aa", ENABLED_KEY, true))
        .await
        .unwrap();
    seen.answered(2).await;
    commands.send(ClientCommand::send("m3", "c")).await.unwrap();
    seen.ended(3).await;
    seen.close(commands).await;
    assert_eq!((calls.count("x"), calls.count("y")), (2, 2));
    let requests = provider.sent();
    let last = |at: usize| texts(requests[at].messages.last().unwrap()).join("");
    assert!(last(0).contains("X\n") && last(0).contains("Y\n"));
    assert!(!last(1).contains("X\n") && !last(1).contains("Y\n"));
    assert!(last(2).contains("X\n") && last(2).contains("Y\n"));
}

#[test]
fn a_plugin_switches_seed_names_off_and_removed_ids_in_the_session_layer() {
    let switches = PluginSwitches {
        off: ["aa".to_owned()].into(),
        removed: ["bb".to_owned()].into(),
    };
    let agent = plain(Fake::new(Vec::new())).with_plugin_switches(&switches);
    let seeded: Vec<(Target, SettingValue)> = agent
        .settings
        .session
        .iter()
        .map(|g| (g.target.clone(), g.value.clone()))
        .collect();
    let off = |id: &str, key: &str| {
        (
            Target::Path(crate::plugin::testing::plugin_path(id, key)),
            SettingValue::Bool(false),
        )
    };
    assert_eq!(seeded, [off("aa", ENABLED_KEY), off("bb", INSTALLED_KEY)]);
}

#[tokio::test]
async fn both_switches_read_the_session_then_the_flags_then_the_environment_then_the_saved_config()
{
    use crate::settings::{Given, Layer};
    let cases = [
        ([None, None, None, None], true, None),
        ([None, None, None, Some(false)], false, Some(Layer::Global)),
        (
            [None, None, Some(true), Some(false)],
            true,
            Some(Layer::Environment),
        ),
        (
            [None, Some(false), Some(true), Some(false)],
            false,
            Some(Layer::Flag),
        ),
        (
            [Some(true), Some(false), Some(true), Some(false)],
            true,
            Some(Layer::Session),
        ),
    ];
    for key in [ENABLED_KEY, INSTALLED_KEY] {
        let path = crate::plugin::testing::plugin_path("aa", key);
        let given = |on: Option<bool>| {
            on.map(|on| Given {
                target: Target::Path(path.clone()),
                value: SettingValue::Bool(on),
                origin: "test".to_owned(),
            })
            .into_iter()
            .collect::<Vec<_>>()
        };
        for ([session, flag, env, saved], on, layer) in cases {
            let mut layers = StartLayers {
                flags: given(flag),
                env: given(env),
                ..StartLayers::default()
            };
            if let Some(saved) = saved {
                layers
                    .saved
                    .insert(path.clone(), SettingValue::Bool(saved), 1);
            }
            let calls = Calls::default();
            let mut agent = with_sections(
                plain(Fake::new(Vec::new())).with_start_layers(layers),
                vec![source(
                    "aa",
                    &hook_body("x", "before-turn", ""),
                    kit::none(),
                    &calls,
                )],
            )
            .await;
            agent.settings.session.extend(given(session));
            agent.apply_switches();
            let case = format!("{key} {session:?} {flag:?} {env:?} {saved:?}");
            assert_eq!(!agent.plugin_off("aa"), on, "{case}");
            assert_eq!(
                agent.placed_value(&path).map(|(_, layer)| layer),
                layer,
                "{case}"
            );
            assert_eq!(
                agent.hooks.switches.removed.contains("aa"),
                key == INSTALLED_KEY && !on,
                "{case}"
            );
        }
    }
}

#[tokio::test]
async fn a_plugin_off_under_a_flag_is_placed_off_and_its_catalog_entry_says_so_from_the_start() {
    let agent = flagged(
        plain(Fake::new(Vec::new())),
        vec![plugin_flag("context", ENABLED_KEY, false)],
    );
    let mut agent = catalogued(agent, Vec::new()).await;
    agent.start_switches();
    let placed = agent.placed();
    let paths: Vec<String> = placed.iter().map(|p| p.path.to_string()).collect();
    let mut sorted = paths.clone();
    sorted.sort();
    assert_eq!(paths, sorted);
    let context = placed
        .iter()
        .find(|p| p.path.to_string() == "plugins.context.enabled")
        .unwrap();
    assert_eq!(context.value, Some(SettingValue::Bool(false)));
    assert_eq!(context.layer, wisp_protocol::settings::Layer::Flag);
    let time = placed
        .iter()
        .find(|p| p.path.to_string() == "plugins.time.installed")
        .unwrap();
    assert_eq!(time.value, Some(SettingValue::Bool(true)));
    assert_eq!(time.layer, wisp_protocol::settings::Layer::Default);
    let entry = agent
        .catalog()
        .plugins
        .iter()
        .find(|p| p.id == "context")
        .unwrap();
    assert_eq!((entry.installed, entry.enabled), (true, false));
}

fn removed(ids: &[&str]) -> PluginSwitches {
    PluginSwitches {
        off: BTreeSet::new(),
        removed: ids.iter().map(|id| (*id).to_owned()).collect(),
    }
}

#[tokio::test]
async fn an_agent_started_without_a_plugin_hands_its_removal_on_to_its_children() {
    let calls = Calls::default();
    let switches = PluginSwitches {
        off: ["bb".to_owned()].into(),
        removed: ["aa".to_owned()].into(),
    };
    let mut agent = started_as(
        plain(Fake::new(Vec::new())),
        vec![
            commanded(&hook_body("x", "before-turn", ""), &calls),
            source(
                "bb",
                &hook_body("y", "before-turn", ""),
                kit::none(),
                &calls,
            ),
        ],
        Start::Child(switches.clone()),
    )
    .await;
    agent.start_switches();
    assert_eq!(agent.child_switches(), switches);
    let aa = agent.catalog.plugins.iter().find(|p| p.id == "aa").unwrap();
    assert_eq!((aa.installed, aa.enabled), (false, false));
}

#[tokio::test]
async fn a_command_of_a_plugin_removed_at_start_is_refused_as_removed_by_name_and_alias() {
    let hint = "aa is removed; add it back in /config";
    for start in [
        Start::Root(removed(&["aa"])),
        Start::Child(removed(&["aa"])),
    ] {
        let calls = Calls::default();
        let make: Make = kit::commanding(kit::none(), named(), calls.clone());
        let body = kit::command("go", "aliases = [\"g\"]\n");
        let agent = started_as(
            plain(Fake::new(Vec::new())),
            vec![(kit::manifest_in("0.2.0", "aa", &body), make)],
            start,
        )
        .await;
        let (commands, events, _task) = spawn(agent);
        let mut seen = Seen::new(events);
        commands.send(go(1)).await.unwrap();
        commands
            .send(ClientCommand::RunPluginCommand {
                name: "aa:g".to_owned(),
                args: wisp_protocol::Secret::new(String::new()),
                request: RequestId(2),
            })
            .await
            .unwrap();
        commands
            .send(ClientCommand::RunPluginCommand {
                name: "go".to_owned(),
                args: wisp_protocol::Secret::new(String::new()),
                request: RequestId(3),
            })
            .await
            .unwrap();
        for request in [1, 2] {
            assert_eq!(
                seen.command(request).await,
                CommandOutcome::Refused {
                    kind: CommandRefusal::PluginOff,
                    hint: hint.to_owned(),
                }
            );
        }
        assert!(matches!(
            seen.command(3).await,
            CommandOutcome::Refused {
                kind: CommandRefusal::Unknown,
                ..
            }
        ));
        seen.close(commands).await;
        assert_eq!(calls.instances(), 0);
    }
}

#[tokio::test]
async fn a_plugin_left_out_at_start_stays_installed_and_enabled_in_the_applied_catalog() {
    let make: Make = Arc::new(|_| panic!("left out"));
    let mut agent = catalogued(
        plain(Fake::new(Vec::new())),
        vec![(
            kit::manifest("aa", &hook_body("x", "before-turn", "")),
            make,
        )],
    )
    .await;
    agent.start_switches();
    let aa = agent.catalog.plugins.iter().find(|p| p.id == "aa").unwrap();
    assert!(!aa.problems.is_empty(), "{aa:?}");
    assert_eq!((aa.installed, aa.enabled), (true, true));
    assert!(!agent.plugin_off("aa"));
}

fn entry_of(catalog: &wisp_protocol::Catalog, name: &str) -> wisp_protocol::plugin::HookEntry {
    catalog
        .plugins
        .iter()
        .flat_map(|p| &p.hooks)
        .find(|h| h.hook == name)
        .cloned()
        .unwrap_or_else(|| panic!("no entry {name} in {catalog:#?}"))
}

fn reasoned() -> Vec<(String, Make)> {
    let calls = Calls::default();
    let owned = "settings = [\"time\"]\n".to_owned()
        + &hook_body("x", "before-turn", "")
        + &hook_body("sw", "before-turn", "")
        + &hook_body("owned", "before-turn", "switch = \"time\"\n")
        + &hook_body("loop", "before-turn", "after = [\"bb/loop\"]\n")
        + &hook_body("own", "before-turn", "requires = [\"zz/gone\"]\n");
    let left_out: Make = Arc::new(|_| panic!("left out"));
    vec![
        source("aa", &owned, kit::none(), &calls),
        source(
            "bb",
            &hook_body("loop", "before-turn", "after = [\"aa/loop\"]\n"),
            kit::none(),
            &calls,
        ),
        source(
            "cc",
            &hook_body("base", "before-turn", ""),
            kit::none(),
            &calls,
        ),
        source(
            "dd",
            &hook_body("needs", "before-turn", "requires = [\"cc/base\"]\n"),
            kit::none(),
            &calls,
        ),
        (
            kit::manifest("ee", &hook_body("h", "before-turn", "")),
            left_out,
        ),
    ]
}

#[tokio::test]
async fn each_reason_a_hook_is_off_has_its_catalog_entry_and_a_left_out_one_stays_left_out() {
    use wisp_protocol::plugin::HookDisabled;
    let mut session = quiet();
    session.message_time = wisp_protocol::MessageTime::Off;
    let agent = flagged(
        Agent::new(Fake::new(Vec::new()), session),
        vec![
            plugin_flag("cc", ENABLED_KEY, false),
            plugin_flag("ee", ENABLED_KEY, false),
            hook_flag("aa", "sw", false),
        ],
    );
    let mut all = crate::plugin::testing::turn_line_sources();
    all.extend(reasoned());
    let (registry, _) = Registry::from_sources(all, &PluginConfig::default());
    let started = registry
        .start(&BTreeSet::new(), Arc::new(KernelHost::new()))
        .await;
    let mut agent = agent
        .with_catalog(started.catalog)
        .with_hooks(started.hooks, started.host);
    agent.start_switches();
    let catalog = agent.catalog().clone();
    let reason = |name: &str| {
        let entry = entry_of(&catalog, name);
        assert_eq!(entry.on, entry.disabled.is_none(), "{entry:?}");
        entry.disabled
    };
    assert_eq!(reason("aa/x"), None);
    assert_eq!(reason("ee/h"), Some(HookDisabled::LeftOut));
    assert!(matches!(
        reason("aa/loop"),
        Some(HookDisabled::Cycle { .. })
    ));
    assert!(matches!(
        reason("bb/loop"),
        Some(HookDisabled::Cycle { .. })
    ));
    assert_eq!(
        reason("aa/own"),
        Some(HookDisabled::Missing {
            requires: vec!["zz/gone".into()]
        })
    );
    assert_eq!(
        reason("dd/needs"),
        Some(HookDisabled::Missing {
            requires: vec!["cc/base".into()]
        })
    );
    assert_eq!(reason("cc/base"), Some(HookDisabled::PluginOff));
    assert_eq!(reason("aa/sw"), Some(HookDisabled::SwitchedOff));
    assert_eq!(reason("aa/owned"), Some(HookDisabled::SwitchedOff));
    assert_eq!(reason("time/sent"), Some(HookDisabled::SwitchedOff));
    assert_eq!(
        entry_of(&catalog, "aa/owned").switch.as_deref(),
        Some("time")
    );
    assert_eq!(entry_of(&catalog, "aa/sw").switch, None);
    let placed: Vec<String> = agent
        .placed()
        .iter()
        .filter(|p| p.path.to_string().contains(".hooks."))
        .map(|p| p.path.to_string())
        .collect();
    assert!(
        placed.contains(&"plugins.aa.hooks.sw".to_owned()),
        "{placed:?}"
    );
    assert!(
        !placed
            .iter()
            .any(|p| p.contains("owned") || p.contains("time.hooks")),
        "{placed:?}"
    );
    assert!(!placed.iter().any(|p| p.contains("ee.hooks")), "{placed:?}");
}

#[tokio::test]
async fn a_hook_switch_sends_one_catalog_change_at_the_moment() {
    use wisp_protocol::plugin::HookDisabled;
    let calls = Calls::default();
    let agent = catalogued(
        plain(Fake::new(vec![ends("one")])),
        vec![source(
            "aa",
            &(hook_body("x", "before-turn", "") + &hook_body("u", "user-input", "")),
            kit::none(),
            &calls,
        )],
    )
    .await;
    let (commands, events, _task) = spawn(agent);
    let mut seen = Seen::new(events);
    seen.until(|e| matches!(e, AgentEvent::CatalogChanged { .. }))
        .await;
    commands.send(switch(1, "aa", "x", false)).await.unwrap();
    let answer = seen.answered(1).await;
    commands.send(ClientCommand::send("m", "go")).await.unwrap();
    let started = seen
        .until(|e| matches!(e, AgentEvent::TurnStarted { .. }))
        .await;
    seen.ended(1).await;
    let seen = seen.close(commands).await;
    let changed: Vec<(usize, wisp_protocol::plugin::HookEntry)> = seen
        .iter()
        .enumerate()
        .filter_map(|(at, e)| match e {
            AgentEvent::CatalogChanged { catalog } => Some((at, entry_of(catalog, "aa/x"))),
            _ => None,
        })
        .collect();
    assert_eq!(changed.len(), 2, "{changed:#?}");
    assert!(changed[0].1.on);
    let (at, entry) = &changed[1];
    let input = input_ran(&seen);
    assert!(
        answer < *at && *at < input && input < started,
        "{answer} {at} {input} {started}"
    );
    assert_eq!(entry.disabled, Some(HookDisabled::SwitchedOff));
    assert_eq!(calls.count("x"), 0);
}

#[tokio::test]
async fn an_owned_switch_changes_the_entries_after_the_turn_changes_not_at_the_moment() {
    use wisp_protocol::plugin::HookDisabled;
    let calls = Calls::default();
    let agent = catalogued(
        plain(Fake::new(vec![ends("one")])),
        vec![source(
            "aa",
            &hook_body("u", "user-input", ""),
            kit::none(),
            &calls,
        )],
    )
    .await;
    let (commands, events, _task) = spawn(agent);
    let mut seen = Seen::new(events);
    seen.until(|e| matches!(e, AgentEvent::CatalogChanged { .. }))
        .await;
    commands
        .send(ClientCommand::ChangeSettings {
            request: RequestId(1),
            changes: vec![SettingChange::named("time", SettingValue::Text("off".into())).unwrap()],
            save: false,
        })
        .await
        .unwrap();
    seen.answered(1).await;
    commands.send(ClientCommand::send("m", "go")).await.unwrap();
    let started = seen
        .until(|e| matches!(e, AgentEvent::TurnStarted { .. }))
        .await;
    let ended = seen.ended(1).await;
    let seen = seen.close(commands).await;
    let changed: Vec<(usize, wisp_protocol::plugin::HookEntry)> = seen
        .iter()
        .enumerate()
        .filter_map(|(at, e)| match e {
            AgentEvent::CatalogChanged { catalog } => Some((at, entry_of(catalog, "time/sent"))),
            _ => None,
        })
        .collect();
    assert_eq!(changed.len(), 2, "{changed:#?}");
    assert!(changed[0].1.on);
    let (at, entry) = &changed[1];
    let input = input_ran(&seen);
    assert!(
        input < *at && *at < started && started < ended,
        "{input} {at} {started} {ended}"
    );
    assert_eq!(entry.disabled, Some(HookDisabled::SwitchedOff));
}

#[tokio::test]
async fn placed_holds_a_switchable_hooks_value_from_each_layer() {
    use crate::settings::{Given, Layer};
    let path = SitePath::plugin_hook("aa", "x").unwrap();
    let given = |on: Option<bool>| {
        on.map(|on| Given {
            target: Target::Path(path.clone()),
            value: SettingValue::Bool(on),
            origin: "test".to_owned(),
        })
        .into_iter()
        .collect::<Vec<_>>()
    };
    let cases = [
        ([None, None, None, None], true, Layer::Default),
        ([None, None, None, Some(false)], false, Layer::Global),
        (
            [None, None, Some(true), Some(false)],
            true,
            Layer::Environment,
        ),
        (
            [None, Some(false), Some(true), Some(false)],
            false,
            Layer::Flag,
        ),
        (
            [Some(true), Some(false), Some(true), Some(false)],
            true,
            Layer::Session,
        ),
    ];
    for ([session, flag, env, saved], on, layer) in cases {
        let mut layers = StartLayers {
            flags: given(flag),
            env: given(env),
            ..StartLayers::default()
        };
        if let Some(saved) = saved {
            layers
                .saved
                .insert(path.clone(), SettingValue::Bool(saved), 1);
        }
        let calls = Calls::default();
        let mut agent = catalogued(
            plain(Fake::new(Vec::new())).with_start_layers(layers),
            vec![source(
                "aa",
                &hook_body("x", "before-turn", ""),
                kit::none(),
                &calls,
            )],
        )
        .await;
        agent.settings.session.extend(given(session));
        agent.apply_switches();
        let placed = agent.placed();
        let found = placed.iter().find(|p| p.path == path).unwrap();
        let case = format!("{session:?} {flag:?} {env:?} {saved:?}");
        assert_eq!(found.value, Some(SettingValue::Bool(on)), "{case}");
        assert_eq!(found.layer, layer.wire(), "{case}");
    }
}

fn input_ran(seen: &[AgentEvent]) -> usize {
    seen.iter()
        .position(|e| matches!(e, AgentEvent::HookRan { point, .. } if point == "user-input"))
        .unwrap_or_else(|| panic!("no user-input run in {seen:#?}"))
}

#[tokio::test]
async fn a_plugin_removed_mid_session_ends_and_leaves_and_comes_back_started_as_resumed() {
    let calls = Calls::default();
    let provider = Fake::new(vec![ends("one"), ends("two"), ends("three")]);
    let body = hook_body("s", "session-start", "") + &hook_body("e", "session-end", "");
    let agent = catalogued(
        plain(provider),
        vec![source("aa", &body, kit::none(), &calls)],
    )
    .await;
    let (commands, events, _task) = spawn(agent);
    let mut seen = Seen::new(events);
    for (n, on) in [(1, None), (2, Some(false)), (3, Some(true))] {
        if let Some(on) = on {
            commands
                .send(plugin_switch(n, "aa", INSTALLED_KEY, on))
                .await
                .unwrap();
            seen.answered(n).await;
        }
        let id = format!("m{n}");
        commands.send(ClientCommand::send(&id, "go")).await.unwrap();
        seen.ended(usize::try_from(n).unwrap()).await;
    }
    let seen = seen.close(commands).await;
    let none: Vec<String> = Vec::new();
    assert_eq!(
        plugins_changed(&seen),
        [
            (none.clone(), vec!["aa".to_owned()], none.clone()),
            (vec!["aa".to_owned()], none.clone(), none)
        ]
    );
    let ran: Vec<(usize, String, Option<bool>)> = calls
        .all()
        .into_iter()
        .map(|c| {
            let resumed = match c.event {
                Event::SessionStart(start) => Some(start.resumed),
                _ => None,
            };
            (c.instance, c.hook, resumed)
        })
        .collect();
    assert_eq!(
        ran,
        [
            (1, "s".to_owned(), Some(false)),
            (1, "e".to_owned(), None),
            (2, "s".to_owned(), Some(true)),
            (2, "e".to_owned(), None),
        ]
    );
    let catalog = seen
        .iter()
        .rev()
        .find_map(|e| match e {
            AgentEvent::CatalogChanged { catalog } => catalog.plugins.iter().find(|p| p.id == "aa"),
            _ => None,
        })
        .unwrap();
    assert!(catalog.installed && catalog.enabled);
}

#[tokio::test]
async fn a_child_spawned_after_an_add_back_starts_the_plugin_and_a_running_child_keeps_its_set() {
    let (calls, child_calls) = (Calls::default(), Calls::default());
    let sources = |calls: &Calls| {
        vec![source(
            "aa",
            &hook_body("e", "session-end", ""),
            kit::none(),
            calls,
        )]
    };
    let root = flagged(
        plain(Fake::new(vec![ends("one")])),
        vec![plugin_flag("aa", INSTALLED_KEY, false)],
    );
    let root = started_as(root, sources(&calls), Start::Root(removed(&["aa"]))).await;
    let start = root.hooks.registry.clone().unwrap();
    let (commands, mut inbox) = mpsc::channel(16);
    let (sink, events) = mpsc::channel(256);
    let task = tokio::spawn(async move {
        let mut root = root;
        root.serve(&mut inbox, &sink).await;
        root
    });
    let mut seen = Seen::new(events);
    commands
        .send(plugin_switch(1, "aa", INSTALLED_KEY, true))
        .await
        .unwrap();
    seen.answered(1).await;
    commands.send(ClientCommand::send("m", "go")).await.unwrap();
    seen.ended(1).await;
    let root = {
        drop(commands);
        while seen.events.recv().await.is_some() {}
        task.await.unwrap()
    };
    let switches = root.child_switches();
    assert!(!switches.removed.contains("aa"), "{switches:?}");
    let child = start
        .start_child(&BTreeSet::new(), &switches, Arc::new(KernelHost::new()))
        .await;
    assert!(child.hooks.plugins.contains_key("aa"));
    let child = plain(Fake::new(vec![ends("two")])).with_parent(wisp_protocol::ParentLink {
        session_id: "parent".into(),
        agent: wisp_protocol::AgentPath::root(),
        depth: 1,
        config: Default::default(),
        todos: Vec::new(),
    });
    let child = started_as(
        child,
        sources(&child_calls),
        Start::Child(PluginSwitches::default()),
    )
    .await;
    let (commands, events, _task) = spawn(child);
    let mut seen = Seen::new(events);
    commands
        .send(plugin_switch(1, "aa", INSTALLED_KEY, false))
        .await
        .unwrap();
    seen.answered(1).await;
    commands.send(ClientCommand::send("m", "go")).await.unwrap();
    seen.ended(1).await;
    let seen = seen.close(commands).await;
    assert!(
        plugins_changed(&seen).is_empty(),
        "{:?}",
        plugins_changed(&seen)
    );
    assert_eq!(child_calls.count("e"), 0);
}

#[tokio::test(start_paused = true)]
async fn leaving_plugins_settle_their_background_session_end_under_one_budget_before_the_cut() {
    let calls = Calls::default();
    let started = Arc::new(Notify::new());
    let short: HookScript = Arc::new(|_, _, _| {
        Box::pin(async {
            tokio::time::sleep(Duration::from_millis(500)).await;
            Reply::None
        })
    });
    let background = "background = true\ntimeout_ms = 60000\n";
    let sources = vec![
        source(
            "aa",
            &hook_body("bye", "session-end", background),
            short,
            &calls,
        ),
        source(
            "bb",
            &hook_body("bye", "session-end", background),
            forever(Arc::new(Notify::new())),
            &calls,
        ),
        source(
            "cc",
            &hook_body("slow", "after-turn", background),
            forever(started.clone()),
            &calls,
        ),
    ];
    let provider = Fake::new(vec![ends("one"), ends("two")]);
    let agent = catalogued(plain(provider), sources).await;
    let (commands, events, _task) = spawn(agent);
    let mut seen = Seen::new(events);
    commands.send(ClientCommand::send("a", "a")).await.unwrap();
    seen.ended(1).await;
    within("the hook's start", started.notified()).await;
    for (n, id) in [(1, "aa"), (2, "bb")] {
        commands
            .send(plugin_switch(n, id, INSTALLED_KEY, false))
            .await
            .unwrap();
        seen.answered(n).await;
    }
    let sent = tokio::time::Instant::now();
    commands.send(ClientCommand::send("b", "b")).await.unwrap();
    let turn = seen
        .until(|e| matches!(e, AgentEvent::TurnStarted { turn: 2, .. }))
        .await;
    let took = sent.elapsed();
    assert!(took >= Duration::from_millis(1_900), "{took:?}");
    assert!(took <= Duration::from_millis(2_100), "{took:?}");
    let before: Vec<(String, String, HookOutcome)> = ran(&seen.all[..turn]);
    assert!(
        before.contains(&run("aa/bye", "session-end", HookOutcome::None)),
        "{before:?}"
    );
    assert!(
        before.contains(&run("bb/bye", "session-end", HookOutcome::Cancelled)),
        "{before:?}"
    );
    assert!(
        !before.iter().any(|(hook, _, _)| hook == "cc/slow"),
        "{before:?}"
    );
    seen.ended(2).await;
    let seen = seen.close(commands).await;
    assert!(ran(&seen).contains(&run("cc/slow", "after-turn", HookOutcome::Cancelled)));
}
