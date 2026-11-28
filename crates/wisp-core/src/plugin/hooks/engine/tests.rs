use std::collections::BTreeSet;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, OnceLock, Weak};
use std::time::Duration;

use futures::FutureExt;
use tokio::time::Instant;
use tokio_util::sync::CancellationToken;
use wisp_plugin_api::hooks::{Event, FillInfo, Reply, SessionStart};
use wisp_plugin_api::host::Host;
use wisp_plugin_api::manifest::Threshold;
use wisp_plugin_api::prompt::{Item, News};
use wisp_plugin_api::tree::{StopInfo, TreeEvent};
use wisp_plugin_testkit::hooks::{self, Calls, Script, none};
use wisp_protocol::HookOutcome;
use wisp_protocol::log::LeadEntry;

use super::{Engine, Finished, Fire, Lead, Payload, Progress, Ran, What, reached};
use crate::plugin::caller::{At, Caller};
use crate::plugin::hooks::Name;
use crate::plugin::hooks::points::{
    AFTER_COMPACT, AFTER_TURN, AGENT_ENDED, BEFORE_TURN, OUTSIDE_BUDGET, REWOUND, SESSION_START,
};
use crate::plugin::testing::hook_body;
use crate::plugin::{KernelHost, Make, PluginConfig, Registry, panicked};
use crate::stack_alarm::{Timer, on_alarm_stack};

fn source(id: &str, body: &str, script: Script, calls: &Calls) -> (String, Make) {
    let make: Make = hooks::instantiate(script, calls.clone());
    (hooks::manifest(id, body), make)
}

async fn engine(sources: Vec<(String, Make)>) -> (Arc<Engine>, Finished) {
    let (registry, problems) = Registry::from_sources(sources, &PluginConfig::default());
    assert!(problems.is_empty(), "{problems:?}");
    let started = registry
        .start(&BTreeSet::new(), Arc::new(KernelHost::new()))
        .await;
    Engine::new(
        started.hooks.table,
        started.hooks.plugins,
        Some(started.host),
    )
}

fn lead() -> Lead {
    Lead {
        turn: 3,
        said: "hello".to_owned(),
        ..Lead::default()
    }
}

fn shell() -> (String, Make) {
    let bundled = wisp_plugin_shell::bundled();
    let make: Make = Arc::new(move |host| (bundled.instantiate)(host));
    (wisp_plugin_shell::MANIFEST.to_owned(), make)
}

fn news() -> News {
    News {
        wake: false,
        items: vec![Item::Typed(7)],
    }
}

fn proc_text() -> String {
    wisp_plugin_shell::notice(&news()).unwrap()
}

fn with_news(mut fire: Fire) -> Fire {
    if let What::Lead(lead) = &mut fire.what {
        lead.news = Some(news());
    }
    fire
}

fn before_turn() -> Fire {
    Fire::new(BEFORE_TURN, What::Lead(Box::new(lead())), At::default())
}

fn observe(point: &str) -> Fire {
    Fire::new(
        point,
        What::Event(Event::SessionStart(SessionStart { resumed: false })),
        At::default(),
    )
}

async fn fired(engine: &Arc<Engine>, fire: Fire) -> Progress {
    let mut progress = Progress::default();
    engine.fire(fire, &mut progress).await;
    progress
}

fn by(lead: &[LeadEntry]) -> Vec<(String, String)> {
    lead.iter()
        .map(|e| (e.by.clone(), e.text.clone()))
        .collect()
}

fn pair(by: &str, text: &str) -> (String, String) {
    (by.to_owned(), text.to_owned())
}

fn outcomes(records: &[Ran]) -> Vec<(String, HookOutcome)> {
    records
        .iter()
        .map(|r| (r.hook.clone(), r.outcome.clone()))
        .collect()
}

fn reply(reply: Reply) -> Script {
    Arc::new(move |_, _, _| {
        let reply = reply.clone();
        Box::pin(async move { reply })
    })
}

fn sleeping() -> Script {
    Arc::new(|_, _, _| {
        Box::pin(async {
            tokio::time::sleep(Duration::from_secs(3600)).await;
            Reply::None
        })
    })
}

#[tokio::test]
async fn hooks_run_in_order_and_each_sees_the_earlier_appends() {
    let calls = Calls::default();
    let body = hook_body("one", BEFORE_TURN, "")
        + &hook_body("two", BEFORE_TURN, "after = [\"aa/one\"]\n");
    let script: Script =
        Arc::new(|_, hook, _| Box::pin(async move { Reply::Append(vec![format!("{hook}\n")]) }));
    let (engine, _finished) = engine(vec![source("aa", &body, script, &calls), shell()]).await;
    let progress = fired(&engine, with_news(before_turn())).await;
    assert_eq!(
        by(&progress.lead),
        vec![
            pair("aa/one", "one\n"),
            pair("aa/two", "two\n"),
            pair("shell/processes", &proc_text()),
        ]
    );
    assert!(progress.worded);
    let seen = calls.of("two");
    let Event::BeforeTurn(event) = &seen[0].event else {
        panic!("{seen:?}");
    };
    let lead: Vec<&str> = event.lead.iter().map(|i| i.by.as_str()).collect();
    assert_eq!(lead, ["aa/one"]);
    assert_eq!((event.turn, event.typed.as_str()), (3, "hello"));
    assert_eq!(
        outcomes(&engine.take_records()),
        vec![
            ("aa/one".to_owned(), HookOutcome::Applied),
            ("aa/two".to_owned(), HookOutcome::Applied),
            ("shell/processes".to_owned(), HookOutcome::Applied),
        ]
    );
}

#[tokio::test]
async fn a_point_without_hooks_leads_with_nothing_and_records_nothing() {
    let calls = Calls::default();
    let body = hook_body("seen", AFTER_TURN, "");
    let (engine, _finished) = engine(vec![source("aa", &body, hooks::none(), &calls)]).await;
    let progress = fired(&engine, with_news(before_turn())).await;
    assert!(progress.lead.is_empty());
    assert!(!progress.worded);
    assert!(engine.take_records().is_empty());
    assert_eq!(calls.all().len(), 0);
    let progress = fired(&engine, with_news(before_turn()).without_hooks()).await;
    assert!(progress.lead.is_empty());
}

#[tokio::test]
async fn blank_over_cap_and_unexpected_replies_are_rejected_and_recorded() {
    let calls = Calls::default();
    let big = "x".repeat(super::points::MAX_APPEND_CHARS + 1);
    let sources = vec![
        source(
            "blank",
            &hook_body("h", BEFORE_TURN, ""),
            reply(Reply::Append(vec![" \n".to_owned()])),
            &calls,
        ),
        source(
            "big",
            &hook_body("h", BEFORE_TURN, ""),
            reply(Reply::Append(vec![big])),
            &calls,
        ),
        source(
            "fit",
            &hook_body("h", BEFORE_TURN, ""),
            reply(Reply::Append(vec![
                "x".repeat(super::points::MAX_APPEND_CHARS),
            ])),
            &calls,
        ),
        source(
            "wrong",
            &hook_body("h", BEFORE_TURN, ""),
            reply(Reply::Rewrite("no".to_owned())),
            &calls,
        ),
    ];
    let (engine, _finished) = engine(sources).await;
    let progress = fired(&engine, before_turn()).await;
    let appended: Vec<&str> = progress.lead.iter().map(|e| e.by.as_str()).collect();
    assert!(appended.contains(&"fit/h"));
    assert!(!appended.contains(&"big/h") && !appended.contains(&"blank/h"));
    assert_eq!(
        outcomes(&engine.take_records()),
        vec![
            ("big/h".to_owned(), rejected("too_large")),
            ("blank/h".to_owned(), rejected("blank")),
            ("fit/h".to_owned(), HookOutcome::Applied),
            ("wrong/h".to_owned(), rejected("not_allowed")),
        ]
    );
}

#[tokio::test(start_paused = true)]
async fn a_hook_past_its_timeout_is_timed_out_and_the_next_still_runs() {
    let calls = Calls::default();
    let sources = vec![
        source(
            "aa",
            &hook_body("slow", SESSION_START, ""),
            sleeping(),
            &calls,
        ),
        source(
            "bb",
            &hook_body("short", SESSION_START, "timeout_ms = 50\n"),
            sleeping(),
            &calls,
        ),
        source(
            "cc",
            &hook_body("quick", SESSION_START, ""),
            hooks::none(),
            &calls,
        ),
    ];
    let (engine, _finished) = engine(sources).await;
    let started = Instant::now();
    fired(&engine, observe(SESSION_START)).await;
    assert_eq!(started.elapsed(), Duration::from_millis(2_050));
    let records = engine.take_records();
    assert_eq!(
        outcomes(&records),
        vec![
            ("aa/slow".to_owned(), HookOutcome::TimedOut),
            ("bb/short".to_owned(), HookOutcome::TimedOut),
            ("cc/quick".to_owned(), HookOutcome::None),
        ]
    );
    assert_eq!((records[0].ms, records[1].ms), (2_000, 50));
}

#[tokio::test(start_paused = true)]
async fn a_budget_cuts_every_hook_at_the_point() {
    let calls = Calls::default();
    let sources = vec![source(
        "aa",
        &hook_body("slow", SESSION_START, "timeout_ms = 10000\n"),
        sleeping(),
        &calls,
    )];
    let (engine, _finished) = engine(sources).await;
    let mut fire = observe(SESSION_START);
    let started = Instant::now();
    fire.budget = Some(started + Duration::from_secs(2));
    fired(&engine, fire).await;
    assert_eq!(started.elapsed(), Duration::from_secs(2));
    assert_eq!(
        outcomes(&engine.take_records()),
        vec![("aa/slow".to_owned(), HookOutcome::TimedOut)]
    );
}

#[tokio::test(start_paused = true)]
async fn a_hook_runs_in_a_caller_holding_the_points_token_and_its_deadline() {
    let calls = Calls::default();
    let seen: Arc<Mutex<Option<Caller>>> = Arc::default();
    let keep = seen.clone();
    let script: Script = Arc::new(move |_, _, _| {
        let keep = keep.clone();
        Box::pin(async move {
            *keep.lock().unwrap() = Some(Caller::current());
            Reply::None
        })
    });
    let body = hook_body("h", SESSION_START, "timeout_ms = 700\n");
    let (engine, _finished) = engine(vec![source("aa", &body, script, &calls)]).await;
    let cancel = CancellationToken::new();
    let mut fire = observe(SESSION_START);
    fire.cancel = cancel.clone();
    fire.at = At {
        turn: Some(2),
        step: Some(1),
        call_id: None,
    };
    let started = Instant::now();
    fired(&engine, fire).await;
    let caller = seen.lock().unwrap().clone().unwrap();
    assert_eq!(caller.deadline, Some(started + Duration::from_millis(700)));
    assert_eq!(caller.depth, 1);
    assert_eq!(caller.at.turn, Some(2));
    let token = caller.cancel.unwrap();
    assert!(!token.is_cancelled());
    cancel.cancel();
    assert!(token.is_cancelled());
}

#[tokio::test]
async fn a_panicking_hook_fails_with_the_plugins_wording_never_the_payload() {
    let calls = Calls::default();
    let at_once: Script = Arc::new(|_, _, _| panic!("secret payload"));
    let later: Script = Arc::new(|_, _, _| Box::pin(async { panic!("secret payload") }));
    let sources = vec![
        source("aa", &hook_body("h", SESSION_START, ""), at_once, &calls),
        source("bb", &hook_body("h", SESSION_START, ""), later, &calls),
    ];
    let (engine, _finished) = engine(sources).await;
    fired(&engine, observe(SESSION_START)).await;
    assert_eq!(
        outcomes(&engine.take_records()),
        vec![
            (
                "aa/h".to_owned(),
                HookOutcome::Failed {
                    message: panicked("aa")
                }
            ),
            (
                "bb/h".to_owned(),
                HookOutcome::Failed {
                    message: panicked("bb")
                }
            ),
        ]
    );
}

#[tokio::test(start_paused = true)]
async fn a_failing_gate_hook_with_block_blocks_and_stops_the_point() {
    let calls = Calls::default();
    let sources = vec![
        source(
            "aa",
            &hook_body(
                "h",
                "user-input",
                "on_failure = \"block\"\ntimeout_ms = 10\n",
            ),
            sleeping(),
            &calls,
        ),
        source(
            "bb",
            &hook_body("h", "user-input", ""),
            hooks::none(),
            &calls,
        ),
    ];
    let (engine, _finished) = engine(sources).await;
    let progress = fired(&engine, observe("user-input")).await;
    assert_eq!(
        progress.blocked,
        Some(("aa/h".to_owned(), "it timed out".to_owned()))
    );
    assert_eq!(calls.count("h"), 1);
}

#[tokio::test]
async fn off_and_unmet_hooks_are_skipped() {
    let calls = Calls::default();
    let body = hook_body("off", SESSION_START, "")
        + &hook_body("needs", SESSION_START, "requires = [\"nope/x\"]\n")
        + &hook_body("on", SESSION_START, "");
    let (engine, _finished) = engine(vec![source("aa", &body, hooks::none(), &calls)]).await;
    engine.set_off([Name::new("aa", "off")].into_iter().collect());
    fired(&engine, observe(SESSION_START)).await;
    let hooks: Vec<String> = calls.all().into_iter().map(|c| c.hook).collect();
    assert_eq!(hooks, ["on"]);
}

#[tokio::test]
async fn background_hooks_report_when_done_and_an_abort_cancels_the_rest() {
    let calls = Calls::default();
    let sources = vec![
        source(
            "aa",
            &hook_body("quick", AFTER_TURN, "background = true\n"),
            hooks::none(),
            &calls,
        ),
        source(
            "bb",
            &hook_body("slow", AFTER_TURN, "background = true\n"),
            sleeping(),
            &calls,
        ),
    ];
    let (engine, mut finished) = engine(sources).await;
    let progress = fired(&engine, observe(AFTER_TURN)).await;
    assert!(progress.current.is_none());
    assert!(engine.take_records().is_empty());
    let (id, ran) = finished.recv().await.unwrap();
    engine.finished(id);
    assert_eq!(
        (ran.hook.as_str(), ran.outcome.clone()),
        ("aa/quick", HookOutcome::None)
    );
    assert!(engine.background_running(None));
    let aborted = engine.abort_background(None);
    assert_eq!(
        outcomes(&aborted),
        vec![("bb/slow".to_owned(), HookOutcome::Cancelled)]
    );
    assert!(!engine.background_running(None));
}

fn nesting() -> Script {
    Arc::new(|host: Arc<dyn Host>, _, _| {
        Box::pin(async move {
            host.fire("x".to_owned(), String::new()).await;
            Reply::None
        })
    })
}

#[test]
fn points_fire_stops_at_the_depth_cap_and_logs_the_nested_runs() {
    on_alarm_stack(Timer::Real, || async {
        let calls = Calls::default();
        let body = "\n[[points]]\nname = \"x\"\nkind = \"observe\"\nversion = \"1\"\n".to_owned()
            + &hook_body("h", "pp/x", "");
        let (engine, _finished) = engine(vec![source("pp", &body, nesting(), &calls)]).await;
        let answers = engine
            .clone()
            .fire_custom("pp", "x".to_owned(), String::new())
            .await;
        assert_eq!(answers.len(), 1);
        assert_eq!(calls.count("h"), usize::from(super::points::MAX_DEPTH));
        let records = outcomes(&engine.take_records());
        let mut expected = vec![(
            "pp/h".to_owned(),
            HookOutcome::Rejected {
                rule: "depth".to_owned(),
            },
        )];
        expected
            .extend((0..super::points::MAX_DEPTH).map(|_| ("pp/h".to_owned(), HookOutcome::None)));
        assert_eq!(records, expected);
    });
}

#[test]
fn points_fire_stops_at_the_depth_cap_with_a_section_shown_and_marked_at_every_point() {
    on_alarm_stack(Timer::Real, || async {
        let calls = Calls::default();
        let marks = hooks::every_point().replace("marks = [", "marks = [\"pp/x\", ");
        let body = "\n[[points]]\nname = \"x\"\nkind = \"observe\"\nversion = \"1\"\n".to_owned()
            + &hook_body("h", "pp/x", "")
            + &hooks::section("sec", &marks);
        let make: Make = hooks::sections(nesting(), hooks::busy(), calls.clone());
        let (registry, problems) = Registry::from_sources(
            vec![(hooks::slotted("pp", &body), make)],
            &PluginConfig::default(),
        );
        assert!(problems.is_empty(), "{problems:?}");
        let started = registry
            .start(&BTreeSet::new(), Arc::new(KernelHost::new()))
            .await;
        let lane = crate::plugin::slots::SlotLane::start(
            &started.hooks.sections,
            &started.hooks.plugins,
            Default::default(),
        );
        let (engine, _finished) = Engine::new(
            started.hooks.table,
            started.hooks.plugins,
            Some(started.host),
        );
        engine.set_lane(lane.signals().clone());
        let (events, mut shown) = tokio::sync::mpsc::channel(64);
        let name = vec!["pp/sec".to_owned()];
        lane.show(name.clone(), name, &events);
        shown.recv().await.unwrap();
        let answers = engine
            .clone()
            .fire_custom("pp", "x".to_owned(), String::new())
            .await;
        assert_eq!(answers.len(), 1);
        assert_eq!(calls.count("h"), usize::from(super::points::MAX_DEPTH));
        tokio::time::sleep(Duration::from_millis(600)).await;
        assert!(calls.renders().len() >= 3, "{:?}", calls.renders());
    });
}

#[tokio::test]
async fn a_plugin_fires_only_its_own_points() {
    let calls = Calls::default();
    let body = "\n[[points]]\nname = \"x\"\nkind = \"contribute\"\nversion = \"2\"\n".to_owned()
        + &hook_body("h", "pp/x", "");
    let sources = vec![
        source("pp", &body, hooks::append("A"), &calls),
        source("qq", "", hooks::none(), &calls),
    ];
    let (engine, _finished) = engine(sources).await;
    let theirs = engine
        .clone()
        .fire_custom("qq", "pp/x".to_owned(), String::new())
        .await;
    assert!(theirs.is_empty());
    let ours = engine
        .clone()
        .fire_custom("pp", "pp/x".to_owned(), "payload".to_owned())
        .await;
    assert_eq!(ours.len(), 1);
    assert_eq!(ours[0].reply, Reply::Append(vec!["A".to_owned()]));
    let Event::Custom(custom) = &calls.of("h")[0].event else {
        panic!();
    };
    assert_eq!(
        (
            custom.point.as_str(),
            custom.version.as_str(),
            custom.payload.as_str()
        ),
        ("pp/x", "2", "payload")
    );
}

#[tokio::test]
async fn reserves_follow_the_order_and_leave_off_hooks_out() {
    let calls = Calls::default();
    let body = hook_body("r", BEFORE_TURN, "reserve = \"RESERVED\"\n")
        + &hook_body("off", BEFORE_TURN, "reserve = \"OFF\"\n");
    let (engine, _finished) = engine(vec![source("aa", &body, hooks::none(), &calls)]).await;
    engine.set_off([Name::new("aa", "off")].into_iter().collect());
    assert_eq!(engine.reserves(BEFORE_TURN), ["RESERVED"]);
}

#[tokio::test]
async fn an_empty_engine_fires_nothing() {
    let (engine, _finished) = Engine::empty();
    assert!(!engine.attached(SESSION_START));
    let progress = fired(&engine, with_news(before_turn())).await;
    assert!(progress.lead.is_empty());
    assert!(!engine.words_news(|_| true));
    assert!(
        fired(&engine, observe(SESSION_START))
            .now_or_never()
            .is_some()
    );
}

#[tokio::test(start_paused = true)]
async fn a_background_hook_keeps_its_own_timeout_beyond_the_budget() {
    let calls = Calls::default();
    let body = hook_body(
        "slow",
        AFTER_TURN,
        "background = true\ntimeout_ms = 60000\n",
    );
    let (engine, mut finished) = engine(vec![source("aa", &body, sleeping(), &calls)]).await;
    let mut fire = observe(AFTER_TURN);
    let started = Instant::now();
    fire.budget = Some(started + Duration::from_secs(2));
    fired(&engine, fire).await;
    let (_, ran) = finished.recv().await.unwrap();
    assert_eq!(ran.outcome, HookOutcome::TimedOut);
    assert_eq!(started.elapsed(), Duration::from_secs(60));
}

#[tokio::test]
async fn an_unmet_or_off_hook_keeps_its_place_so_the_others_keep_their_order() {
    let calls = Calls::default();
    let sources = vec![
        source(
            "zz",
            &hook_body("x", SESSION_START, "before = [\"mm/a\"]\n"),
            hooks::none(),
            &calls,
        ),
        source(
            "mm",
            &(hook_body(
                "a",
                SESSION_START,
                "requires = [\"nope/q\"]\nbefore = [\"bb/y\"]\n",
            ) + &hook_body(
                "b",
                SESSION_START,
                "after = [\"zz/x\"]\nbefore = [\"bb/z\"]\n",
            )),
            hooks::none(),
            &calls,
        ),
        source(
            "bb",
            &(hook_body("y", SESSION_START, "") + &hook_body("z", SESSION_START, "")),
            hooks::none(),
            &calls,
        ),
    ];
    let (engine, _finished) = engine(sources).await;
    engine.set_off([Name::new("mm", "b")].into_iter().collect());
    fired(&engine, observe(SESSION_START)).await;
    let hooks: Vec<String> = calls.all().into_iter().map(|c| c.hook).collect();
    assert_eq!(hooks, ["x", "y", "z"]);
    let order: Vec<String> = engine
        .table()
        .order(SESSION_START)
        .iter()
        .map(ToString::to_string)
        .collect();
    assert_eq!(order, ["zz/x", "mm/a", "bb/y", "mm/b", "bb/z"]);
}

fn parsed(text: &str) -> wisp_plugin_api::manifest::Manifest {
    wisp_plugin_api::manifest::Manifest::parse(text, wisp_plugin_api::manifest::Origin::Bundled)
        .unwrap()
}

fn before_turn_names(table: &super::Table) -> Vec<String> {
    table
        .order(BEFORE_TURN)
        .iter()
        .map(ToString::to_string)
        .collect()
}

#[test]
fn the_bundled_hooks_sit_between_the_tools_note_and_the_processes() {
    let time = parsed(wisp_plugin_time::MANIFEST);
    let context = parsed(wisp_plugin_context::MANIFEST);
    let workspace = parsed(wisp_plugin_workspace::MANIFEST);
    let shell = parsed(wisp_plugin_shell::MANIFEST);
    let idle = parsed(wisp_plugin_idle::MANIFEST);
    let agents = parsed(wisp_plugin_agents::MANIFEST);
    let both = super::Table::build([&agents, &context, &idle, &time, &workspace, &shell]);
    assert_eq!(
        before_turn_names(&both),
        [
            "workspace/environment",
            "workspace/terminal",
            "agents/children",
            "core/tools",
            "time/sent",
            "context/fill",
            "idle/note",
            "agents/kept-warm",
            "shell/processes",
            "core/typed"
        ]
    );
    assert!(both.problems().is_empty(), "{:?}", both.problems());
    assert_eq!(
        before_turn_names(&super::Table::build([&context])),
        ["core/tools", "context/fill", "core/typed"]
    );
}

#[test]
fn sentences_follow_their_order_skip_a_missing_id_and_a_cycle_leaves_them_out() {
    let time = parsed(wisp_plugin_time::MANIFEST);
    let context = parsed(wisp_plugin_context::MANIFEST);
    let text = |m: &wisp_plugin_api::manifest::Manifest| m.system.clone().unwrap().text;
    for table in [
        super::Table::build([&time, &context]),
        super::Table::build([&context, &time]),
    ] {
        assert_eq!(table.sentences(), [text(&time), text(&context)]);
    }
    assert_eq!(
        super::Table::build([&context]).sentences(),
        [text(&context)]
    );
    let looped = |id: &str, other: &str| {
        parsed(&hooks::manifest(
            id,
            &format!("\n[system]\ntext = \"Plugin {id} is here.\"\nafter = [\"{other}\"]\n"),
        ))
    };
    let (aa, bb) = (looped("aa", "bb"), looped("bb", "aa"));
    let table = super::Table::build([&aa, &bb, &time]);
    assert_eq!(table.sentences(), [text(&time)]);
    let cycles: Vec<(String, String)> = table
        .problems()
        .into_iter()
        .filter(|p| p.cycle)
        .map(|p| (p.plugin, p.message))
        .collect();
    let message = "its system sentence is left out: it is in a cycle, aa/system \u{2192} bb/system \u{2192} aa/system";
    assert_eq!(
        cycles,
        [
            ("aa".to_owned(), message.to_owned()),
            ("bb".to_owned(), message.to_owned())
        ]
    );
}

fn ended_event() -> TreeEvent {
    TreeEvent::AgentEnded(StopInfo {
        agent: "/a".to_owned(),
        target: "/a".to_owned(),
        by: None,
    })
}

fn watched(id: &str, body: &str, watch: hooks::Watch, calls: &Calls) -> (String, Make) {
    let make: Make = hooks::observing(hooks::none(), watch, calls.clone());
    (hooks::manifest_in("0.2.0", id, body), make)
}

fn asleep() -> hooks::Watch {
    Arc::new(|_, _, _| {
        Box::pin(async {
            tokio::time::sleep(Duration::from_secs(3600)).await;
        })
    })
}

#[tokio::test]
async fn the_doctor_note_names_a_long_hook_at_after_compact_and_rewound() {
    let calls = Calls::default();
    let body = hook_body("tidy", AFTER_COMPACT, "timeout_ms = 10000\n")
        + &hook_body("back", REWOUND, "timeout_ms = 10000\n")
        + &hook_body("free", REWOUND, "background = true\ntimeout_ms = 10000\n");
    let (engine, _finished) = engine(vec![source("aa", &body, hooks::none(), &calls)]).await;
    let problems: Vec<String> = engine
        .table()
        .problems()
        .into_iter()
        .map(|p| p.message)
        .collect();
    assert_eq!(
        problems,
        [
            "its hook `aa/back` at `rewound` runs outside a turn, so its timeout is cut to 2 s; set background = true for longer work",
            "its hook `aa/tidy` at `after-compact` runs outside a turn, so its timeout is cut to 2 s; set background = true for longer work",
        ]
    );
}

#[tokio::test(start_paused = true)]
async fn an_agent_hook_is_cut_at_two_seconds_unless_background_and_the_note_names_it() {
    let calls = Calls::default();
    let body = hook_body("held", AGENT_ENDED, "timeout_ms = 10000\n")
        + &hook_body(
            "free",
            AGENT_ENDED,
            "background = true\ntimeout_ms = 10000\n",
        );
    let (engine, mut finished) = engine(vec![watched("aa", &body, asleep(), &calls)]).await;
    let problems: Vec<String> = engine
        .table()
        .problems()
        .into_iter()
        .map(|p| p.message)
        .collect();
    assert_eq!(
        problems,
        [
            "its hook `aa/held` at `agent-ended` runs outside a turn, so its timeout is cut to 2 s; set background = true for longer work"
        ]
    );
    let started = Instant::now();
    engine
        .clone()
        .fire_detached(AGENT_ENDED, Payload::Tree(ended_event()))
        .await;
    assert_eq!(started.elapsed(), Duration::from_secs(2));
    let mut runs = Vec::new();
    for _ in 0..2 {
        let (id, ran) = finished.recv().await.unwrap();
        engine.finished(id);
        runs.push((ran.hook, ran.outcome, ran.ms, ran.at));
    }
    assert_eq!(
        runs,
        [
            (
                "aa/held".to_owned(),
                HookOutcome::TimedOut,
                2000,
                At::default()
            ),
            (
                "aa/free".to_owned(),
                HookOutcome::TimedOut,
                10_000,
                At::default()
            ),
        ]
    );
    let seen: Vec<String> = calls.tree().into_iter().map(|s| s.hook).collect();
    assert_eq!(seen, ["free", "held"]);
    assert!(!engine.background_running(None));
}

#[tokio::test]
async fn agent_hooks_at_one_point_run_in_their_after_order_and_see_the_event() {
    let calls = Calls::default();
    let body = hook_body("late", AGENT_ENDED, "after = [\"aa/early\"]\n")
        + &hook_body("early", AGENT_ENDED, "");
    let none: hooks::Watch = Arc::new(|_, _, _| Box::pin(async {}));
    let (engine, mut finished) = engine(vec![watched("aa", &body, none, &calls)]).await;
    engine
        .clone()
        .fire_detached(AGENT_ENDED, Payload::Tree(ended_event()))
        .await;
    let seen: Vec<(String, TreeEvent)> = calls
        .tree()
        .into_iter()
        .map(|s| (s.hook, s.event))
        .collect();
    assert_eq!(
        seen,
        [
            ("early".to_owned(), ended_event()),
            ("late".to_owned(), ended_event())
        ]
    );
    let mut hooks = Vec::new();
    while let Ok((id, ran)) = finished.try_recv() {
        engine.finished(id);
        hooks.push((ran.hook, ran.outcome));
    }
    assert_eq!(
        hooks,
        [
            ("aa/early".to_owned(), HookOutcome::None),
            ("aa/late".to_owned(), HookOutcome::None)
        ]
    );
}

const ROUNDS: usize = 2000;

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_background_run_is_registered_before_its_first_poll() {
    let calls = Calls::default();
    let slot: Arc<OnceLock<Weak<Engine>>> = Arc::default();
    let unregistered = Arc::new(AtomicUsize::new(0));
    let script: Script = {
        let slot = slot.clone();
        let unregistered = unregistered.clone();
        Arc::new(move |_, _, _| {
            let registered = slot
                .get()
                .and_then(Weak::upgrade)
                .is_some_and(|e| e.background_running(None));
            if !registered {
                unregistered.fetch_add(1, Ordering::SeqCst);
            }
            Box::pin(async { Reply::None })
        })
    };
    let body = hook_body("quick", AFTER_TURN, "background = true\n");
    let (engine, mut finished) = engine(vec![source("aa", &body, script, &calls)]).await;
    slot.set(Arc::downgrade(&engine)).unwrap();
    let spinning = tokio::spawn(async {
        loop {
            tokio::task::yield_now().await;
        }
    });
    for _ in 0..ROUNDS {
        fired(&engine, observe(AFTER_TURN)).await;
        let (id, _) = finished.recv().await.unwrap();
        engine.finished(id);
    }
    spinning.abort();
    assert_eq!(calls.count("quick"), ROUNDS);
    assert_eq!(unregistered.load(Ordering::SeqCst), 0);
}

mod compact;
mod entries;
mod full;

#[test]
fn a_section_in_a_cycle_is_named_as_a_prompt_section() {
    use wisp_plugin_testkit::hooks as kit;
    let aa = parsed(&kit::manifest_in(
        "0.2.0",
        "aa",
        &kit::prompt_section("notes", "lead", "after = [\"bb/h\"]\n"),
    ));
    let bb = parsed(&kit::manifest_in(
        "0.2.0",
        "bb",
        "\n[[hooks]]\nname = \"h\"\npoint = \"before-turn\"\nafter = [\"aa/notes\"]\n",
    ));
    let table = super::Table::build([&aa, &bb]);
    let messages: Vec<String> = table
        .problems()
        .into_iter()
        .filter(|p| p.cycle)
        .map(|p| p.message)
        .collect();
    assert_eq!(messages.len(), 2, "{messages:?}");
    assert!(
        messages
            .iter()
            .any(|m| m.starts_with("its prompt section `aa/notes` is turned off: it is in a cycle")),
        "{messages:?}"
    );
    assert!(
        messages
            .iter()
            .any(|m| m.starts_with("its hook `bb/h` is turned off: it is in a cycle")),
        "{messages:?}"
    );
}

fn reading(text: impl Fn(Option<News>) -> Reply + Send + Sync + 'static) -> Script {
    let text = Arc::new(text);
    Arc::new(move |host: Arc<dyn Host>, _, _| {
        let reply = text(host.opening());
        Box::pin(async move { reply })
    })
}

fn news_source(body_extra: &str, script: Script, calls: &Calls) -> (String, Make) {
    let make: Make = hooks::instantiate(script, calls.clone());
    let body = hook_body("words", BEFORE_TURN, body_extra);
    (hooks::manifest_in("0.2.0", "nn", &body), make)
}

#[tokio::test]
async fn only_a_job_news_hook_reads_the_news_and_only_an_accepted_append_words_it() {
    let calls = Calls::default();
    let seen: Arc<Mutex<Vec<Option<News>>>> = Arc::default();
    let plain = {
        let seen = seen.clone();
        reading(move |news| {
            seen.lock().unwrap().push(news);
            Reply::Append(vec!["plain".to_owned()])
        })
    };
    let (plain_engine, _finished) = engine(vec![source(
        "aa",
        &hook_body("p", BEFORE_TURN, ""),
        plain,
        &calls,
    )])
    .await;
    let progress = fired(&plain_engine, with_news(before_turn())).await;
    assert!(!progress.worded);
    assert_eq!(*seen.lock().unwrap(), [None]);
    assert!(!plain_engine.words_news(|_| true));
    let cases: [(Reply, HookOutcome, bool); 3] = [
        (
            Reply::Append(vec!["x".repeat(12_000)]),
            HookOutcome::Applied,
            true,
        ),
        (
            Reply::Append(vec!["  ".to_owned()]),
            HookOutcome::Rejected {
                rule: "blank".to_owned(),
            },
            false,
        ),
        (Reply::None, HookOutcome::None, false),
    ];
    for (reply, outcome, worded) in cases {
        let script = reading(move |news| {
            assert_eq!(news, Some(self::news()));
            reply.clone()
        });
        let (news_engine, _finished) =
            engine(vec![news_source("job_news = true\n", script, &calls)]).await;
        assert!(news_engine.words_news(|_| true));
        let progress = fired(&news_engine, with_news(before_turn())).await;
        assert_eq!(progress.worded, worded);
        assert_eq!(
            outcomes(&news_engine.take_records()),
            [("nn/words".to_owned(), outcome)]
        );
        assert!(!news_engine.words_news(|name| *name != Name::new("nn", "words")));
    }
}

#[tokio::test]
async fn a_hook_without_job_news_keeps_the_append_cap() {
    let calls = Calls::default();
    let script = reply(Reply::Append(vec!["x".repeat(12_000)]));
    let (engine, _finished) = engine(vec![news_source("", script, &calls)]).await;
    let progress = fired(&engine, with_news(before_turn())).await;
    assert!(progress.lead.is_empty());
    assert_eq!(
        outcomes(&engine.take_records()),
        [(
            "nn/words".to_owned(),
            HookOutcome::Rejected {
                rule: "too_large".to_owned()
            }
        )]
    );
}

#[tokio::test]
async fn a_failed_or_timed_out_news_hook_leaves_the_news_unworded() {
    let calls = Calls::default();
    let read = Arc::new(AtomicUsize::new(0));
    let late = |then: Script| -> Script {
        let read = read.clone();
        Arc::new(move |host: Arc<dyn Host>, hook, event| {
            if host.opening().is_some() {
                read.fetch_add(1, Ordering::Relaxed);
            }
            then(host, hook, event)
        })
    };
    let panicking: Script = Arc::new(|_, _, _| Box::pin(async { panic!("boom") }));
    for (extra, script) in [
        ("job_news = true\n", late(panicking)),
        ("job_news = true\ntimeout_ms = 50\n", late(sleeping())),
    ] {
        let (engine, _finished) = engine(vec![news_source(extra, script, &calls)]).await;
        let progress = fired(&engine, with_news(before_turn())).await;
        assert!(!progress.worded);
        assert!(progress.lead.is_empty());
    }
    assert_eq!(read.load(Ordering::Relaxed), 2);
}

fn filled(estimate: u64) -> What {
    What::Event(Event::ContextThreshold(FillInfo {
        turn: 1,
        step: 0,
        before_turn: true,
        estimate,
        measured: None,
        window: Some(200_000),
        point: None,
        ceiling: None,
        auto_compact: true,
        epoch: 0,
    }))
}

#[test]
fn from_is_reached_at_exactly_its_tokens_or_share() {
    for from in [Threshold::Tokens(20_000), Threshold::Percent(10)] {
        assert!(!reached(Some(from), &filled(19_999)), "{from:?}");
        assert!(reached(Some(from), &filled(20_000)), "{from:?}");
        assert!(reached(Some(from), &filled(20_001)), "{from:?}");
    }
    assert!(reached(None, &filled(0)));
}

#[test]
fn every_manifest_point_has_its_core_rule() {
    use crate::plugin::hooks::points::{self, AGENT_POINTS};
    use wisp_plugin_api::manifest::{CORE_POINTS, OBSERVE_POINTS};
    let manifest: BTreeSet<&str> = CORE_POINTS.into_iter().chain(AGENT_POINTS).collect();
    for point in manifest {
        let rule = points::core(point).unwrap_or_else(|| panic!("{point} has no core rule"));
        let observe = rule.takes.is_empty() && !rule.gate;
        let listed = OBSERVE_POINTS.contains(&point) || AGENT_POINTS.contains(&point);
        assert_eq!(observe, listed, "{point}");
    }
}

#[test]
fn the_points_table_has_one_row_for_each_core_point_with_its_timeout() {
    use crate::plugin::hooks::points::{self, AGENT_POINTS};
    use crate::test_guard::{table_rows, ticked};
    use wisp_plugin_api::manifest::{CORE_POINTS, OBSERVE_POINTS};
    let docs = include_str!("../../../../../../docs/plugins.md");
    let mut rows: Vec<(String, String)> =
        table_rows(docs, "| Point | Fires | Accepts | Default timeout |")
            .filter_map(|line| {
                let (point, _) = line.strip_prefix("| `")?.split_once('`')?;
                let timeout = line.strip_suffix(" |")?.rsplit("| ").next()?;
                Some((point.to_owned(), timeout.to_owned()))
            })
            .filter(|(point, _)| point != "<plugin>/<point>")
            .collect();
    let mut want: Vec<(String, String)> = CORE_POINTS
        .into_iter()
        .chain(AGENT_POINTS)
        .map(|point| {
            let timeout = points::core(point).unwrap().timeout.as_secs();
            (point.to_owned(), format!("{timeout} s"))
        })
        .collect();
    rows.sort_unstable();
    want.sort_unstable();
    assert_eq!(rows, want, "docs/plugins.md's points table");
    let prose = docs
        .lines()
        .find(|line| line.starts_with("A hook's `point` is one of world"));
    let (listed, observe) = prose.unwrap().split_once(" may use, are ").unwrap();
    let listed = listed
        .split_once(" points (")
        .unwrap()
        .1
        .split_once("), or")
        .unwrap()
        .0;
    for (text, source) in [(listed, &CORE_POINTS[..]), (observe, &OBSERVE_POINTS[..])] {
        let (mut named, mut source) = (ticked(text), source.to_vec());
        named.sort_unstable();
        source.sort_unstable();
        assert_eq!(named, source, "docs/plugins.md's list of points");
    }
}

fn rejected(rule: &str) -> HookOutcome {
    HookOutcome::Rejected {
        rule: rule.to_owned(),
    }
}

fn panicking() -> Script {
    Arc::new(|_, _, _| Box::pin(async { panic!("hook payload") }))
}

fn restart_firing() -> Script {
    Arc::new(|host: Arc<dyn Host>, hook, _| {
        Box::pin(async move {
            if hook == "start" {
                host.fire("x".to_owned(), String::new()).await;
            }
            Reply::None
        })
    })
}

#[tokio::test(start_paused = true)]
async fn a_restart_runs_only_that_plugins_session_start_hooks_resumed_at_the_sessions_depth() {
    let calls = Calls::default();
    let others = Calls::default();
    let body = "\n[[points]]\nname = \"x\"\nkind = \"observe\"\nversion = \"1\"\n".to_owned()
        + &hook_body("start", SESSION_START, "")
        + &hook_body("h", "pp/x", "")
        + &hook_body("turn", BEFORE_TURN, "");
    let sources = vec![
        source("pp", &body, restart_firing(), &calls),
        source(
            "qq",
            &hook_body("start", SESSION_START, ""),
            none(),
            &others,
        ),
        source(
            "zz",
            &hook_body("slow", SESSION_START, "timeout_ms = 60000\n"),
            sleeping(),
            &Calls::default(),
        ),
    ];
    let (engine, _finished) = engine(sources).await;
    let deep =
        Caller::call(CancellationToken::new(), None).hook(super::points::MAX_DEPTH, At::default());
    deep.scope(engine.clone().restarted("pp")).await;
    let events: Vec<Event> = calls.of("start").into_iter().map(|c| c.event).collect();
    assert_eq!(
        events,
        vec![Event::SessionStart(SessionStart { resumed: true })]
    );
    assert_eq!(others.count("start"), 0);
    assert_eq!(
        outcomes(&engine.take_records()),
        vec![
            ("pp/h".to_owned(), HookOutcome::None),
            ("pp/start".to_owned(), HookOutcome::None),
        ]
    );
    let started = Instant::now();
    let deadline = Caller::call(
        CancellationToken::new(),
        Some(started + Duration::from_millis(100)),
    );
    deadline.scope(engine.clone().restarted("zz")).await;
    assert_eq!(started.elapsed(), Duration::from_millis(100));
    let unbounded = Caller::call(CancellationToken::new(), None);
    unbounded.scope(engine.clone().restarted("zz")).await;
    assert_eq!(
        started.elapsed(),
        Duration::from_millis(100) + OUTSIDE_BUDGET
    );
    assert_eq!(
        outcomes(&engine.take_records()),
        vec![
            ("zz/slow".to_owned(), HookOutcome::TimedOut),
            ("zz/slow".to_owned(), HookOutcome::TimedOut),
        ]
    );
}

#[tokio::test]
async fn a_replaced_set_keeps_the_records_and_the_background_hooks_running() {
    let calls = Calls::default();
    let slow = || {
        source(
            "aa",
            &hook_body("slow", AFTER_TURN, "background = true\n"),
            sleeping(),
            &calls,
        )
    };
    let quick = || {
        source(
            "bb",
            &hook_body("quick", AFTER_TURN, ""),
            hooks::none(),
            &calls,
        )
    };
    let (engine, _finished) = engine(vec![slow(), quick()]).await;
    fired(&engine, observe(AFTER_TURN)).await;
    let (registry, _) = Registry::from_sources(vec![quick()], &PluginConfig::default());
    let started = registry
        .start(&BTreeSet::new(), Arc::new(KernelHost::new()))
        .await;
    engine.replace(started.hooks.table, started.hooks.plugins);
    assert_eq!(engine.plugin_ids(), ["bb"]);
    assert_eq!(
        outcomes(&engine.take_records()),
        [("bb/quick".to_owned(), HookOutcome::None)]
    );
    let leaving = BTreeSet::from(["aa".to_owned()]);
    let staying = BTreeSet::from(["bb".to_owned()]);
    assert!(engine.background_running(Some(&leaving)));
    assert!(engine.abort_background(Some(&staying)).is_empty());
    assert_eq!(
        outcomes(&engine.abort_background(Some(&leaving))),
        [("aa/slow".to_owned(), HookOutcome::Cancelled)]
    );
    assert!(!engine.background_running(None));
}
