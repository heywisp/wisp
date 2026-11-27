use std::collections::BTreeSet;
use std::time::Duration;

use tokio::sync::Notify;
use wisp_plugin_api::hooks::{Event, Reply};
use wisp_plugin_testkit::hooks::{self as kit, Calls, Script as HookScript};
use wisp_protocol::log::{LEAD_ENVIRONMENT, LEAD_PROCESSES, LogEvent};
use wisp_protocol::settings::{
    PLUGIN_HOOK, Place, SettingChange, SettingTarget, SettingValue, SitePath, Target, find,
};
use wisp_protocol::{HookOutcome, RequestId};

use super::*;
use crate::agent::tool_tests::terminal;
use crate::plugin::hooks::Name;
use crate::plugin::testing::hook_body;
use crate::plugin::{KernelHost, Make, PluginConfig, Registry};
use crate::test_guard::until_event;

fn source(id: &str, body: &str, script: HookScript, calls: &Calls) -> (String, Make) {
    let make: Make = kit::instantiate(script, calls.clone());
    (kit::manifest(id, body), make)
}

fn quiet() -> SessionConfig {
    let mut session = default_session();
    session.context_note = false;
    session
}

fn plain(provider: Arc<Fake>) -> Agent {
    Agent::new(provider, quiet()).with_clock(TickingClock::new())
}

fn texts(message: &Message) -> Vec<&str> {
    message.content.iter().map(text_of).collect()
}

async fn settle() {
    for _ in 0..16 {
        tokio::task::yield_now().await;
    }
}

fn forever(started: Arc<Notify>) -> HookScript {
    gated(Arc::new(Notify::new()), started)
}

async fn cancel_when_started(
    agent: &mut Agent,
    started: Arc<Notify>,
) -> (TurnOutcome, Vec<AgentEvent>) {
    let (cmd_tx, mut cmd_rx) = mpsc::channel(4);
    let (ev_tx, mut ev_rx) = mpsc::channel(256);
    let canceller = tokio::spawn(async move {
        within("the hook's start", started.notified()).await;
        cmd_tx.send(ClientCommand::Cancel).await.unwrap();
        cmd_tx
    });
    let collector = tokio::spawn(async move {
        let mut events = Vec::new();
        while let Some(e) = ev_rx.recv().await {
            events.push(e);
        }
        events
    });
    let outcome = agent
        .run_turn("a".into(), &mut cmd_rx, &ev_tx)
        .await
        .unwrap();
    drop(ev_tx);
    drop(canceller.await.unwrap());
    (outcome, collector.await.unwrap())
}

fn ran(events: &[AgentEvent]) -> Vec<(String, String, HookOutcome)> {
    pick!(
        events,
        AgentEvent::HookRan {
            hook,
            point,
            outcome,
            ..
        } => (hook.clone(), point.clone(), outcome.clone())
    )
}

fn logged_runs(store: &MemoryStore) -> Vec<(String, String, HookOutcome)> {
    pick!(
        log_events(store),
        LogEvent::HookRan {
            hook,
            point,
            outcome,
            ..
        } => (hook, point, outcome)
    )
}

const TERMINAL: &str = "workspace/terminal";

const KEPT_WARM: &str = "agents/kept-warm";

fn run(hook: &str, point: &str, outcome: HookOutcome) -> (String, String, HookOutcome) {
    (hook.to_owned(), point.to_owned(), outcome)
}

#[tokio::test]
async fn a_before_turn_append_lands_at_its_place_in_the_new_user_message_only() {
    let provider = Fake::new(vec![ends("one"), ends("two")]);
    let calls = Calls::default();
    let sources = vec![
        source(
            "aa",
            &hook_body("x", "before-turn", ""),
            kit::append("X\n"),
            &calls,
        ),
        source(
            "bb",
            &hook_body("y", "before-turn", "after = [\"time/sent\"]\n"),
            kit::append("Y\n"),
            &calls,
        ),
    ];
    let mut agent = with_sections(plain(provider.clone()), sources).await;
    turn(&mut agent, "a").await;
    turn(&mut agent, "b").await;
    let requests = provider.sent();
    let first = &requests[0].messages[0];
    let stamp = texts(first)[1].to_owned();
    assert!(stamp.starts_with("Sent: "), "{stamp}");
    assert_eq!(texts(first), ["X\n", stamp.as_str(), "Y\n", "a"]);
    assert_eq!(requests[1].messages[0], *first);
    assert_eq!(texts(&requests[1].messages[2])[0], "X\n");
    assert_eq!(texts(&requests[1].messages[2])[3], "b");
    assert_eq!((calls.count("x"), calls.count("y")), (2, 2));
    let Event::BeforeTurn(event) = &calls.of("y")[0].event else {
        panic!();
    };
    let lead: Vec<&str> = event.lead.iter().map(|i| i.by.as_str()).collect();
    assert_eq!(lead, ["aa/x", "time/sent"]);
    assert_eq!(
        (event.turn, event.typed.as_str(), event.retry),
        (1, "a", false)
    );
}

#[tokio::test]
async fn hooks_at_other_points_leave_the_request_bytes_unchanged() {
    let script = || Fake::new(vec![ends("one"), ends("two")]);
    let bare = script();
    let mut agent = plain(bare.clone());
    turn(&mut agent, "a").await;
    turn(&mut agent, "b").await;
    let with = script();
    let calls = Calls::default();
    let body = hook_body("seen", "after-turn", "")
        + &hook_body("asked", "before-request", "")
        + &hook_body("heard", "after-response", "");
    let mut agent = with_sections(
        plain(with.clone()),
        vec![source("aa", &body, kit::none(), &calls)],
    )
    .await;
    turn(&mut agent, "a").await;
    turn(&mut agent, "b").await;
    assert_eq!(bare.texts(), with.texts());
    assert_eq!(calls.count("asked"), 2);
    assert_eq!(calls.count("heard"), 2);
}

#[tokio::test]
async fn esc_during_before_turn_sends_no_request_and_ends_the_turn_cancelled_once() {
    let provider = Fake::new(vec![ends("unused")]);
    let calls = Calls::default();
    let started = Arc::new(Notify::new());
    let sources = vec![source(
        "aa",
        &hook_body("x", "before-turn", ""),
        forever(started.clone()),
        &calls,
    )];
    let mut agent = with_sections(plain(provider.clone()), sources).await;
    let (outcome, events) = cancel_when_started(&mut agent, started).await;
    assert!(
        matches!(outcome, TurnOutcome::Cancelled { .. }),
        "{outcome:?}"
    );
    assert!(provider.sent().is_empty());
    assert!(
        events
            .iter()
            .any(|e| matches!(e, AgentEvent::TurnStarted { .. }))
    );
    assert_eq!(
        ran(&events),
        [
            run(LEAD_ENVIRONMENT, "before-turn", HookOutcome::None),
            run(TERMINAL, "before-turn", HookOutcome::None),
            run("aa/x", "before-turn", HookOutcome::Cancelled)
        ]
    );
    assert!(agent.history().is_empty());
}

#[tokio::test]
async fn esc_during_before_request_sends_no_request() {
    let provider = Fake::new(vec![ends("unused")]);
    let calls = Calls::default();
    let started = Arc::new(Notify::new());
    let sources = vec![source(
        "aa",
        &hook_body("x", "before-request", ""),
        forever(started.clone()),
        &calls,
    )];
    let mut agent = with_sections(plain(provider.clone()), sources).await;
    let (outcome, events) = cancel_when_started(&mut agent, started).await;
    assert!(
        matches!(outcome, TurnOutcome::Cancelled { .. }),
        "{outcome:?}"
    );
    assert!(provider.sent().is_empty());
    assert_eq!(
        ran(&events),
        [
            run(LEAD_ENVIRONMENT, "before-turn", HookOutcome::None),
            run(TERMINAL, "before-turn", HookOutcome::None),
            run("time/sent", "before-turn", HookOutcome::Applied),
            run(KEPT_WARM, "before-turn", HookOutcome::None),
            run(LEAD_PROCESSES, "before-turn", HookOutcome::None),
            run("aa/x", "before-request", HookOutcome::Cancelled)
        ]
    );
}

#[tokio::test]
async fn esc_during_after_response_on_a_reply_that_ended_the_turn_completes_it() {
    let provider = Fake::new(vec![ends("done")]);
    let calls = Calls::default();
    let started = Arc::new(Notify::new());
    let sources = vec![source(
        "aa",
        &hook_body("x", "after-response", ""),
        forever(started.clone()),
        &calls,
    )];
    let mut agent = with_sections(plain(provider.clone()), sources).await;
    let (outcome, events) = cancel_when_started(&mut agent, started).await;
    assert!(committed(&outcome), "{outcome:?}");
    assert_eq!(
        ran(&events),
        [
            run(LEAD_ENVIRONMENT, "before-turn", HookOutcome::None),
            run(TERMINAL, "before-turn", HookOutcome::None),
            run("time/sent", "before-turn", HookOutcome::Applied),
            run(KEPT_WARM, "before-turn", HookOutcome::None),
            run(LEAD_PROCESSES, "before-turn", HookOutcome::None),
            run("aa/x", "after-response", HookOutcome::Cancelled)
        ]
    );
    assert_eq!(agent.history().len(), 2);
}

#[tokio::test]
async fn esc_during_after_response_on_a_pause_turn_reply_without_calls_cancels_it() {
    let provider = Fake::new(vec![
        Step::from(reply("paused", 600, 0, StopReason::PauseTurn)),
        ends("unused"),
    ]);
    let calls = Calls::default();
    let started = Arc::new(Notify::new());
    let sources = vec![source(
        "aa",
        &hook_body("x", "after-response", ""),
        forever(started.clone()),
        &calls,
    )];
    let agent = with_sections(plain(provider.clone()), sources).await;
    let (commands, mut events, _task) = spawn(agent);
    commands.send(ClientCommand::send("a", "a")).await.unwrap();
    within("the hook's start", started.notified()).await;
    commands.send(ClientCommand::Cancel).await.unwrap();
    let mut seen = ended(&mut events).await;
    drop(commands);
    while let Some(e) = events.recv().await {
        seen.push(e);
    }
    let outcomes: Vec<&TurnOutcome> = pick!(
        &seen,
        AgentEvent::TurnEnded { outcome, .. } => outcome
    );
    assert!(
        matches!(outcomes.as_slice(), [TurnOutcome::Cancelled { .. }]),
        "{outcomes:?}"
    );
    assert_eq!(provider.sent().len(), 1);
}

#[tokio::test]
async fn a_mail_arriving_during_a_hook_is_queued_for_the_next_boundary() {
    let calls = Calls::default();
    let gate = Arc::new(Notify::new());
    let started = Arc::new(Notify::new());
    let provider = Fake::new(vec![ends("one"), ends("two")]);
    let sources = vec![source(
        "aa",
        &hook_body("held", "before-request", ""),
        held_once(gate.clone(), started.clone()),
        &calls,
    )];
    let agent = with_sections(plain(provider.clone()), sources).await;
    let mail = agent.mailbox.sender();
    let (commands, mut events, _task) = spawn(agent);
    commands.send(ClientCommand::send("a", "a")).await.unwrap();
    within("the hook's start", started.notified()).await;
    mail.send(super::mail::Mail {
        id: wisp_protocol::agents::MailId("1".into()),
        from: wisp_protocol::agents::AgentPath::root(),
        to: wisp_protocol::agents::AgentPath::root(),
        kind: wisp_protocol::agents::MailKind::Message,
        wake: true,
        text: "a letter".into(),
        checklist_spans: Vec::new(),
    })
    .unwrap();
    settle().await;
    gate.notify_one();
    until_event(&mut events, &mut Vec::new(), "the letter's request", |e| {
        matches!(e, AgentEvent::TurnEnded { .. }) && provider.sent().len() == 2
    })
    .await;
    drop(commands);
    let requests = provider.sent();
    let said: Vec<String> = requests
        .iter()
        .flat_map(|r| &r.messages)
        .filter(|m| m.role == wisp_protocol::Role::User)
        .flat_map(|m| m.content.iter().map(|b| text_of(b).to_owned()))
        .collect();
    assert!(said.iter().any(|t| t.contains("a letter")), "{said:?}");
}

#[tokio::test]
async fn a_point_fired_from_a_tool_call_logs_its_turn_step_and_call() {
    let calls = Calls::default();
    let body = "\n[[tools]]\nname = \"fire\"\n\n[[points]]\nname = \"x\"\nkind = \"observe\"\nversion = \"1\"\n".to_owned()
        + &hook_body("h", "aa/x", "");
    let make: Make = kit::firing(kit::none(), calls.clone());
    let mut sources = crate::plugin::testing::turn_line_sources();
    sources.push((kit::manifest("aa", &body), make));
    let (registry, problems) = Registry::from_sources(sources, &PluginConfig::default());
    assert!(problems.is_empty(), "{problems:?}");
    let started = registry
        .start(
            &BTreeSet::from([kit::FIRE.to_owned()]),
            Arc::new(KernelHost::new()),
        )
        .await;
    let fire = tool_call("t", kit::FIRE);
    let dir = std::env::temp_dir();
    let workspace = crate::workspace::Workspace {
        root: dir.clone(),
        home: None,
        temp: dir,
    };
    let mut agent = plain(Fake::new(vec![Step::from(fire), ends("done")]))
        .with_tools(started.toolbox, workspace)
        .with_hooks(started.hooks, started.host);
    let (outcome, events) = turn(&mut agent, "go").await;
    assert!(committed(&outcome), "{outcome:?}");
    assert_eq!(calls.count("h"), 1);
    let at: Vec<_> = events
        .iter()
        .filter_map(|e| match e {
            AgentEvent::HookRan {
                point,
                turn,
                step,
                call_id,
                ..
            } if point == "aa/x" => Some((*turn, *step, call_id.clone())),
            _ => None,
        })
        .collect();
    assert_eq!(at, [(Some(1), Some(1), Some("t".to_owned()))]);
}

#[tokio::test]
async fn every_run_is_logged_and_a_point_without_hooks_logs_none() {
    let store = MemoryStore::default();
    let calls = Calls::default();
    let body = hook_body("start", "session-start", "") + &hook_body("after", "after-turn", "");
    let agent = with_sections(
        logged(Fake::new(vec![ends("one")]), &store),
        vec![source("aa", &body, kit::none(), &calls)],
    )
    .await;
    let (commands, mut events, _task) = spawn(agent);
    commands.send(ClientCommand::send("a", "a")).await.unwrap();
    until_seen(
        &mut events,
        |e| matches!(e, AgentEvent::HookRan { point, .. } if point == "after-turn"),
    )
    .await;
    drop(commands);
    while events.recv().await.is_some() {}
    assert_eq!(
        logged_runs(&store),
        [
            run("aa/start", "session-start", HookOutcome::None),
            run(LEAD_ENVIRONMENT, "before-turn", HookOutcome::None),
            run(TERMINAL, "before-turn", HookOutcome::None),
            run("time/sent", "before-turn", HookOutcome::Applied),
            run("context/fill", "before-turn", HookOutcome::Applied),
            run(KEPT_WARM, "before-turn", HookOutcome::None),
            run(LEAD_PROCESSES, "before-turn", HookOutcome::None),
            run("aa/after", "after-turn", HookOutcome::None),
        ]
    );
    let kinds = kinds(&store);
    assert_eq!(kinds.last().unwrap(), "session_ended");
}

#[tokio::test]
async fn session_end_runs_before_the_end_and_a_background_hook_left_is_cancelled() {
    let store = MemoryStore::default();
    let calls = Calls::default();
    let started = Arc::new(Notify::new());
    let sources = vec![
        source(
            "aa",
            &hook_body("bye", "session-end", ""),
            kit::none(),
            &calls,
        ),
        source(
            "bb",
            &hook_body(
                "slow",
                "after-turn",
                "background = true\ntimeout_ms = 60000\n",
            ),
            forever(started.clone()),
            &calls,
        ),
    ];
    let agent = with_sections(logged(Fake::new(vec![ends("one")]), &store), sources).await;
    let (commands, mut events, _task) = spawn(agent);
    commands.send(ClientCommand::send("a", "a")).await.unwrap();
    within("the hook's start", started.notified()).await;
    drop(commands);
    while events.recv().await.is_some() {}
    let logged_events = log_events(&store);
    let place = |want: &dyn Fn(&LogEvent) -> bool| logged_events.iter().position(want);
    let bye = place(&|e| matches!(e, LogEvent::HookRan { hook, .. } if hook == "aa/bye")).unwrap();
    let slow = place(&|e| {
        matches!(
            e,
            LogEvent::HookRan { hook, outcome: HookOutcome::Cancelled, .. } if hook == "bb/slow"
        )
    })
    .unwrap();
    let ended = place(&|e| matches!(e, LogEvent::SessionEnded { .. })).unwrap();
    assert!(bye < ended && slow < ended, "{bye} {slow} {ended}");
}

#[tokio::test(start_paused = true)]
async fn a_quit_waits_for_background_hooks_within_session_ends_one_budget() {
    let calls = Calls::default();
    let started = Arc::new(Notify::new());
    let slow_bye: HookScript = Arc::new(|_, _, _| {
        Box::pin(async {
            tokio::time::sleep(Duration::from_millis(1_500)).await;
            Reply::None
        })
    });
    let sources = vec![
        source("aa", &hook_body("bye", "session-end", ""), slow_bye, &calls),
        source(
            "bb",
            &hook_body(
                "slow",
                "after-turn",
                "background = true\ntimeout_ms = 60000\n",
            ),
            forever(started.clone()),
            &calls,
        ),
    ];
    let agent = with_sections(plain(Fake::new(vec![ends("one")])), sources).await;
    let (commands, mut events, _task) = spawn(agent);
    commands.send(ClientCommand::send("a", "a")).await.unwrap();
    within("the hook's start", started.notified()).await;
    let quit = tokio::time::Instant::now();
    drop(commands);
    while events.recv().await.is_some() {}
    let took = quit.elapsed();
    assert!(took >= Duration::from_millis(1_500), "{took:?}");
    assert!(took <= Duration::from_millis(2_100), "{took:?}");
}

#[tokio::test]
async fn a_message_sent_during_session_start_starts_its_turn_when_the_point_ends() {
    let calls = Calls::default();
    let gate = Arc::new(Notify::new());
    let started = Arc::new(Notify::new());
    let sources = vec![source(
        "aa",
        &hook_body("start", "session-start", ""),
        gated(gate.clone(), started.clone()),
        &calls,
    )];
    let agent = with_sections(plain(Fake::new(vec![ends("one")])), sources).await;
    let (commands, mut events, _task) = spawn(agent);
    commands.send(ClientCommand::send("a", "a")).await.unwrap();
    within("the hook's start", started.notified()).await;
    commands.send(ClientCommand::Cancel).await.unwrap();
    settle().await;
    gate.notify_one();
    let seen = ended(&mut events).await;
    let hook_at = seen
        .iter()
        .position(|e| matches!(e, AgentEvent::HookRan { hook, .. } if hook == "aa/start"))
        .unwrap();
    let turn_at = seen
        .iter()
        .position(|e| matches!(e, AgentEvent::TurnStarted { .. }))
        .unwrap();
    assert!(hook_at < turn_at);
    assert_eq!(
        ran(&seen),
        [
            run("aa/start", "session-start", HookOutcome::None),
            run(LEAD_ENVIRONMENT, "before-turn", HookOutcome::None),
            run(TERMINAL, "before-turn", HookOutcome::None),
            run("time/sent", "before-turn", HookOutcome::Applied),
            run(KEPT_WARM, "before-turn", HookOutcome::None),
            run(LEAD_PROCESSES, "before-turn", HookOutcome::None)
        ]
    );
    assert_eq!(terminal(&seen), 1);
}

#[tokio::test(start_paused = true)]
async fn a_session_start_hook_declaring_ten_seconds_is_cut_at_two() {
    let calls = Calls::default();
    let sleeping: HookScript = Arc::new(|_, _, _| {
        Box::pin(async {
            tokio::time::sleep(Duration::from_secs(3600)).await;
            Reply::None
        })
    });
    let sources = vec![source(
        "aa",
        &hook_body("start", "session-start", "timeout_ms = 10000\n"),
        sleeping,
        &calls,
    )];
    let agent = with_sections(plain(Fake::new(vec![ends("one")])), sources).await;
    let (commands, mut events, _task) = spawn(agent);
    let ran = until_event(&mut events, &mut Vec::new(), "the hook run", |e| {
        matches!(e, AgentEvent::HookRan { .. })
    })
    .await;
    assert!(
        matches!(
            ran,
            AgentEvent::HookRan {
                outcome: HookOutcome::TimedOut,
                ms: 2_000,
                ..
            }
        ),
        "{ran:?}"
    );
    drop(commands);
}

fn hook_target(plugin: &str, hook: &str) -> SettingTarget {
    SettingTarget::Path(SitePath::new(
        Place::PluginHook {
            plugin: plugin.to_owned(),
            hook: hook.to_owned(),
        },
        find(PLUGIN_HOOK).unwrap(),
    ))
}

fn switch(request: u64, plugin: &str, hook: &str, on: bool) -> ClientCommand {
    ClientCommand::ChangeSettings {
        request: RequestId(request),
        changes: vec![SettingChange {
            target: hook_target(plugin, hook),
            value: Some(SettingValue::Bool(on)),
        }],
        save: false,
    }
}

async fn until_seen(
    events: &mut mpsc::Receiver<AgentEvent>,
    stop: impl Fn(&AgentEvent) -> bool,
) -> Vec<AgentEvent> {
    let mut seen = Vec::new();
    until_event(events, &mut seen, "the event", stop).await;
    seen
}

async fn ended(events: &mut mpsc::Receiver<AgentEvent>) -> Vec<AgentEvent> {
    let mut seen = Vec::new();
    until_turn_end(events, &mut seen).await;
    seen
}

#[tokio::test]
async fn a_hook_switched_off_skips_the_next_turn_and_an_unknown_hook_is_refused() {
    let calls = Calls::default();
    let sources = vec![source(
        "aa",
        &hook_body("x", "before-turn", ""),
        kit::append("X\n"),
        &calls,
    )];
    let provider = Fake::new(vec![ends("one"), ends("two")]);
    let agent = with_sections(plain(provider.clone()), sources).await;
    let (commands, mut events, _task) = spawn(agent);
    commands.send(ClientCommand::send("a", "a")).await.unwrap();
    ended(&mut events).await;
    commands.send(switch(1, "aa", "x", false)).await.unwrap();
    let answer = answered(&mut events, 1).await;
    assert!(
        matches!(answer, AgentEvent::SettingsChanged { .. }),
        "{answer:?}"
    );
    commands.send(switch(2, "aa", "nope", false)).await.unwrap();
    let AgentEvent::Rejected { reason, .. } = answered(&mut events, 2).await else {
        panic!();
    };
    assert!(
        reason.contains("aa/nope is no hook; hooks: aa/x"),
        "{reason}"
    );
    commands
        .send(switch(3, "time", "sent", false))
        .await
        .unwrap();
    let AgentEvent::Rejected { reason, .. } = answered(&mut events, 3).await else {
        panic!();
    };
    assert!(
        reason.contains("time/sent is switched by time; set that instead"),
        "{reason}"
    );
    commands.send(ClientCommand::send("b", "b")).await.unwrap();
    ended(&mut events).await;
    drop(commands);
    assert_eq!(calls.count("x"), 1);
    let requests = provider.sent();
    assert!(!texts(&requests[1].messages[2]).contains(&"X\n"));
}

#[tokio::test]
async fn a_change_made_during_a_hook_reports_settings_changed_once_after_the_point() {
    let calls = Calls::default();
    let order: Arc<std::sync::Mutex<Vec<&'static str>>> = Arc::default();
    let gate = Arc::new(Notify::new());
    let started = Arc::new(Notify::new());
    let held = {
        let order = order.clone();
        let gate = gate.clone();
        let started = started.clone();
        let script: HookScript = Arc::new(move |_, _, _| {
            let order = order.clone();
            let gate = gate.clone();
            let started = started.clone();
            Box::pin(async move {
                started.notify_one();
                gate.notified().await;
                order.lock().unwrap().push("before-request");
                Reply::None
            })
        });
        script
    };
    let noted = {
        let order = order.clone();
        let script: HookScript = Arc::new(move |_, _, _| {
            order.lock().unwrap().push("settings-changed");
            Box::pin(async { Reply::None })
        });
        script
    };
    let sources = vec![
        source("aa", &hook_body("held", "before-request", ""), held, &calls),
        source(
            "bb",
            &hook_body("noted", "settings-changed", ""),
            noted,
            &calls,
        ),
    ];
    let agent = with_sections(plain(Fake::new(vec![ends("one")])), sources).await;
    let (commands, mut events, _task) = spawn(agent);
    commands.send(ClientCommand::send("a", "a")).await.unwrap();
    within("the hook's start", started.notified()).await;
    commands
        .send(ClientCommand::ChangeSettings {
            request: RequestId(1),
            changes: vec![
                SettingChange::named("context.count_tokens", SettingValue::Bool(false)).unwrap(),
            ],
            save: false,
        })
        .await
        .unwrap();
    let answer = answered(&mut events, 1).await;
    assert!(
        matches!(answer, AgentEvent::SettingsChanged { .. }),
        "{answer:?}"
    );
    gate.notify_one();
    ended(&mut events).await;
    drop(commands);
    while events.recv().await.is_some() {}
    assert_eq!(
        *order.lock().unwrap(),
        ["before-request", "settings-changed"]
    );
    let Event::SettingsChanged(names) = &calls.of("noted")[0].event else {
        panic!();
    };
    assert_eq!(names, &["context.count_tokens".to_owned()]);
}

async fn change_with_a_settings_changed_hook(
    changes: Vec<SettingChange>,
) -> (Calls, Vec<AgentEvent>, MemoryStore) {
    let calls = Calls::default();
    let store = MemoryStore::default();
    let sources = vec![source(
        "bb",
        &hook_body("noted", "settings-changed", ""),
        kit::none(),
        &calls,
    )];
    let agent = with_sections(logged(Fake::new(vec![ends("one")]), &store), sources).await;
    let (commands, mut events, _task) = spawn(agent);
    commands
        .send(ClientCommand::ChangeSettings {
            request: RequestId(1),
            changes,
            save: false,
        })
        .await
        .unwrap();
    let answer = answered(&mut events, 1).await;
    assert!(
        matches!(answer, AgentEvent::SettingsChanged { .. }),
        "{answer:?}"
    );
    commands.send(ClientCommand::send("a", "a")).await.unwrap();
    drop(commands);
    let mut rest = Vec::new();
    while let Some(e) = events.recv().await {
        rest.push(e);
    }
    assert_eq!(terminal(&rest), 1);
    (calls, rest, store)
}

fn settings_runs(runs: Vec<(String, String, HookOutcome)>) -> Vec<(String, String, HookOutcome)> {
    runs.into_iter()
        .filter(|(_, point, _)| point == "settings-changed")
        .collect()
}

fn theme() -> SettingChange {
    SettingChange::named("display.theme", SettingValue::Text("light".into())).unwrap()
}

fn logged_changes(store: &MemoryStore) -> Vec<Vec<String>> {
    log_events(store)
        .into_iter()
        .filter_map(|e| match e {
            LogEvent::SettingsChanged { changes, .. } if !changes.is_empty() => {
                Some(changes.into_iter().map(|c| c.target.to_string()).collect())
            }
            _ => None,
        })
        .collect()
}

#[tokio::test]
async fn a_change_of_only_client_owned_settings_runs_no_settings_changed_hook() {
    let (calls, events, store) = change_with_a_settings_changed_hook(vec![theme()]).await;
    assert_eq!(calls.count("noted"), 0);
    assert!(settings_runs(ran(&events)).is_empty(), "{events:?}");
    assert!(settings_runs(logged_runs(&store)).is_empty());
    assert_eq!(logged_changes(&store), [["display.theme"]]);
}

#[tokio::test]
async fn a_fold_or_pin_change_runs_no_settings_changed_hook() {
    let list = |name: &str| {
        SettingChange::named(name, SettingValue::List(vec!["aa/sec".to_owned()])).unwrap()
    };
    let (calls, events, store) = change_with_a_settings_changed_hook(vec![
        list("display.sidebar_folded"),
        list("display.sidebar_pinned"),
    ])
    .await;
    assert_eq!(calls.count("noted"), 0);
    assert!(settings_runs(ran(&events)).is_empty(), "{events:?}");
    assert!(settings_runs(logged_runs(&store)).is_empty());
    assert_eq!(
        logged_changes(&store),
        [["display.sidebar_folded", "display.sidebar_pinned"]]
    );
}

#[tokio::test]
async fn a_mixed_change_runs_settings_changed_once_with_only_its_kernel_names() {
    let count = SettingChange::named("context.count_tokens", SettingValue::Bool(false)).unwrap();
    let (calls, events, store) = change_with_a_settings_changed_hook(vec![count, theme()]).await;
    assert_eq!(calls.count("noted"), 1);
    let Event::SettingsChanged(names) = &calls.of("noted")[0].event else {
        panic!();
    };
    assert_eq!(names, &["context.count_tokens".to_owned()]);
    assert_eq!(settings_runs(ran(&events)).len(), 1);
    assert_eq!(
        logged_changes(&store),
        [["context.count_tokens", "display.theme"]]
    );
}

#[tokio::test]
async fn hook_switches_replay_on_resume_under_a_flag_and_over_the_saved_config() {
    let row = crate::plugin::testing::declared(
        "resumed-row",
        wisp_protocol::plugin::RowKind::Int {
            min: 1,
            max: 60,
            default: 10,
        },
    );
    let store = MemoryStore::default();
    let calls = Calls::default();
    let body = hook_body("x", "after-turn", "");
    let agent = with_sections(
        logged(Fake::new(vec![ends("one")]), &store),
        vec![source("aa", &body, kit::none(), &calls)],
    )
    .await;
    let (commands, mut events, _task) = spawn(agent);
    commands.send(switch(1, "aa", "x", false)).await.unwrap();
    answered(&mut events, 1).await;
    commands
        .send(wisp_protocol::ClientCommand::ChangeSettings {
            request: RequestId(2),
            changes: vec![SettingChange::named(row.name, SettingValue::Int(25)).unwrap()],
            save: false,
        })
        .await
        .unwrap();
    answered(&mut events, 2).await;
    drop(commands);
    while events.recv().await.is_some() {}
    let logged = crate::resume::LoggedSession::read(&records(&store)).unwrap();
    let name = Name::new("aa", "x");
    let path = SitePath::new(
        Place::PluginHook {
            plugin: "aa".into(),
            hook: "x".into(),
        },
        find(PLUGIN_HOOK).unwrap(),
    );
    let fresh = |flags: Vec<crate::settings::Given>, saved: Option<bool>| {
        let mut layers = crate::settings::host::StartLayers {
            flags,
            ..Default::default()
        };
        if let Some(on) = saved {
            layers.saved.insert(path.clone(), SettingValue::Bool(on), 1);
        }
        let calls = calls.clone();
        let body = body.clone();
        async move {
            with_sections(
                plain(Fake::new(Vec::new())).with_start_layers(layers),
                vec![source("aa", &body, kit::none(), &calls)],
            )
            .await
        }
    };
    let mut saved_on = fresh(Vec::new(), Some(true)).await;
    saved_on.apply_switches();
    assert!(saved_on.hook_on(&name));
    saved_on.restore_kernel(&logged);
    let snapshot = saved_on.snapshot(RequestId(3), None);
    assert_eq!(
        snapshot
            .settings
            .iter()
            .find(|setting| setting.name.as_str() == row.name)
            .and_then(|setting| setting.value.as_ref()),
        Some(&SettingValue::Int(25))
    );
    assert!(saved_on.hook_on(&name));
    saved_on.apply_switches();
    assert!(!saved_on.hook_on(&name));
    let flag = crate::settings::Given {
        target: Target::Path(path.clone()),
        value: SettingValue::Bool(true),
        origin: "--set".into(),
    };
    let mut flagged = fresh(vec![flag], None).await;
    flagged.restore_kernel(&logged);
    flagged.apply_switches();
    assert!(flagged.hook_on(&name));
    flagged.settings.session.push(crate::settings::Given {
        target: Target::Path(path.clone()),
        value: SettingValue::Bool(false),
        origin: "session".into(),
    });
    assert!(flagged.hook_on(&name));
    flagged.apply_switches();
    assert!(!flagged.hook_on(&name));
    let mut saved_off = fresh(Vec::new(), Some(false)).await;
    saved_off.apply_switches();
    assert!(!saved_off.hook_on(&name));
}

#[tokio::test]
async fn a_hook_only_plugin_runs_and_reads_its_settings_as_the_turn_applied_them() {
    for (endpoint, shown) in [
        ("claude-api", "claude-api"),
        ("sk-ant-api03-CANARY0123456789", "(not shown)"),
        ("bad account", "(not shown)"),
    ] {
        let calls = Calls::default();
        let read: Arc<std::sync::Mutex<Vec<Option<String>>>> = Arc::default();
        let keep = read.clone();
        let script: HookScript = Arc::new(move |host, _, _| {
            assert_eq!(host.get("endpoint").as_deref(), Some(shown));
            keep.lock().unwrap().push(host.get("time"));
            Box::pin(async { Reply::None })
        });
        let body =
            "settings = [\"time\", \"endpoint\"]\n".to_owned() + &hook_body("x", "before-turn", "");
        let provider = Fake::new(vec![ends("one"), ends("two")]);
        let mut agent = plain(provider);
        agent.settings.session.push(crate::settings::Given {
            target: Target::Name(find("endpoint").unwrap()),
            value: SettingValue::Text(endpoint.to_owned()),
            origin: "session".into(),
        });
        assert_eq!(agent.owned_value("endpoint").as_deref(), Some(shown));
        let agent = with_sections(agent, vec![source("aa", &body, script, &calls)]).await;
        let (commands, mut events, _task) = spawn(agent);
        commands.send(ClientCommand::send("a", "a")).await.unwrap();
        ended(&mut events).await;
        commands
            .send(ClientCommand::ChangeSettings {
                request: RequestId(1),
                changes: vec![
                    SettingChange::named("time", SettingValue::Text("off".into())).unwrap(),
                ],
                save: false,
            })
            .await
            .unwrap();
        answered(&mut events, 1).await;
        commands.send(ClientCommand::send("b", "b")).await.unwrap();
        ended(&mut events).await;
        drop(commands);
        let read = read.lock().unwrap().clone();
        assert_eq!(read.len(), 2);
        assert_ne!(read[0], read[1]);
        assert_eq!(read[1].as_deref(), Some("off"));
    }
}

#[tokio::test]
async fn the_provisional_head_holds_each_on_hooks_reserve_in_order() {
    let calls = Calls::default();
    let body = hook_body("x", "before-turn", "reserve = \"RESERVED\\n\"\n");
    let agent = with_sections(
        plain(Fake::new(Vec::new())),
        vec![source("aa", &body, kit::none(), &calls)],
    )
    .await;
    let head: Vec<String> = agent
        .provisional_head()
        .iter()
        .map(|b| text_of(b).to_owned())
        .collect();
    assert_eq!(head.len(), 2, "{head:?}");
    assert_eq!(head[0], "RESERVED\n");
    assert!(head[1].starts_with("Sent: "), "{head:?}");
    agent
        .hooks
        .engine
        .set_off([Name::new("aa", "x")].into_iter().collect());
    assert_eq!(agent.provisional_head().len(), 1);
    assert_eq!(calls.all().len(), 0);
}

#[tokio::test]
async fn esc_and_a_rewind_during_an_after_turn_hook_open_no_turn_and_wait_for_the_point() {
    let calls = Calls::default();
    let gate = Arc::new(Notify::new());
    let started = Arc::new(Notify::new());
    let sources = vec![source(
        "aa",
        &hook_body("after", "after-turn", ""),
        gated(gate.clone(), started.clone()),
        &calls,
    )];
    let agent = with_sections(plain(Fake::new(vec![ends("one")])), sources).await;
    let (commands, mut events, _task) = spawn(agent);
    commands.send(ClientCommand::send("a", "a")).await.unwrap();
    within("the hook's start", started.notified()).await;
    commands.send(ClientCommand::Cancel).await.unwrap();
    commands
        .send(ClientCommand::Rewind {
            to: wisp_protocol::RewindPoint::Turn { turn: 1 },
            request: RequestId(7),
        })
        .await
        .unwrap();
    settle().await;
    gate.notify_one();
    let seen = until_seen(&mut events, |e| {
        matches!(
            e,
            AgentEvent::Rewound { .. }
                | AgentEvent::Rejected {
                    request: Some(RequestId(7)),
                    ..
                }
        )
    })
    .await;
    drop(commands);
    let hook_at = seen
        .iter()
        .position(|e| matches!(e, AgentEvent::HookRan { hook, .. } if hook == "aa/after"))
        .unwrap();
    assert_eq!(
        ran(&seen),
        [
            run(LEAD_ENVIRONMENT, "before-turn", HookOutcome::None),
            run(TERMINAL, "before-turn", HookOutcome::None),
            run("time/sent", "before-turn", HookOutcome::Applied),
            run(KEPT_WARM, "before-turn", HookOutcome::None),
            run(LEAD_PROCESSES, "before-turn", HookOutcome::None),
            run("aa/after", "after-turn", HookOutcome::None)
        ]
    );
    assert!(
        matches!(seen.last(), Some(AgentEvent::Rewound { .. })),
        "{:?}",
        seen.last()
    );
    let turns = seen
        .iter()
        .filter(|e| matches!(e, AgentEvent::TurnStarted { .. }))
        .count();
    assert_eq!(turns, 1);
    assert!(hook_at < seen.len() - 1);
}

#[tokio::test]
async fn a_steer_sent_during_a_hook_is_delivered_at_the_next_boundary() {
    let calls = Calls::default();
    let gate = Arc::new(Notify::new());
    let started = Arc::new(Notify::new());
    let script = held_once(gate.clone(), started.clone());
    let provider = Fake::new(vec![ends("one"), ends("two")]);
    let sources = vec![source(
        "aa",
        &hook_body("held", "before-request", ""),
        script,
        &calls,
    )];
    let agent = with_sections(plain(provider.clone()), sources).await;
    let (commands, mut events, _task) = spawn(agent);
    commands.send(ClientCommand::send("a", "a")).await.unwrap();
    within("the hook's start", started.notified()).await;
    commands
        .send(ClientCommand::send("b", "also this"))
        .await
        .unwrap();
    gate.notify_one();
    ended(&mut events).await;
    drop(commands);
    let requests = provider.sent();
    let said: Vec<String> = requests
        .iter()
        .flat_map(|r| &r.messages)
        .filter(|m| m.role == wisp_protocol::Role::User)
        .flat_map(|m| m.content.iter().map(|b| text_of(b).to_owned()))
        .collect();
    assert!(said.iter().any(|t| t == "also this"), "{said:?}");
}

type Read = (String, Option<String>);

fn held_once(gate: Arc<Notify>, started: Arc<Notify>) -> HookScript {
    let first = Arc::new(std::sync::atomic::AtomicBool::new(true));
    Arc::new(move |_, _, _| {
        let gate = gate.clone();
        let started = started.clone();
        let hold = first.swap(false, std::sync::atomic::Ordering::SeqCst);
        Box::pin(async move {
            if hold {
                started.notify_one();
                gate.notified().await;
            }
            Reply::None
        })
    })
}

#[tokio::test]
async fn esc_during_a_turn_start_settings_changed_still_reports_the_switch_once_after_the_turn() {
    for (endpoint, shown) in [
        ("claude-api", "claude-api"),
        ("sk-ant-api03-CANARY0123456789", "(not shown)"),
        ("bad account", "(not shown)"),
    ] {
        let calls = Calls::default();
        let started = Arc::new(Notify::new());
        let sources = vec![
            source(
                "aa",
                &hook_body("noted", "settings-changed", ""),
                forever(started.clone()),
                &calls,
            ),
            source(
                "bb",
                &(hook_body("switched", "model-switched", "")
                    + &hook_body("opened", "before-turn", "")),
                kit::none(),
                &calls,
            ),
        ];
        let provider = Fake::new(vec![ends("unused")]);
        let mut agent = plain(provider.clone());
        agent.connections[0].name = endpoint.to_owned();
        let agent = with_sections(agent, sources).await;
        let (commands, mut events, _task) = spawn(agent);
        commands
            .send(ClientCommand::ChangeSettings {
                request: RequestId(1),
                changes: vec![
                    SettingChange::named("model", SettingValue::Text("claude-sonnet-5-5".into()))
                        .unwrap(),
                ],
                save: false,
            })
            .await
            .unwrap();
        answered(&mut events, 1).await;
        commands.send(ClientCommand::send("a", "a")).await.unwrap();
        within("the hook's start", started.notified()).await;
        commands.send(ClientCommand::Cancel).await.unwrap();
        let mut seen = Vec::new();
        until_event(
            &mut events,
            &mut seen,
            "model-switched",
            |e| matches!(e, AgentEvent::HookRan { point, .. } if point == "model-switched"),
        )
        .await;
        drop(commands);
        assert!(provider.sent().is_empty());
        let ended_at = seen
            .iter()
            .position(|e| {
                matches!(
                    e,
                    AgentEvent::TurnEnded {
                        outcome: TurnOutcome::Cancelled { .. },
                        ..
                    }
                )
            })
            .unwrap();
        assert!(ended_at + 1 < seen.len());
        assert_eq!(calls.count("switched"), 1);
        assert_eq!(calls.count("opened"), 0);
        assert!(
            !seen
                .iter()
                .any(|e| matches!(e, AgentEvent::HookRan { point, .. } if point == "before-turn")),
            "{seen:?}"
        );
        assert!(seen.iter().any(|e| matches!(
            e,
            AgentEvent::HookRan { point, outcome: HookOutcome::Cancelled, .. } if point == "settings-changed"
        )));
        let Event::ModelSwitched(switch) = &calls.of("switched")[0].event else {
            panic!();
        };
        assert_eq!(switch.model, "claude-sonnet-5-5");
        assert_eq!(switch.endpoint, shown);
    }
}

#[tokio::test]
async fn a_change_queued_during_a_turn_leaves_the_plugin_rows_the_turn_applied() {
    let calls = Calls::default();
    let gate = Arc::new(Notify::new());
    let started = Arc::new(Notify::new());
    let read: Arc<std::sync::Mutex<Vec<Read>>> = Arc::default();
    let reading = {
        let read = read.clone();
        let gate = gate.clone();
        let started = started.clone();
        let script: HookScript = Arc::new(move |host, hook, _| {
            read.lock().unwrap().push((hook.clone(), host.get("time")));
            let gate = gate.clone();
            let started = started.clone();
            Box::pin(async move {
                if hook == "asked" {
                    started.notify_one();
                    gate.notified().await;
                }
                Reply::None
            })
        });
        script
    };
    let body = "settings = [\"time\"]\n".to_owned()
        + &hook_body("asked", "before-request", "")
        + &hook_body("heard", "after-response", "");
    let agent = with_sections(
        plain(Fake::new(vec![ends("one")])),
        vec![source("aa", &body, reading, &calls)],
    )
    .await;
    let (commands, mut events, _task) = spawn(agent);
    commands.send(ClientCommand::send("a", "a")).await.unwrap();
    within("the hook's start", started.notified()).await;
    commands
        .send(ClientCommand::ChangeSettings {
            request: RequestId(1),
            changes: vec![SettingChange::named("time", SettingValue::Text("off".into())).unwrap()],
            save: false,
        })
        .await
        .unwrap();
    answered(&mut events, 1).await;
    gate.notify_one();
    ended(&mut events).await;
    drop(commands);
    let read = read.lock().unwrap().clone();
    assert_eq!(read.len(), 2, "{read:?}");
    assert_eq!(read[0].1, read[1].1, "{read:?}");
    assert_ne!(read[1].1.as_deref(), Some("off"));
}

fn letter(text: &str) -> super::inbox::Input {
    super::inbox::Input {
        id: wisp_protocol::InputId("m1".into()),
        mode: wisp_protocol::DeliveryMode::Steer,
        text: text.to_owned(),
        images: Vec::new(),
        refs: Vec::new(),
        held: false,
        arrival: None,
        note: None,
        from: Some(super::inbox::Letter {
            from: wisp_protocol::agents::MailFrom {
                agent: wisp_protocol::agents::AgentPath::root(),
                kind: wisp_protocol::agents::MailKind::Message,
                mail: wisp_protocol::agents::MailId("1".into()),
                wake: true,
                plugin: None,
            },
            above: true,
            returned: false,
        }),
        checklist_spans: Vec::new(),
        revision_unlogged: false,
    }
}

async fn fill_on_a_mail_turn(text: &str) -> (u64, String) {
    let calls = Calls::default();
    let seen: Arc<std::sync::Mutex<Vec<u64>>> = Arc::default();
    let keep = seen.clone();
    let script: HookScript = Arc::new(move |host, _, _| {
        keep.lock().unwrap().push(host.current_fill().estimate);
        Box::pin(async { Reply::None })
    });
    let mut agent = with_sections(
        plain(Fake::new(vec![ends("one")])),
        vec![source(
            "aa",
            &hook_body("x", "before-turn", ""),
            script,
            &calls,
        )],
    )
    .await;
    let (_cmd_tx, mut cmd_rx) = mpsc::channel(4);
    let (ev_tx, mut ev_rx) = mpsc::channel(256);
    let drain = tokio::spawn(async move { while ev_rx.recv().await.is_some() {} });
    agent
        .run_inputs(
            vec![letter(text)],
            None,
            Some(TurnOrigin::Mail),
            &mut cmd_rx,
            &ev_tx,
        )
        .await;
    drop(ev_tx);
    drain.await.unwrap();
    let Event::BeforeTurn(turn) = &calls.of("x")[0].event else {
        panic!();
    };
    let estimate = seen.lock().unwrap()[0];
    (estimate, turn.typed.clone())
}

#[tokio::test]
async fn a_mail_opened_turn_keeps_mail_out_of_typed_but_in_the_fill_estimate() {
    let (short, typed) = fill_on_a_mail_turn("hi").await;
    assert_eq!(typed, "");
    let (long, typed) = fill_on_a_mail_turn(&"word ".repeat(1000)).await;
    assert_eq!(typed, "");
    assert!(long >= short + 900, "{short} {long}");
}

#[tokio::test]
async fn a_mail_opened_turns_context_line_counts_the_mail() {
    let provider = Fake::new(vec![ends("one")]);
    let mut agent = Agent::new(provider.clone(), default_session()).with_clock(TickingClock::new());
    let (_cmd_tx, mut cmd_rx) = mpsc::channel(4);
    let (ev_tx, mut ev_rx) = mpsc::channel(256);
    let drain = tokio::spawn(async move { while ev_rx.recv().await.is_some() {} });
    agent
        .run_inputs(
            vec![letter(&"word ".repeat(400))],
            None,
            Some(TurnOrigin::Mail),
            &mut cmd_rx,
            &ev_tx,
        )
        .await;
    drop(ev_tx);
    drain.await.unwrap();
    let requests = provider.sent();
    let lines: Vec<&str> = texts(&requests[0].messages[0])
        .into_iter()
        .filter(|t| t.starts_with("Context:"))
        .collect();
    assert_eq!(
        lines,
        ["Context: 833 of 1M tokens (0%). Wisp compacts at 850k.\n"]
    );
}

mod end;
mod input;
mod runs;
mod switches;
