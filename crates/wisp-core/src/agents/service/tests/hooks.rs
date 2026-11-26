use std::sync::atomic::{AtomicBool, Ordering};

use tokio::sync::Notify;
use wisp_plugin_api::hooks::{Event, Reply};
use wisp_plugin_api::host::Host;
use wisp_plugin_api::tree::{self, AgentNode, Identity, ReportInfo, StopInfo, TreeEvent};
use wisp_plugin_testkit::hooks::{self as kit, Calls};
use wisp_protocol::HookOutcome;

use super::resume::{
    cancel, child_send, idle, interrupted_tree, reported, resume, sibling_mail, streaming,
};
use super::*;
use crate::plugin::hooks::points::AGENT_POINTS;
use crate::stack_alarm::{Timer, on_alarm_stack};

#[test]
fn a_spawned_child_runs_hooks_on_its_own_instance_and_its_brief_is_never_typed() {
    on_alarm_stack(Timer::Paused, || async {
        let calls = Calls::default();
        let mut run = start(
            "hooks-child",
            Setup {
                hooks: vec![hooked(&calls)],
                ..Setup::default()
            },
            vec![
                (
                    "/review",
                    vec![uses(&[report_call("r1", "Looked at it")]), done("bye")],
                ),
                ("/", vec![done("noted"), done("noted")]),
            ],
        )
        .await;
        run.spawn(spawn_request("review", "Review the change."))
            .await;
        assert!(matches!(
            run.child_ended("/review").await,
            TurnOutcome::Completed { .. }
        ));
        run.root_ended().await;
        let run = run.quit().await;
        let starts = calls.of("start");
        let instances: std::collections::BTreeSet<usize> =
            starts.iter().map(|c| c.instance).collect();
        assert_eq!(starts.len(), 2, "{starts:?}");
        assert_eq!(instances.len(), 2, "{starts:?}");
        let turns = calls.of("turn");
        assert!(!turns.is_empty());
        for called in &turns {
            let Event::BeforeTurn(turn) = &called.event else {
                panic!("{called:?}");
            };
            assert_eq!((turn.typed.as_str(), turn.images), ("", 0), "{turn:?}");
            assert!(instances.contains(&called.instance));
        }
        let child_turns = run
            .records("/review")
            .into_iter()
            .filter(|e| matches!(e, LogEvent::HookRan { point, .. } if point == "before-turn"))
            .count();
        assert!(child_turns >= 1);
        let child_runs = run
            .records("/review")
            .into_iter()
            .filter(|e| matches!(e, LogEvent::HookRan { hook, point, .. } if hook == "hooked/start" && point == "session-start"))
            .count();
        assert_eq!(child_runs, 1);
    });
}

#[test]
fn a_spawn_during_a_before_request_hook_builds_the_child_and_the_point_goes_on() {
    on_alarm_stack(Timer::Paused, || async {
        spawn_mid_hook("hooks-spawn-mid-hook", false).await;
    });
}

#[test]
fn a_spawn_during_a_before_request_hook_with_a_section_shown_and_marked_at_every_point() {
    on_alarm_stack(Timer::Paused, || async {
        spawn_mid_hook("hooks-spawn-mid-hook-shown", true).await;
    });
}

pub(super) fn gated_hook(
    answer: impl Fn(&str, &Event) -> (bool, Reply) + Send + Sync + 'static,
) -> (kit::Script, Arc<Notify>, Arc<Notify>) {
    let gate = Arc::new(Notify::new());
    let started = Arc::new(Notify::new());
    let script: kit::Script = {
        let gate = gate.clone();
        let started = started.clone();
        Arc::new(move |_, hook, event| {
            let (hold, reply) = answer(&hook, &event);
            let gate = gate.clone();
            let started = started.clone();
            Box::pin(async move {
                if hold {
                    started.notify_one();
                    gate.notified().await;
                }
                reply
            })
        })
    };
    (script, gate, started)
}

fn first_only() -> impl Fn() -> bool + Send + Sync + 'static {
    let held = AtomicBool::new(false);
    move || !held.swap(true, Ordering::SeqCst)
}

async fn spawn_mid_hook(name: &str, shown: bool) {
    let calls = Calls::default();
    let first = first_only();
    let (script, gate, started) = gated_hook(move |_, _| (first(), Reply::None));
    let body = "\n[[hooks]]\nname = \"asked\"\npoint = \"before-request\"\n";
    let make: crate::plugin::Make = kit::instantiate(script, calls.clone());
    let mut hooks = vec![(kit::manifest("hooked", body), make)];
    hooks.extend(sidebar_hooks(shown));
    let mut run = start(
        name,
        Setup {
            hooks,
            ..Setup::default()
        },
        vec![
            (
                "/review",
                vec![uses(&[report_call("r1", "Looked at it")]), done("bye")],
            ),
            ("/", vec![done("noted"), done("noted")]),
        ],
    )
    .await;
    if shown {
        run.send(sidebar_shown()).await;
    }
    run.say("go").await;
    within("the hook's start", started.notified()).await;
    run.spawn(spawn_request("review", "Review the change."))
        .await;
    run.until("the spawn's answer", |e| {
        matches!(e, AgentEvent::AgentSpawned { .. })
    })
    .await;
    gate.notify_one();
    assert!(matches!(
        run.root_ended().await,
        TurnOutcome::Completed { .. }
    ));
    assert!(matches!(
        run.child_ended("/review").await,
        TurnOutcome::Completed { .. }
    ));
    let run = run.quit().await;
    let instances: std::collections::BTreeSet<usize> =
        calls.of("asked").iter().map(|c| c.instance).collect();
    assert_eq!(instances.len(), 2);
    assert!(!run.requests("/").is_empty());
}

#[test]
fn a_spawn_during_an_overflow_compactions_before_compact_hook_builds_the_child_and_the_compaction_goes_on()
 {
    on_alarm_stack(Timer::Paused, || async {
        spawn_mid_compaction("hooks-spawn-mid-gate", "before-compact", false).await;
    });
}

#[test]
fn a_spawn_during_an_overflow_compactions_head_hook_builds_the_child_and_the_head_goes_on() {
    on_alarm_stack(Timer::Paused, || async {
        spawn_mid_compaction("hooks-spawn-mid-head", "compaction-head", false).await;
    });
}

#[test]
fn a_spawn_during_the_head_hook_of_a_swap_for_an_overflow_builds_the_child_and_the_swap_goes_on() {
    on_alarm_stack(Timer::Paused, || async {
        spawn_mid_compaction("hooks-spawn-mid-swap", "compaction-head", true).await;
    });
}

#[test]
fn a_spawn_during_a_boundarys_context_threshold_hook_builds_the_child_and_the_request_goes_on() {
    on_alarm_stack(Timer::Paused, || async {
        spawn_mid_request("hooks-spawn-mid-threshold", "ask").await;
    });
}

#[test]
fn a_spawn_during_the_before_compact_hook_of_a_requested_compaction_builds_the_child_and_it_goes_on()
 {
    on_alarm_stack(Timer::Paused, || async {
        spawn_mid_request("hooks-spawn-mid-request-gate", "gate").await;
    });
}

async fn spawn_mid_request(name: &str, held: &'static str) {
    use super::limits::{SUMMARY, priced};
    let calls = Calls::default();
    let first_ask = first_only();
    let first_hold = first_only();
    let (script, gate, started) = gated_hook(move |hook, event| {
        let reply = match event {
            Event::ContextThreshold(fill) if !fill.before_turn && first_ask() => {
                Reply::Compact(None)
            }
            _ => Reply::None,
        };
        (hook == held && first_hold(), reply)
    });
    let body = "\n[[hooks]]\nname = \"ask\"\npoint = \"context-threshold\"\n\n[[hooks]]\nname = \"gate\"\npoint = \"before-compact\"\n";
    let make: crate::plugin::Make = kit::instantiate(script, calls.clone());
    let mut run = start(
        name,
        Setup {
            hooks: vec![(kit::manifest("hooked", body), make)],
            models: Some(WISP_ONLY),
            ..Setup::default()
        },
        vec![
            (
                "/review",
                vec![uses(&[report_call("r1", "Looked at it")]), done("bye")],
            ),
            (
                "/",
                vec![
                    priced(uses(&[call("c1", "nothing", json!({}))]), 50_000, 10),
                    priced(done(SUMMARY), 50_000, 300),
                    done("noted"),
                    done("noted"),
                ],
            ),
        ],
    )
    .await;
    run.say("go").await;
    within("the hook's start", started.notified()).await;
    run.spawn(spawn_request("review", "Review the change."))
        .await;
    run.until("the spawn's answer", |e| {
        matches!(e, AgentEvent::AgentSpawned { .. })
    })
    .await;
    gate.notify_one();
    let ended = run.root_ended().await;
    assert!(matches!(ended, TurnOutcome::Completed { .. }), "{ended:?}");
    assert!(matches!(
        run.child_ended("/review").await,
        TurnOutcome::Completed { .. }
    ));
    let run = run.quit().await;
    let requested: Vec<Option<String>> = pick!(
        run.records("/"),
        LogEvent::ContextCompacted { requested_by, .. } => requested_by
    );
    assert_eq!(requested, [Some("hooked/ask".to_owned())]);
}

const WISP_ONLY: &str = "[models.\"claude-opus-5-5\"]\nnative_compaction = false\n";
const BAND: &str = "[models.\"claude-opus-5-5\"]\ncompact_at = 80000\n";

pub(super) fn native_summary(summary: &str) -> Step {
    let usage = Usage {
        input_tokens: 1_200,
        ..Usage::default()
    };
    let mut script = vec![
        Ok(ProviderEvent::Sending),
        Ok(started(Some("msg"), MODEL, usage)),
    ];
    script.extend(opaque(0, json!({"type": "compaction", "content": summary})));
    let summed = Usage {
        output_tokens: 300,
        ..usage
    };
    script.extend(ended(StopReason::Compaction, summed, Vec::new()));
    Step::from(script)
}

fn plain_overflow() -> Step {
    Step::from(vec![
        Ok(ProviderEvent::Sending),
        Err(ProviderError::new(
            ErrorKind::ContextOverflow,
            "prompt is too long",
        )),
    ])
}

pub(super) fn overflowed() -> Step {
    Step::from(vec![
        Ok(ProviderEvent::Sending),
        Err(ProviderError::new(
            ErrorKind::ContextOverflow,
            "prompt is too long: 250000 tokens > 200000 maximum",
        )),
    ])
}

async fn spawn_mid_compaction(name: &str, point: &str, swap: bool) {
    use super::limits::{SUMMARY, priced};
    let calls = Calls::default();
    let first = first_only();
    let (script, gate, started) = gated_hook(move |_, event| {
        let swapped = matches!(event, Event::CompactionHead(h) if h.trigger == "background");
        ((!swap || swapped) && first(), Reply::None)
    });
    let body = format!("\n[[hooks]]\nname = \"asked\"\npoint = \"{point}\"\n");
    let make: crate::plugin::Make = kit::instantiate(script, calls.clone());
    let precompute = Arc::new(Semaphore::new(0));
    let overflow = Arc::new(Semaphore::new(0));
    let root = if swap {
        vec![
            priced(done("one"), 79_500, 10),
            super::gated(native_summary("So far: one turn."), &precompute),
            priced(uses(&[call("c1", "nothing", json!({}))]), 79_600, 10),
            super::gated(plain_overflow(), &overflow),
            done("noted"),
            done("noted"),
        ]
    } else {
        vec![
            priced(done("one"), 50_000, 10),
            priced(done("two"), 150_000, 10),
            overflowed(),
            priced(done(SUMMARY), 50_000, 300),
            done("noted"),
            done("noted"),
        ]
    };
    let flags = if swap {
        vec![crate::settings::Given {
            target: wisp_protocol::settings::Target::Name(
                wisp_protocol::settings::find("context.background_tokens").unwrap(),
            ),
            value: wisp_protocol::settings::SettingValue::Tokens(1_000),
            origin: "--context.background_tokens".into(),
        }]
    } else {
        Vec::new()
    };
    let mut run = start(
        name,
        Setup {
            hooks: vec![(kit::manifest("hooked", &body), make)],
            models: Some(if swap { BAND } else { WISP_ONLY }),
            documented: swap,
            flags,
            ..Setup::default()
        },
        vec![
            (
                "/review",
                vec![uses(&[report_call("r1", "Looked at it")]), done("bye")],
            ),
            ("/", root),
        ],
    )
    .await;
    run.say("first").await;
    assert!(matches!(
        run.root_ended().await,
        TurnOutcome::Completed { .. }
    ));
    if !swap {
        run.say("second").await;
        assert!(matches!(
            run.root_ended().await,
            TurnOutcome::Completed { .. }
        ));
    }
    if swap {
        run.until("the first step", |e| {
            matches!(e, AgentEvent::StepStarted { .. })
        })
        .await;
        run.until("the precompute", |e| {
            matches!(
                e,
                AgentEvent::CompactionStarted {
                    trigger: wisp_protocol::CompactionTrigger::Background,
                    ..
                }
            )
        })
        .await;
        within("the second request", async {
            while run.requests("/").len() < 2 {
                tokio::time::sleep(Duration::from_millis(1)).await;
            }
        })
        .await;
    }
    run.say("go").await;
    if swap {
        run.until("the overflowing step", |e| {
            matches!(e, AgentEvent::StepStarted { step: 2 })
        })
        .await;
        precompute.add_permits(1);
        tokio::time::sleep(Duration::from_millis(200)).await;
        overflow.add_permits(1);
    }
    within("the hook's start", started.notified()).await;
    run.spawn(spawn_request("review", "Review the change."))
        .await;
    run.until("the spawn's answer", |e| {
        matches!(e, AgentEvent::AgentSpawned { .. })
    })
    .await;
    gate.notify_one();
    let ended = run.root_ended().await;
    assert!(matches!(ended, TurnOutcome::Completed { .. }), "{ended:?}");
    assert!(matches!(
        run.child_ended("/review").await,
        TurnOutcome::Completed { .. }
    ));
    let run = run.quit().await;
    let compacted = run
        .records("/")
        .into_iter()
        .filter(|e| matches!(e, LogEvent::ContextCompacted { .. }))
        .count();
    assert_eq!(compacted, 1);
}

fn set_time(request: u64, value: &str) -> ClientCommand {
    use wisp_protocol::settings::{SettingChange, SettingName, SettingTarget, SettingValue};
    ClientCommand::ChangeSettings {
        request: RequestId(request),
        changes: wisp_protocol::settings::sorted_changes(vec![SettingChange {
            target: SettingTarget::Name(SettingName::of(
                wisp_protocol::settings::find("time").unwrap(),
            )),
            value: Some(SettingValue::Text(value.into())),
        }]),
        save: false,
    }
}

#[tokio::test(start_paused = true)]
async fn a_child_stamps_with_the_time_its_parent_applied_and_keeps_its_note_off() {
    let mut run = start(
        "hooks-applied",
        Setup {
            clock: Some(Arc::new(ShiftedClock::at(
                "2026-10-28T06:00:00Z".parse().unwrap(),
                Some(jiff::tz::TimeZone::fixed(
                    jiff::tz::Offset::from_seconds(19_800).unwrap(),
                )),
            ))),
            ..Setup::default()
        },
        vec![
            (
                "/review",
                vec![uses(&[report_call("r1", "Looked at it")]), done("bye")],
            ),
            ("/", vec![done("noted"), done("noted")]),
        ],
    )
    .await;
    run.send(set_time(100, "utc")).await;
    run.until("the time change", |e| {
        matches!(
            e,
            AgentEvent::SettingsChanged {
                answers: Some(RequestId(100)),
                ..
            }
        )
    })
    .await;
    run.say("go").await;
    run.root_ended().await;
    run.spawn(spawn_request("review", "Review the change."))
        .await;
    run.child_ended("/review").await;
    run.root_ended().await;
    let run = run.quit().await;
    let stamp = "Sent: Wednesday 2026-10-28 06:00:00 UTC+00:00\n";
    let root = run.requests("/")[0].first_user_texts();
    assert!(root.iter().any(|t| t == stamp), "{root:?}");
    let child = run.requests("/review")[0].first_user_texts();
    assert!(child.iter().any(|t| t == stamp), "{child:?}");
    for texts in [&root, &child] {
        assert!(
            !texts.iter().any(|t| t.starts_with("Context:")),
            "{texts:?}"
        );
    }
}

#[derive(Debug, Clone)]
struct Read {
    hook: String,
    me: Option<Identity>,
    nodes: Vec<AgentNode>,
    expected: Vec<AgentNode>,
    entries: Vec<AgentEntry>,
}

type Reads = Arc<StdMutex<Vec<Read>>>;

const SEES_NOTHING: &str = "current none; tree [] | current none; tree []";

fn queue_reason(reason: wisp_protocol::QueueReason) -> tree::QueueReason {
    match reason {
        wisp_protocol::QueueReason::Concurrency => tree::QueueReason::Concurrency,
        wisp_protocol::QueueReason::Stagger => tree::QueueReason::Stagger,
        wisp_protocol::QueueReason::Endpoint => tree::QueueReason::Endpoint,
        wisp_protocol::QueueReason::Retry => tree::QueueReason::Retry,
        wisp_protocol::QueueReason::Unknown => tree::QueueReason::Unknown,
    }
}

fn hold_reason(reason: HoldReason) -> tree::HoldReason {
    match reason {
        HoldReason::Stopped => tree::HoldReason::Stopped,
        HoldReason::HeldMessages => tree::HoldReason::HeldMessages,
        HoldReason::RetryOffer => tree::HoldReason::RetryOffer,
        HoldReason::Concurrency => tree::HoldReason::Concurrency,
        HoldReason::Unknown => tree::HoldReason::Unknown,
    }
}

fn idle_outcome(outcome: IdleOutcome) -> tree::IdleOutcome {
    match outcome {
        IdleOutcome::Reported => tree::IdleOutcome::Reported,
        IdleOutcome::Asked => tree::IdleOutcome::Asked,
        IdleOutcome::Waiting => tree::IdleOutcome::Waiting,
        IdleOutcome::NoReport => tree::IdleOutcome::NoReport,
        IdleOutcome::InvalidReport => tree::IdleOutcome::InvalidReport,
        IdleOutcome::Failed => tree::IdleOutcome::Failed,
        IdleOutcome::Stopped => tree::IdleOutcome::Stopped,
        IdleOutcome::Unknown => tree::IdleOutcome::Unknown,
    }
}

fn expected_node(entry: AgentEntry) -> AgentNode {
    AgentNode {
        path: entry.path.to_string(),
        name: entry.name,
        parent: entry.parent.map(|p| p.to_string()),
        session_id: entry.session_id,
        preset: entry.preset,
        model: wisp_protocol::shown_model(&entry.model).to_owned(),
        effort: entry.effort.as_str().to_owned(),
        endpoint: entry
            .endpoint
            .as_deref()
            .map(|e| crate::agents::endpoints::plain(e).to_owned()),
        provider: entry.provider,
        depth: entry.depth,
        state: match entry.state {
            AgentState::Queued { reason } => tree::AgentState::Queued(queue_reason(reason)),
            AgentState::Running => tree::AgentState::Running,
            AgentState::Stopping => tree::AgentState::Stopping,
            AgentState::Idle { outcome } => tree::AgentState::Idle(outcome.map(idle_outcome)),
            AgentState::Held { reason } => tree::AgentState::Held(hold_reason(reason)),
            AgentState::Ended => tree::AgentState::Ended,
            AgentState::Unknown => tree::AgentState::Unknown,
        },
        activity: match entry.activity {
            Activity::Idle | Activity::Waiting { .. } => tree::Activity::Idle,
            Activity::Streaming => tree::Activity::Streaming,
            Activity::Tool { name } => tree::Activity::Tool(name),
            Activity::Queued { reason } => tree::Activity::Queued(queue_reason(reason)),
            Activity::Held { reason } => tree::Activity::Held(hold_reason(reason)),
            Activity::Unknown => tree::Activity::Unknown,
        },
        since_ms: entry.since_ms.unwrap(),
        spent_micros: crate::pricing::micros(entry.spent_usd),
        lower_bound: entry.lower_bound,
        priced: entry.priced,
        checklist: entry.checklist.map(|p| tree::Progress {
            completed: u32::try_from(p.completed).unwrap(),
            total: u32::try_from(p.total).unwrap(),
        }),
        cwd: entry.cwd,
        running_ms: entry.running_ms,
        context: entry.context.map(|c| tree::AgentFill {
            used: c.used,
            window: c.window,
            limit: c.limit,
            estimated: c.estimated,
        }),
    }
}

fn expected_nodes(service: &AgentsService) -> Vec<AgentNode> {
    let mut entries = service.snapshot();
    entries.sort_by_key(|e| {
        e.path
            .as_str()
            .split('/')
            .map(str::to_owned)
            .collect::<Vec<_>>()
    });
    entries.into_iter().map(expected_node).collect()
}

fn reader(
    kernels: &Kernels,
    reads: &Reads,
    body: &str,
    tool: bool,
) -> (String, crate::plugin::Make) {
    let kernels = kernels.clone();
    let reads = reads.clone();
    let script: kit::Script = Arc::new(move |host, hook, _| {
        let me = host.current_agent();
        let nodes = host.snapshot();
        let service = service_of(&kernels);
        reads.lock().unwrap().push(Read {
            hook,
            me,
            nodes,
            expected: expected_nodes(&service),
            entries: service.snapshot(),
        });
        Box::pin(async { wisp_plugin_api::hooks::Reply::None })
    });
    let (body, make) = if tool {
        (
            format!("\n[[tools]]\nname = \"{}\"\n{body}", kit::SEEN),
            kit::seeing(script, Calls::default()),
        )
    } else {
        (body.to_owned(), kit::instantiate(script, Calls::default()))
    };
    (kit::manifest_in("0.2.0", "seer", &body), make)
}

fn paths(nodes: &[AgentNode]) -> Vec<&str> {
    nodes.iter().map(|n| n.path.as_str()).collect()
}

#[tokio::test(start_paused = true)]
async fn agent_tree_snapshot_matches_the_services_tree_in_tree_order_and_holds_no_text_an_agent_wrote()
 {
    let kernels = Kernels::default();
    let reads = Reads::default();
    let body = "\n[[hooks]]\nname = \"response\"\npoint = \"after-response\"\n\n[[hooks]]\nname = \"turn\"\npoint = \"after-turn\"\n";
    let mut run = start(
        "tree-snapshot",
        Setup {
            hooks: vec![reader(&kernels, &reads, body, false)],
            kernels: kernels.clone(),
            ..Setup::default()
        },
        vec![
            ("/", vec![done("noted"); 8]),
            (
                "/a",
                vec![
                    uses(&[spawn_call("s1", "c", "Check MARKER-brief.")]),
                    uses(&[report_call("r1", "a found MARKER-report")]),
                    done("bye"),
                    done("bye"),
                ],
            ),
            (
                "/a/c",
                vec![uses(&[report_call("r2", "c done")]), done("bye")],
            ),
            (
                "/a-x",
                vec![
                    uses(&[report_call("r3", "x done")]),
                    done("bye"),
                    done("thanks"),
                ],
            ),
        ],
    )
    .await;
    run.spawn(spawn_request("a", "Lead MARKER-brief.")).await;
    run.spawn(spawn_request("a-x", "Help MARKER-brief.")).await;
    run.say("go").await;
    reported(&mut run, "/a").await;
    reported(&mut run, "/a-x").await;
    run.send(child_send("/a-x", "you-1", "Read MARKER-message."))
        .await;
    tokio::time::sleep(Duration::from_secs(30)).await;
    let run = run.quit().await;
    let reads = reads.lock().unwrap().clone();
    assert!(
        reads
            .iter()
            .any(|r| r.hook == "response" && r.me.as_ref().is_some_and(|m| m.path == "/")),
        "{reads:#?}"
    );
    assert!(
        reads
            .iter()
            .any(|r| r.hook == "turn" && r.me.as_ref().is_some_and(|m| m.path != "/")),
        "{reads:#?}"
    );
    for read in &reads {
        assert_eq!(read.nodes, read.expected, "{}", read.hook);
        assert!(read.nodes.len() >= 2, "{read:#?}");
        assert!(!format!("{:?}", read.nodes).contains("MARKER"), "{read:#?}");
        assert!(
            read.nodes.iter().all(|n| n.endpoint.as_deref() == Some(ENV)
                && n.provider.as_deref() == Some("anthropic")
                && n.priced),
            "{read:#?}"
        );
    }
    assert!(
        reads
            .iter()
            .any(|r| paths(&r.nodes) == ["/a", "/a/c", "/a-x"])
    );
    assert!(reads.iter().any(|r| {
        r.entries.iter().any(|e| {
            e.last_report
                .as_deref()
                .is_some_and(|t| t.contains("MARKER-report"))
        })
    }));
    assert!(format!("{:?}", run.records("/a-x")).contains("MARKER-message"));
    assert!(format!("{:?}", run.records("/a")).contains("MARKER-brief"));
}

#[tokio::test(start_paused = true)]
async fn current_agent_names_each_agent_and_a_hook_and_a_tool_call_see_the_tree_before_their_first_await()
 {
    let kernels = Kernels::default();
    let reads = Reads::default();
    let body = "\n[[hooks]]\nname = \"turn\"\npoint = \"before-turn\"\n\n[[hooks]]\nname = \"start\"\npoint = \"session-start\"\nbackground = true\n";
    let mut run = start(
        "tree-current",
        Setup {
            hooks: vec![reader(&kernels, &reads, body, true)],
            plugins: vec![kit::SEEN],
            presets: presets("[agents.presets.reviewer]\ndescription = \"Reviews\"\n"),
            kernels: kernels.clone(),
            ..Setup::default()
        },
        vec![
            ("/", vec![done("noted"); 4]),
            (
                "/a",
                vec![
                    uses(&[call("t1", kit::SEEN, json!({}))]),
                    uses(&[report_call("r1", "done")]),
                    done("bye"),
                ],
            ),
        ],
    )
    .await;
    run.spawn(SpawnRequest {
        preset: Some("reviewer".into()),
        ..spawn_request("a", "Look.")
    })
    .await;
    reported(&mut run, "/a").await;
    run.say("go").await;
    run.root_ended().await;
    let run = run.quit().await;
    let service = service_of(&kernels);
    let a = service
        .snapshot()
        .into_iter()
        .find(|e| e.path.as_str() == "/a")
        .unwrap();
    let root_session = service.tree().entries[&AgentPath::root()]
        .session_id
        .clone();
    let reads = reads.lock().unwrap().clone();
    let child: Vec<&Read> = reads
        .iter()
        .filter(|r| r.me.as_ref().is_some_and(|m| m.path == "/a"))
        .collect();
    assert!(!child.is_empty(), "{reads:#?}");
    for read in &child {
        assert_eq!(
            read.me,
            Some(Identity {
                path: "/a".into(),
                name: "a".into(),
                parent: Some("/".into()),
                preset: Some("reviewer".into()),
                depth: 1,
                session_id: a.session_id.clone(),
                root_session_id: root_session.clone(),
            })
        );
        assert_eq!(paths(&read.nodes), ["/a"]);
    }
    assert!(child.iter().any(|r| r.hook == "start"), "{reads:#?}");
    let root = |hook: &str| -> Vec<&Read> {
        reads
            .iter()
            .filter(|r| r.hook == hook && r.me.as_ref().is_some_and(|m| m.path == "/"))
            .collect()
    };
    let (root_turns, root_starts) = (root("turn"), root("start"));
    assert!(
        !root_turns.is_empty() && !root_starts.is_empty(),
        "{reads:#?}"
    );
    for read in root_turns.iter().chain(&root_starts) {
        assert_eq!(
            read.me,
            Some(Identity {
                path: "/".into(),
                name: "main".into(),
                parent: None,
                preset: None,
                depth: 0,
                session_id: root_session.clone(),
                root_session_id: root_session.clone(),
            })
        );
    }
    for read in &root_turns {
        assert_eq!(paths(&read.nodes), ["/a"]);
    }
    assert_eq!(
        reads.len(),
        child.len() + root_turns.len() + root_starts.len()
    );
    assert_eq!(
        result_of(&run.requests("/a")[1], "t1"),
        (false, "current /a; tree [/a]".to_owned())
    );
}

fn seen_description(request: &Sent) -> String {
    request
        .tools
        .iter()
        .find(|t| t.name == kit::SEEN)
        .map(|t| t.description.clone())
        .unwrap_or_else(|| panic!("no {} tool in {:?}", kit::SEEN, request.tool_names()))
}

fn seer_setup(kernels: &Kernels) -> Setup {
    Setup {
        hooks: vec![reader(kernels, &Reads::default(), "", true)],
        plugins: vec![kit::SEEN],
        kernels: kernels.clone(),
        ..Setup::default()
    }
}

#[tokio::test(start_paused = true)]
async fn describe_sees_no_agent_in_the_root_a_child_spawned_inside_its_call_or_that_child_resumed()
{
    let kernels = Kernels::default();
    let mut run = start(
        "tree-describe",
        seer_setup(&kernels),
        vec![
            (
                "/",
                vec![
                    uses(&[spawn_call("s1", "a", "Work.")]),
                    done("ok"),
                    done("noted"),
                    done("noted"),
                ],
            ),
            (
                "/a",
                vec![uses(&[report_call("r1", "a done")]), done("bye")],
            ),
            (
                "/b",
                vec![uses(&[report_call("r2", "b done")]), done("bye")],
            ),
        ],
    )
    .await;
    run.spawn(spawn_request("b", "Work.")).await;
    reported(&mut run, "/b").await;
    run.say("go").await;
    reported(&mut run, "/a").await;
    tokio::time::sleep(Duration::from_secs(30)).await;
    let run = run.quit().await;
    let root = &run.requests("/")[0];
    let a = &run.requests("/a")[0];
    let b = &run.requests("/b")[0];
    for request in [root, a, b] {
        assert_eq!(seen_description(request), SEES_NOTHING);
    }
    assert_eq!(
        crate::tool::canonical_specs(&a.tools),
        crate::tool::canonical_specs(&b.tools)
    );
    let sent = run.requests("/a").len();

    let back = resume(
        run,
        seer_setup(&Kernels::default()),
        vec![("/a", vec![done("again")])],
    )
    .await;
    assert!(back.live.contains(&"/a".to_owned()), "{:?}", back.live);
    let mut run = back.run;
    run.send(child_send("/a", "you-1", "Once more.")).await;
    run.child_ended("/a").await;
    let run = run.quit().await;
    let requests = run.requests("/a");
    assert_eq!(requests.len(), sent + 1);
    assert_eq!(seen_description(&requests[sent]), SEES_NOTHING);
}

#[tokio::test(start_paused = true)]
async fn the_empty_caller_alone_keeps_the_tree_from_a_build_doctor_or_prepare_inside_a_call() {
    use crate::plugin::caller::Caller;
    let kernels = Kernels::default();
    let mut run = start(
        "tree-empty-caller",
        Setup {
            kernels: kernels.clone(),
            digests: true,
            ..Setup::default()
        },
        vec![
            ("/", vec![done("noted")]),
            ("/a", vec![hanging("working")]),
            ("/b", vec![hanging("working")]),
        ],
    )
    .await;
    for (name, keep_warm) in [("a", false), ("b", true)] {
        run.spawn(SpawnRequest {
            keep_warm,
            ..spawn_request(name, "Work.")
        })
        .await;
        let agent = format!("/{name}");
        run.until("the child to stream", |e| {
            matches!(inner(e, &agent), Some(AgentEvent::TextDelta { .. }))
        })
        .await;
    }
    let service = service_of(&kernels);
    let kernel = Arc::new(crate::plugin::KernelHost::new());
    kernel.set_tree(service.handle(path("/a")));
    let built: Arc<StdMutex<Vec<String>>> = Arc::default();
    let make: crate::plugin::Make = {
        let built = built.clone();
        let inner = kit::seeing(kit::none(), Calls::default());
        Arc::new(move |host: Arc<dyn wisp_plugin_api::host::Host>| {
            built.lock().unwrap().push(kit::seen(host.as_ref()));
            inner(host)
        })
    };
    let manifest = kit::manifest_in(
        "0.2.0",
        "seer",
        &format!("\n[[tools]]\nname = \"{}\"\n", kit::SEEN),
    );
    let (registry, problems) = crate::plugin::Registry::from_sources(
        vec![(manifest, make)],
        &crate::plugin::PluginConfig::default(),
    );
    assert!(problems.is_empty(), "{problems:?}");
    let on = std::collections::BTreeSet::from([kit::SEEN.to_owned()]);
    let token = tokio_util::sync::CancellationToken::new();
    let started = Caller::call(token.clone(), None)
        .scope(registry.start(&on, kernel.clone()))
        .await;
    let described = started
        .toolbox
        .get(kit::SEEN)
        .map(|t| t.spec().description.clone());
    assert_eq!(described.as_deref(), Some(SEES_NOTHING));
    let tool = started.toolbox.get(kit::SEEN).unwrap();
    let input = json!({});
    let prepared = Caller::call(token.clone(), None)
        .scope(tool.prepare(&input))
        .await
        .unwrap();
    assert_eq!(
        prepared.display().action,
        wisp_protocol::ToolAction::Other {
            name: SEES_NOTHING.to_owned()
        }
    );
    let rows = Caller::call(token.clone(), None)
        .scope(registry.doctor(&on, kernel.clone()))
        .await;
    assert!(rows.is_empty());
    assert_eq!(
        *built.lock().unwrap(),
        ["current none; tree []", "current none; tree []"]
    );
    let states = |world, caller: Caller| {
        let host = crate::plugin::PluginHost::new("seer", world, &[], kernel.clone());
        caller.sync_scope(|| wisp_plugin_api::tree::AgentStates::states(&host))
    };
    let working = |path: &str, keep_warm| wisp_plugin_api::tree::ShownState {
        path: path.to_owned(),
        state: wisp_plugin_api::slots::NodeState::Working,
        keep_warm,
    };
    let fresh = wisp_plugin_api::host::WarmRow {
        refreshes: 0,
        limit: Limits::default().warm_refreshes,
        paused: false,
    };
    assert_eq!(
        states("0.3.0", Caller::call(token.clone(), None)),
        [working("/a", None), working("/b", Some(fresh))]
    );
    assert_eq!(states("0.2.0", Caller::call(token.clone(), None)), []);
    assert_eq!(states("0.3.0", Caller::default()), []);
    let host = crate::plugin::PluginHost::new("seer", "0.2.0", &[], kernel);
    let in_a_call = Caller::call(token, None).sync_scope(|| kit::seen(&host));
    assert_eq!(in_a_call, "current /a; tree [/a, /b]");
    let _ = run.quit().await;
}

#[tokio::test(start_paused = true)]
async fn since_ms_is_the_spawn_time_and_moves_only_when_the_state_changes() {
    const SPAWNED: i64 = 1_800_000_000_000;
    const STREAMED: i64 = 1_800_000_060_000;
    const SETTLED: i64 = 1_800_000_120_000;
    let kernels = Kernels::default();
    let clock = Arc::new(ShiftedClock::at(Timestamp::UNIX_EPOCH, None));
    clock.set(Timestamp::from_millisecond(SPAWNED - 1_000).unwrap());
    let first = Arc::new(Semaphore::new(0));
    let second = Arc::new(Semaphore::new(0));
    let mut run = start(
        "tree-since",
        Setup {
            clock: Some(clock.clone()),
            kernels: kernels.clone(),
            ..Setup::default()
        },
        vec![
            ("/", vec![done("noted"); 2]),
            (
                "/a",
                vec![
                    gated(
                        uses(&[call("l1", wisp_protocol::agents::LIST_AGENTS, json!({}))]),
                        &first,
                    ),
                    gated(uses(&[report_call("r1", "done")]), &second),
                ],
            ),
        ],
    )
    .await;
    clock.set(Timestamp::from_millisecond(SPAWNED).unwrap());
    run.spawn(spawn_request("a", "Work.")).await;
    run.until("/a's spawn", |e| {
        matches!(e, AgentEvent::AgentSpawned { .. })
    })
    .await;
    let service = service_of(&kernels);
    assert_eq!(since_ms(&service, "/a"), SPAWNED as u64);
    clock.set(Timestamp::from_millisecond(STREAMED).unwrap());
    first.add_permits(1);
    run.until("/a in its tool", |e| {
        matches!(e, AgentEvent::AgentStateChanged { agent, activity: Activity::Tool { .. }, .. } if agent.as_str() == "/a")
    })
    .await;
    let spend = run.until("/a's spend", |e| {
        matches!(e, AgentEvent::AgentStateChanged { agent, state: AgentState::Running, spent_usd, .. } if agent.as_str() == "/a" && *spent_usd > 0.0)
    })
    .await;
    assert_eq!(since_ms(&service, "/a"), SPAWNED as u64);
    assert!(
        matches!(spend, AgentEvent::AgentStateChanged { since_ms: Some(at), .. } if at == SPAWNED as u64)
    );
    clock.set(Timestamp::from_millisecond(SETTLED).unwrap());
    second.add_permits(1);
    let idle = run.until("/a to go idle", |e| {
        matches!(e, AgentEvent::AgentStateChanged { agent, state: AgentState::Idle { .. }, .. } if agent.as_str() == "/a")
    })
    .await;
    assert_eq!(since_ms(&service, "/a"), SETTLED as u64);
    assert!(
        matches!(idle, AgentEvent::AgentStateChanged { since_ms: Some(at), .. } if at == SETTLED as u64)
    );
    let entry = service
        .snapshot()
        .into_iter()
        .find(|e| e.path.as_str() == "/a");
    assert_eq!(entry.and_then(|e| e.since_ms), Some(SETTLED as u64));
    let _ = run.quit().await;
}

#[tokio::test(start_paused = true)]
async fn a_child_gets_no_lane_and_its_marks_render_nothing() {
    let calls = Calls::default();
    let body = "\n[[points]]\nname = \"ready\"\nkind = \"observe\"\nversion = \"1\"\n\n[[hooks]]\nname = \"start\"\npoint = \"session-start\"\n".to_owned()
        + &kit::section("sec", "marks = [\"hooked/ready\"]\n");
    let make: crate::plugin::Make = kit::sections(kit::fires("ready"), kit::views(), calls.clone());
    let mut run = start(
        "hooks-child-no-lane",
        Setup {
            hooks: vec![(kit::slotted("hooked", &body), make)],
            ..Setup::default()
        },
        vec![
            (
                "/review",
                vec![uses(&[report_call("r1", "Looked at it")]), done("bye")],
            ),
            ("/", vec![done("noted")]),
        ],
    )
    .await;
    run.send(ClientCommand::SlotsShown {
        sections: vec!["hooked/sec".to_owned()],
        pages: Vec::new(),
    })
    .await;
    run.until("the first render", |e| {
        matches!(e, AgentEvent::SlotContent { .. })
    })
    .await;
    tokio::time::sleep(Duration::from_secs(5)).await;
    let before = calls.renders().len();
    run.spawn(spawn_request("review", "Review the change."))
        .await;
    assert!(matches!(
        run.child_ended("/review").await,
        TurnOutcome::Completed { .. }
    ));
    run.root_ended().await;
    tokio::time::sleep(Duration::from_secs(5)).await;
    let rendered = calls.renders().len();
    run.quit().await;
    let starts = calls.of("start");
    let instances: std::collections::BTreeSet<usize> = starts.iter().map(|c| c.instance).collect();
    assert_eq!(instances.len(), 2, "{starts:?}");
    let root = starts[0].instance;
    assert_eq!(rendered, before);
    assert!(
        calls.render_instances().iter().all(|i| *i == root),
        "{:?}",
        calls.render_instances()
    );
}

fn change(request: u64, name: &str, value: wisp_protocol::settings::SettingValue) -> ClientCommand {
    let change = wisp_protocol::settings::SettingChange::named(name, value).unwrap();
    ClientCommand::ChangeSettings {
        request: RequestId(request),
        changes: vec![change],
        save: false,
    }
}

fn notice_off(request: u64) -> ClientCommand {
    use wisp_protocol::settings::{
        PLUGIN_HOOK, Place, SettingChange, SettingTarget, SitePath, find,
    };
    let path = SitePath::new(
        Place::PluginHook {
            plugin: "shell".to_owned(),
            hook: "processes".to_owned(),
        },
        find(PLUGIN_HOOK).unwrap(),
    );
    ClientCommand::ChangeSettings {
        request: RequestId(request),
        changes: vec![SettingChange {
            target: SettingTarget::Path(path),
            value: Some(wisp_protocol::settings::SettingValue::Bool(false)),
        }],
        save: false,
    }
}

async fn listed_after_auto_compact_on(name: &str, shown: bool, prompt_sections: bool) -> String {
    use wisp_protocol::settings::SettingValue;
    let child_gate = Arc::new(Semaphore::new(0));
    let root_gate = Arc::new(Semaphore::new(0));
    let body = kit::section("sec", &kit::every_point());
    let make: crate::plugin::Make = kit::sections(kit::none(), kit::views(), Calls::default());
    let mut run = start(
        name,
        Setup {
            hooks: vec![(kit::slotted("hooked", &body), make)],
            prompt_sections,
            ..Setup::default()
        },
        vec![
            (
                "/peek",
                vec![
                    gated(uses(&[call("l1", "list_agents", json!({}))]), &child_gate),
                    uses(&[report_call("r1", "Looked")]),
                    done("bye"),
                ],
            ),
            (
                "/",
                vec![
                    done("one"),
                    gated(done("two"), &root_gate),
                    done("noted"),
                    done("noted"),
                ],
            ),
        ],
    )
    .await;
    if shown {
        run.send(ClientCommand::SlotsShown {
            sections: vec!["hooked/sec".to_owned()],
            pages: Vec::new(),
        })
        .await;
    }
    run.send(change(70, "compact_at", SettingValue::Tokens(400_000)))
        .await;
    run.send(notice_off(69)).await;
    run.say("go").await;
    run.root_ended().await;
    run.send(change(71, "context.auto_compact", SettingValue::Bool(true)))
        .await;
    run.until("the auto_compact change", |e| {
        matches!(
            e,
            AgentEvent::SettingsChanged {
                answers: Some(RequestId(71)),
                ..
            }
        )
    })
    .await;
    run.spawn(spawn_request("peek", "Look around.")).await;
    run.say("again").await;
    assert!(
        until(|| run.requests("/").len() >= 2).await,
        "no second request"
    );
    child_gate.add_permits(10);
    assert!(
        until(|| run.requests("/peek").len() >= 2).await,
        "no second request from /peek"
    );
    let listed = run.requests("/peek")[1].last_user_texts().join("\n");
    root_gate.add_permits(10);
    run.child_ended("/peek").await;
    run.quit().await;
    listed
}

#[tokio::test(start_paused = true)]
async fn after_auto_compact_is_turned_on_list_agents_gives_the_roots_last_context_changed() {
    for (name, shown) in [("auto-on-hidden", false), ("auto-on-shown", true)] {
        let listed = listed_after_auto_compact_on(name, shown, false).await;
        let parent = listed
            .lines()
            .find(|line| line.contains("(your parent)"))
            .unwrap();
        assert!(
            parent.contains("context 100 of 1M, limit 400k"),
            "{name}: {listed}"
        );
    }
}

#[tokio::test(start_paused = true)]
async fn with_a_prompt_section_list_agents_gives_the_limit_the_roots_opening_published() {
    for (name, shown) in [("section-hidden", false), ("section-shown", true)] {
        let listed = listed_after_auto_compact_on(name, shown, true).await;
        let parent = listed
            .lines()
            .find(|line| line.contains("(your parent)"))
            .unwrap();
        assert!(
            parent.contains("context 100 of 1M, limit 400k"),
            "{name}: {listed}"
        );
    }
}

fn seen_from(
    caller: crate::plugin::caller::Caller,
    kernel: &Arc<crate::plugin::KernelHost>,
) -> String {
    let host = crate::plugin::PluginHost::new("hooked", "0.2.0", &[], kernel.clone());
    caller.sync_scope(|| kit::seen(&host))
}

#[tokio::test(start_paused = true)]
async fn a_sidebar_render_sees_the_agent_and_the_tree_and_a_start_render_or_a_build_sees_nothing() {
    use crate::plugin::caller::{Caller, Render};
    let kernels = Kernels::default();
    let rendered: kit::Seen = Arc::default();
    let render: kit::Render = {
        let rendered = rendered.clone();
        Arc::new(move |host, _, _| {
            let words = kit::seen(host.as_ref());
            rendered.lock().unwrap().push(words.clone());
            Box::pin(async move { Ok(kit::line(&words)) })
        })
    };
    let make: crate::plugin::Make = kit::sections(kit::none(), render, Calls::default());
    let mut run = start(
        "tree-sidebar-render",
        Setup {
            hooks: vec![(kit::slotted("hooked", &kit::section("sec", "")), make)],
            kernels: kernels.clone(),
            ..Setup::default()
        },
        vec![
            (
                "/review",
                vec![uses(&[report_call("r1", "Looked at it")]), done("bye")],
            ),
            ("/", vec![done("noted")]),
        ],
    )
    .await;
    run.spawn(spawn_request("review", "Review the change."))
        .await;
    run.child_ended("/review").await;
    run.root_ended().await;
    run.send(ClientCommand::SlotsShown {
        sections: vec!["hooked/sec".to_owned()],
        pages: Vec::new(),
    })
    .await;
    run.until("the render", |e| {
        matches!(e, AgentEvent::SlotContent { .. })
    })
    .await;
    let sees_all = "current /; tree [/review]";
    let sees_nothing = "current none; tree []";
    let renders = rendered.lock().unwrap().clone();
    assert!(
        !renders.is_empty() && renders.iter().all(|r| r == sees_all),
        "{renders:?}"
    );
    let root = kernels
        .lock()
        .unwrap()
        .iter()
        .find(|k| k.tree().path().as_str() == "/")
        .cloned()
        .unwrap();
    let start_render = Caller {
        cancel: Some(tokio_util::sync::CancellationToken::new()),
        ..Caller::rendering(Render::Start)
    };
    assert_eq!(seen_from(Caller::rendering(Render::Slot), &root), sees_all);
    assert_eq!(seen_from(start_render, &root), sees_nothing);
    assert_eq!(seen_from(Caller::default(), &root), sees_nothing);
    let old_world = crate::plugin::PluginHost::new("hooked", "0.1.0", &[], root.clone());
    assert_eq!(
        Caller::rendering(Render::Slot).sync_scope(|| kit::seen(&old_world)),
        sees_nothing
    );
    let no_tree = Arc::new(crate::plugin::KernelHost::new());
    assert_eq!(
        seen_from(Caller::rendering(Render::Slot), &no_tree),
        sees_nothing
    );
    run.quit().await;
}

#[derive(Debug, Clone)]
pub(super) struct Heard {
    pub(super) hook: String,
    pub(super) event: Option<TreeEvent>,
    pub(super) me: Option<Identity>,
    pub(super) tree: usize,
    pub(super) at: tokio::time::Instant,
}

pub(super) type Heards = Arc<StdMutex<Vec<Heard>>>;

pub(super) type Gates = Vec<(&'static str, Arc<Semaphore>)>;

pub(super) fn agent_hooks(extra: &str) -> String {
    AGENT_POINTS
        .iter()
        .map(|p| format!("\n[[hooks]]\nname = \"{p}\"\npoint = \"{p}\"\n{extra}"))
        .collect()
}

fn held(gates: &Gates, hook: &str) -> BoxFuture<'static, ()> {
    let gate = gates
        .iter()
        .find(|(name, _)| *name == hook)
        .map(|(_, gate)| gate.clone());
    Box::pin(async move {
        if let Some(gate) = gate {
            let _ = gate.acquire().await.map(|p| p.forget());
        }
    })
}

fn hear(heard: &Heards, hook: String, event: Option<TreeEvent>, host: &dyn Host) {
    let tree = if event.is_some() {
        host.snapshot().len()
    } else {
        0
    };
    heard.lock().unwrap().push(Heard {
        hook,
        event,
        me: host.current_agent(),
        tree,
        at: tokio::time::Instant::now(),
    });
}

pub(super) fn listener(heard: &Heards, body: &str, gates: Gates) -> (String, crate::plugin::Make) {
    let gates = Arc::new(gates);
    let watch: kit::Watch = {
        let heard = heard.clone();
        let gates = gates.clone();
        Arc::new(move |host, hook, event| {
            hear(&heard, hook.clone(), Some(event), host.as_ref());
            held(&gates, &hook)
        })
    };
    let script: kit::Script = {
        let heard = heard.clone();
        Arc::new(move |host, hook, _| {
            hear(&heard, hook.clone(), None, host.as_ref());
            let wait = held(&gates, &hook);
            let heard = heard.clone();
            Box::pin(async move {
                wait.await;
                hear(&heard, format!("{hook} done"), None, host.as_ref());
                wisp_plugin_api::hooks::Reply::None
            })
        })
    };
    (
        kit::manifest_in("0.2.0", "watcher", body),
        kit::observing(script, watch, Calls::default()),
    )
}

pub(super) fn heard_events(heard: &Heards) -> Vec<TreeEvent> {
    heard
        .lock()
        .unwrap()
        .iter()
        .filter_map(|h| h.event.clone())
        .collect()
}

pub(super) async fn heard_until(heard: &Heards, what: &str, want: impl Fn(&[TreeEvent]) -> bool) {
    assert!(
        until(|| want(&heard_events(heard))).await,
        "never heard {what}: {:#?}",
        heard_events(heard)
    );
}

fn concerns(event: &TreeEvent, agent: &str) -> bool {
    match event {
        TreeEvent::AgentSpawned(node) => node.path == agent,
        TreeEvent::AgentState(s) => s.agent == agent,
        TreeEvent::AgentMail(m) => m.from == agent || m.to == agent,
        TreeEvent::AgentReported(r) => r.agent == agent,
        TreeEvent::AgentStopped(s) | TreeEvent::AgentEnded(s) => s.agent == agent,
    }
}

pub(super) fn about<'a>(events: &'a [TreeEvent], agent: &str) -> Vec<&'a TreeEvent> {
    events.iter().filter(|e| concerns(e, agent)).collect()
}

fn last_spend(events: &[TreeEvent], agent: &str) -> Option<u64> {
    events.iter().rev().find_map(|e| match e {
        TreeEvent::AgentState(s) if s.agent == agent => Some(s.spent_micros),
        _ => None,
    })
}

fn stop_info(agent: &str, target: &str) -> StopInfo {
    StopInfo {
        agent: agent.to_owned(),
        target: target.to_owned(),
        by: None,
    }
}

fn mails_logged(run: &Run, paths: &[&str], from_seq: &BTreeMap<String, usize>) -> Vec<String> {
    let mut out = Vec::new();
    for agent in paths {
        let skip = from_seq.get(*agent).copied().unwrap_or(0);
        for event in run.records(agent).into_iter().skip(skip) {
            if let LogEvent::InputAccepted {
                from: Some(from),
                text,
                ..
            } = event
            {
                let frame = crate::agent::render_mail(&from.agent, from.kind, "").len();
                out.push(format!(
                    "{} {} {agent} {:?} {} {}",
                    from.mail,
                    from.agent,
                    from.kind,
                    from.wake,
                    text.len() - frame
                ));
            }
        }
    }
    out.sort();
    out
}

fn mails_heard(events: &[TreeEvent]) -> Vec<String> {
    let mut out: Vec<String> = pick!(
        events,
        TreeEvent::AgentMail(m) => format!(
            "{} {} {} {:?} {} {}",
            m.id, m.from, m.to, m.kind, m.wake, m.bytes
        )
    );
    out.sort();
    out
}

fn lane_runs(run: &Run) -> Vec<(String, String, HookOutcome, u64)> {
    run.records("/")
        .into_iter()
        .filter_map(|e| match e {
            LogEvent::HookRan {
                hook,
                point,
                turn,
                step,
                call_id,
                outcome,
                ms,
            } if AGENT_POINTS.contains(&point.as_str()) && hook.starts_with("watcher/") => {
                assert_eq!((turn, step, call_id), (None, None, None), "{hook}");
                Some((hook, point, outcome, ms))
            }
            _ => None,
        })
        .collect()
}

async fn logged_end(run: &Run, agent: &str) {
    for _ in 0..600 {
        if run
            .records(agent)
            .iter()
            .any(|e| matches!(e, LogEvent::SessionEnded { .. }))
        {
            return;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    panic!("{agent}'s log never ended");
}

#[tokio::test(start_paused = true)]
async fn the_six_agent_points_fire_in_the_roots_instance_in_order_and_carry_no_text() {
    let heard = Heards::default();
    let kernels = Kernels::default();
    let old = Calls::default();
    let mut run = start(
        "points-six",
        Setup {
            hooks: vec![listener(&heard, &agent_hooks(""), Vec::new()), hooked(&old)],
            kernels: kernels.clone(),
            ..Setup::default()
        },
        vec![
            ("/", vec![done("noted"); 8]),
            (
                "/a",
                vec![
                    uses(&[send_call("m1", "../b", "Read MARKER-message.")]),
                    uses(&[report_call("r1", "a found MARKER-report")]),
                    done("bye"),
                ],
            ),
            (
                "/b",
                vec![
                    uses(&[spawn_call("s1", "c", "Check MARKER-brief.")]),
                    hanging("thinking"),
                ],
            ),
            (
                "/b/c",
                vec![
                    uses(&[report_call("r2", "c done MARKER-report")]),
                    done("bye"),
                ],
            ),
        ],
    )
    .await;
    run.spawn(spawn_request("a", "Lead MARKER-brief.")).await;
    run.spawn(spawn_request("b", "Help MARKER-brief.")).await;
    reported(&mut run, "/a").await;
    reported(&mut run, "/b/c").await;
    idle(&mut run, "/b/c").await;
    streaming(&mut run, "/b").await;
    run.send(cancel("/b")).await;
    run.child_ended("/b").await;
    run.send(ClientCommand::EndAgent { agent: path("/b") })
        .await;
    heard_until(&heard, "both ends", |e| {
        e.iter()
            .filter(|e| matches!(e, TreeEvent::AgentEnded(_)))
            .count()
            == 2
    })
    .await;
    tokio::time::sleep(Duration::from_secs(30)).await;
    let run = run.quit().await;
    let service = service_of(&kernels);
    let root_session = service.tree().entries[&AgentPath::root()]
        .session_id
        .clone();
    let all = heard.lock().unwrap().clone();
    for h in all.iter().filter(|h| h.event.is_some()) {
        let me =
            h.me.as_ref()
                .map(|m| (m.path.as_str(), m.session_id.as_str()));
        assert_eq!(me, Some(("/", root_session.as_str())), "{h:?}");
    }
    let events = heard_events(&heard);
    assert!(!format!("{events:?}").contains("MARKER"), "{events:#?}");
    for agent in ["/a", "/b", "/b/c"] {
        let mine = about(&events, agent);
        assert!(
            matches!(mine.first(), Some(TreeEvent::AgentSpawned(n)) if n.path == agent),
            "{agent}: {mine:#?}"
        );
        let spawned = mine
            .iter()
            .filter(|e| matches!(e, TreeEvent::AgentSpawned(_)))
            .count();
        assert_eq!(spawned, 1, "{agent}");
    }
    let a = service
        .snapshot()
        .into_iter()
        .find(|e| e.path.as_str() == "/a")
        .unwrap();
    assert!(a.spent_usd > 0.0);
    assert_eq!(
        last_spend(&events, "/a"),
        Some(crate::pricing::micros(a.spent_usd))
    );
    let paths = ["/", "/a", "/b", "/b/c"];
    let logged = mails_logged(&run, &paths, &BTreeMap::new());
    assert_eq!(mails_heard(&events), logged);
    for kind in ["Task", "Message", "Report", "Notice"] {
        assert!(
            logged.iter().any(|m| m.contains(&format!(" {kind} "))),
            "{kind}: {logged:#?}"
        );
    }
    assert!(
        logged.iter().any(|m| m.ends_with(&format!(
            " /a /b Message true {}",
            "Read MARKER-message.".len()
        ))),
        "{logged:#?}"
    );
    let mut reports: Vec<ReportInfo> = pick!(
        &run.seen,
        AgentEvent::AgentReported {
            agent,
            turn,
            outcome,
            cost,
            duration_ms,
            ..
        } => ReportInfo {
            agent: agent.to_string(),
            turn: *turn,
            outcome: match outcome {
                ReportOutcome::Reported => tree::ReportOutcome::Reported,
                ReportOutcome::NoReport => tree::ReportOutcome::NoReport,
                ReportOutcome::InvalidReport => tree::ReportOutcome::InvalidReport,
                ReportOutcome::Unknown => tree::ReportOutcome::Unknown,
            },
            cost_micros: cost.map(crate::pricing::micros),
            duration_ms: *duration_ms,
        }
    );
    let mut heard_reports: Vec<ReportInfo> = pick!(
        &events,
        TreeEvent::AgentReported(r) => r.clone()
    );
    reports.sort_by(|x, y| x.agent.cmp(&y.agent));
    heard_reports.sort_by(|x, y| x.agent.cmp(&y.agent));
    assert_eq!(heard_reports, reports);
    assert_eq!(heard_reports.len(), 2, "{heard_reports:#?}");
    assert!(
        heard_reports
            .iter()
            .all(|r| r.outcome == tree::ReportOutcome::Reported && r.cost_micros.is_some())
    );
    let stopped: Vec<&StopInfo> = pick!(&events, TreeEvent::AgentStopped(s) => s);
    assert_eq!(stopped, [&stop_info("/b", "/b")]);
    let ended: Vec<&StopInfo> = pick!(&events, TreeEvent::AgentEnded(s) => s);
    assert_eq!(ended, [&stop_info("/b", "/b"), &stop_info("/b/c", "/b")]);
    let b = about(&events, "/b");
    let at = b
        .iter()
        .position(|e| matches!(e, TreeEvent::AgentStopped(_)))
        .unwrap();
    assert!(
        b[..at].iter().any(|e| matches!(
            e,
            TreeEvent::AgentState(s) if s.state == tree::AgentState::Stopping
        )),
        "{b:#?}"
    );
    for agent in ["/b", "/b/c"] {
        let mine = about(&events, agent);
        let at = mine
            .iter()
            .position(|e| matches!(e, TreeEvent::AgentEnded(_)))
            .unwrap();
        assert!(
            matches!(mine[at - 1], TreeEvent::AgentState(s) if s.state == tree::AgentState::Ended),
            "{agent}: {mine:#?}"
        );
        for later in &mine[at + 1..] {
            match later {
                TreeEvent::AgentState(s) => assert_eq!(s.state, tree::AgentState::Ended),
                TreeEvent::AgentReported(_) | TreeEvent::AgentMail(_) => {}
                other => panic!("{agent} after its end: {other:?}"),
            }
        }
    }
    let runs = lane_runs(&run);
    for point in AGENT_POINTS {
        assert!(
            runs.iter().any(|(hook, p, outcome, _)| p == point
                && *hook == format!("watcher/{point}")
                && *outcome == HookOutcome::None),
            "{point}: {runs:#?}"
        );
    }
    for child in ["/a", "/b", "/b/c"] {
        let records = run.records(child);
        let ran = |name: &str| {
            records
                .iter()
                .any(|e| matches!(e, LogEvent::HookRan { hook, .. } if hook == name))
        };
        assert!(ran("hooked/start") && ran("hooked/turn"), "{child}");
        assert!(
            !records.iter().any(|e| matches!(
                e,
                LogEvent::HookRan { point, .. } if AGENT_POINTS.contains(&point.as_str())
            )),
            "{child}"
        );
    }
}

#[tokio::test(start_paused = true)]
async fn after_agent_ended_come_only_its_ended_spend_its_report_and_its_mail() {
    let heard = Heards::default();
    let kernels = Kernels::default();
    let mut run = start(
        "points-after-end",
        Setup {
            hooks: vec![listener(&heard, &agent_hooks(""), Vec::new())],
            kernels: kernels.clone(),
            ..Setup::default()
        },
        vec![
            ("/", vec![done("noted"); 4]),
            (
                "/w",
                vec![
                    uses(&[spawn_call("s1", "g", "Gather.")]),
                    hanging("working"),
                ],
            ),
            (
                "/w/g",
                vec![uses(&[report_call("r1", "g done")]), done("bye")],
            ),
        ],
    )
    .await;
    run.spawn(spawn_request("w", "Work.")).await;
    reported(&mut run, "/w/g").await;
    idle(&mut run, "/w/g").await;
    streaming(&mut run, "/w").await;
    run.send(ClientCommand::EndAgent { agent: path("/w") })
        .await;
    logged_end(&run, "/w").await;
    logged_end(&run, "/w/g").await;
    let service = service_of(&kernels);
    let row = service
        .snapshot()
        .into_iter()
        .find(|e| e.path.as_str() == "/w")
        .unwrap();
    let want = crate::pricing::micros(row.spent_usd);
    heard_until(&heard, "/w's priced end", |e| {
        last_spend(e, "/w") == Some(want)
    })
    .await;
    let _ = run.quit().await;
    let events = heard_events(&heard);
    for agent in ["/w", "/w/g"] {
        let mine = about(&events, agent);
        let ends: Vec<usize> = mine
            .iter()
            .enumerate()
            .filter(|(_, e)| matches!(e, TreeEvent::AgentEnded(_)))
            .map(|(i, _)| i)
            .collect();
        assert_eq!(ends.len(), 1, "{agent}: {mine:#?}");
        let at = ends[0];
        assert_eq!(mine[at], &TreeEvent::AgentEnded(stop_info(agent, "/w")));
        assert!(
            matches!(mine[at - 1], TreeEvent::AgentState(s) if s.state == tree::AgentState::Ended)
        );
        assert!(!mine.iter().any(|e| matches!(e, TreeEvent::AgentStopped(_))));
        for later in &mine[at + 1..] {
            match later {
                TreeEvent::AgentState(s) => assert_eq!(s.state, tree::AgentState::Ended),
                TreeEvent::AgentReported(_) | TreeEvent::AgentMail(_) => {}
                other => panic!("{agent} after its end: {other:?}"),
            }
        }
    }
    let w = about(&events, "/w");
    let at = w
        .iter()
        .position(|e| matches!(e, TreeEvent::AgentEnded(_)))
        .unwrap();
    let TreeEvent::AgentState(at_end) = w[at - 1] else {
        unreachable!()
    };
    let later: Vec<u64> = pick!(&w[at + 1..], TreeEvent::AgentState(s) => s.spent_micros);
    assert!(later.iter().any(|s| *s > at_end.spent_micros), "{w:#?}");
    assert_eq!(later.last(), Some(&want));
    assert!(
        w[at + 1..].iter().any(|e| matches!(
            e,
            TreeEvent::AgentMail(m) if m.from == "/w" && m.to == "/" && m.kind == tree::MailKind::Notice
        )),
        "{w:#?}"
    );
}

#[tokio::test(start_paused = true)]
async fn a_report_held_at_mailing_while_its_agent_is_ended_fires_after_agent_ended() {
    let heard = Heards::default();
    let mut run = start(
        "points-report-after-end",
        Setup {
            hooks: vec![listener(&heard, &agent_hooks(""), Vec::new())],
            ..Setup::default()
        },
        vec![
            ("/", vec![done("noted"); 4]),
            (
                "/r",
                vec![uses(&[report_call("r1", "r done")]), done("bye")],
            ),
        ],
    )
    .await;
    let mailing = pause_at("/r", "mailing");
    run.spawn(spawn_request("r", "Report.")).await;
    within("the mailing pause", mailing.wait()).await;
    run.send(ClientCommand::EndAgent { agent: path("/r") })
        .await;
    heard_until(&heard, "/r's end", |e| {
        e.iter()
            .any(|e| matches!(e, TreeEvent::AgentEnded(s) if s.agent == "/r"))
    })
    .await;
    within("the mailing pause", mailing.wait()).await;
    reported(&mut run, "/r").await;
    heard_until(&heard, "/r's report mail", |e| {
        e.iter().any(|e| {
            matches!(e, TreeEvent::AgentMail(m) if m.from == "/r" && m.kind == tree::MailKind::Report)
        })
    })
    .await;
    let _ = run.quit().await;
    let events = heard_events(&heard);
    let r = about(&events, "/r");
    let ended = r
        .iter()
        .position(|e| matches!(e, TreeEvent::AgentEnded(_)))
        .unwrap();
    let reported = r
        .iter()
        .position(|e| {
            matches!(e, TreeEvent::AgentReported(x) if x.outcome == tree::ReportOutcome::Reported)
        })
        .unwrap();
    let mailed = r
        .iter()
        .position(|e| {
            matches!(e, TreeEvent::AgentMail(m) if m.kind == tree::MailKind::Report && m.to == "/")
        })
        .unwrap();
    assert!(ended < reported && reported < mailed, "{r:#?}");
}

fn state_hook(extra: &str) -> String {
    format!("\n[[hooks]]\nname = \"agent-state\"\npoint = \"agent-state\"\n{extra}")
}

async fn blocked_lane(name: &str, release: bool) -> (Run, Heards, tokio::time::Instant) {
    let heard = Heards::default();
    let gate = Arc::new(Semaphore::new(0));
    let mut run = start(
        name,
        Setup {
            hooks: vec![listener(
                &heard,
                &state_hook(""),
                vec![("agent-state", gate.clone())],
            )],
            ..Setup::default()
        },
        vec![
            ("/", vec![done("noted"); 4]),
            (
                "/a",
                vec![uses(&[report_call("r1", "a done")]), done("bye")],
            ),
        ],
    )
    .await;
    run.spawn(spawn_request("a", "Work.")).await;
    assert!(matches!(
        run.child_ended("/a").await,
        TurnOutcome::Completed { .. }
    ));
    let ended = tokio::time::Instant::now();
    if release {
        gate.add_permits(Semaphore::MAX_PERMITS / 2);
    }
    run.root_ended().await;
    tokio::time::sleep(Duration::from_secs(60)).await;
    (run.quit().await, heard, ended)
}

#[tokio::test(start_paused = true)]
async fn a_blocked_agent_hook_holds_no_step_and_is_cut_at_two_seconds_per_item() {
    let (run, heard, ended) = blocked_lane("points-blocked", false).await;
    let all = heard.lock().unwrap().clone();
    let first = all.first().unwrap().at;
    assert!(
        ended < first + Duration::from_secs(2),
        "the step ended while the hook blocked"
    );
    let runs = lane_runs(&run);
    assert_eq!(runs.len(), all.len(), "{runs:#?}");
    assert!(runs.len() >= 2, "{runs:#?}");
    for (hook, point, outcome, ms) in &runs {
        assert_eq!(
            (hook.as_str(), point.as_str(), outcome, *ms),
            (
                "watcher/agent-state",
                "agent-state",
                &HookOutcome::TimedOut,
                2000
            )
        );
    }
    for pair in all.windows(2) {
        assert!(pair[1].at >= pair[0].at + Duration::from_secs(2));
    }
    let last = run.seen.iter().rev().find_map(|e| match e {
        AgentEvent::AgentStateChanged {
            agent,
            state: AgentState::Idle { .. },
            spent_usd,
            ..
        } if agent.as_str() == "/a" => Some(crate::pricing::micros(*spent_usd)),
        _ => None,
    });
    assert_eq!(last_spend(&heard_events(&heard), "/a"), last);
}

#[tokio::test(start_paused = true)]
async fn an_agent_hook_released_before_the_tree_idles_ends_none() {
    let (run, _, _) = blocked_lane("points-released", true).await;
    let runs = lane_runs(&run);
    assert!(!runs.is_empty());
    assert_eq!(runs[0].2, HookOutcome::None, "{runs:#?}");
    assert!(runs[0].3 < 2000, "{runs:#?}");
}

#[tokio::test(start_paused = true)]
async fn a_resumed_tree_fires_queued_points_after_session_start_and_none_once_session_end_starts() {
    let run = interrupted_tree("points-resume").await;
    sibling_mail(&run, "/lead", "/solo", "m90", "One more thing.");
    let paths = ["/", "/lead", "/lead/w", "/solo"];
    let before: BTreeMap<String, usize> = paths
        .iter()
        .map(|p| ((*p).to_owned(), run.records(p).len()))
        .collect();
    let heard = Heards::default();
    let start_gate = Arc::new(Semaphore::new(0));
    let end_gate = Arc::new(Semaphore::new(0));
    let lead_gate = Arc::new(Semaphore::new(0));
    let never = Arc::new(Semaphore::new(0));
    let body = agent_hooks("")
        + "\n[[hooks]]\nname = \"start\"\npoint = \"session-start\"\n"
        + "\n[[hooks]]\nname = \"end\"\npoint = \"session-end\"\n"
        + "\n[[hooks]]\nname = \"lingering\"\npoint = \"agent-mail\"\nbackground = true\ntimeout_ms = 600000\n";
    let back = resume(
        run,
        Setup {
            hooks: vec![listener(
                &heard,
                &body,
                vec![
                    ("start", start_gate.clone()),
                    ("end", end_gate.clone()),
                    ("lingering", never),
                ],
            )],
            ..Setup::default()
        },
        vec![
            (
                "/lead",
                vec![
                    gated(uses(&[report_call("r1", "w was interrupted")]), &lead_gate),
                    done("x"),
                ],
            ),
            ("/", vec![done("noted"); 3]),
        ],
    )
    .await;
    let mut run = back.run;
    run.until("the lead's notice", |e| {
        matches!(inner(e, "/lead"), Some(AgentEvent::InputAccepted { .. }))
    })
    .await;
    assert!(
        heard_events(&heard).is_empty(),
        "{:#?}",
        heard_events(&heard)
    );
    start_gate.add_permits(1);
    heard_until(&heard, "the lead's notice", |e| {
        e.iter()
            .any(|e| matches!(e, TreeEvent::AgentMail(m) if m.to == "/lead"))
    })
    .await;
    tokio::time::sleep(Duration::from_secs(30)).await;
    let logged = mails_logged(&run, &paths, &before);
    let mails = |heard: &Heards| {
        let events: Vec<TreeEvent> = heard
            .lock()
            .unwrap()
            .iter()
            .filter(|h| h.hook != "lingering")
            .filter_map(|h| h.event.clone())
            .collect();
        mails_heard(&events)
    };
    assert_eq!(mails(&heard), logged);
    let turns = |records: Vec<wisp_protocol::log::LogRecord>| {
        records
            .iter()
            .filter(|r| matches!(r.event, LogEvent::TurnEnded { .. }))
            .count()
    };
    let store = run.store.clone();
    let lead_log = session_name("/lead");
    let lead_turns = turns(store.read(&lead_log).unwrap());
    let ending = async {
        assert!(
            until(|| heard.lock().unwrap().iter().any(|h| h.hook == "end")).await,
            "no session-end hook"
        );
        lead_gate.add_permits(1);
        for _ in 0..100 {
            if turns(store.read(&lead_log).unwrap()) > lead_turns {
                break;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        end_gate.add_permits(1);
    };
    let (run, ()) = tokio::join!(run.quit(), ending);
    tokio::time::sleep(Duration::from_secs(30)).await;
    assert!(
        turns(run.store.read(&lead_log).unwrap()) > lead_turns,
        "the lead's turn ran while session-end's hook held"
    );
    let all = heard.lock().unwrap().clone();
    let hooks: Vec<&str> = all.iter().map(|h| h.hook.as_str()).collect();
    for child in ["/lead", "/lead/w", "/solo"] {
        assert!(
            all.iter().any(|h| h.hook == "start"
                && h.me
                    .as_ref()
                    .is_some_and(|m| m.path == child && m.parent.is_some())),
            "{child}: {hooks:?}"
        );
    }
    let started = hooks.iter().position(|h| *h == "start done").unwrap();
    let first = all.iter().position(|h| h.event.is_some()).unwrap();
    assert!(started < first, "{hooks:?}");
    let ending = hooks.iter().position(|h| *h == "end").unwrap();
    let last = all.iter().rposition(|h| h.event.is_some()).unwrap();
    assert!(last < ending, "{hooks:?}");
    let events: Vec<TreeEvent> = all
        .iter()
        .filter(|h| h.hook != "lingering")
        .filter_map(|h| h.event.clone())
        .collect();
    assert!(
        !events
            .iter()
            .any(|e| matches!(e, TreeEvent::AgentMail(m) if m.id == "m90")),
        "{events:#?}"
    );
    assert!(!logged.is_empty());
    assert_eq!(mails_heard(&events), logged);
    assert!(mails_logged(&run, &paths, &before).len() > logged.len());
    let root = run.records("/");
    let lingering = root
        .iter()
        .position(|e| {
            matches!(e, LogEvent::HookRan { hook, outcome: HookOutcome::Cancelled, .. } if hook == "watcher/lingering")
        })
        .unwrap();
    let ended = root
        .iter()
        .rposition(|e| matches!(e, LogEvent::SessionEnded { .. }))
        .unwrap();
    assert!(lingering < ended);
    assert!(!lane_runs(&run).is_empty());
}

#[tokio::test(start_paused = true)]
async fn a_mail_delivered_twice_fires_one_agent_mail() {
    let heard = Heards::default();
    let kernels = Kernels::default();
    let mut run = start(
        "points-duplicate-mail",
        Setup {
            hooks: vec![listener(&heard, &agent_hooks(""), Vec::new())],
            kernels: kernels.clone(),
            ..Setup::default()
        },
        vec![
            ("/", vec![done("noted"); 4]),
            (
                "/a",
                vec![uses(&[report_call("r1", "a done")]), done("bye")],
            ),
        ],
    )
    .await;
    run.spawn(spawn_request("a", "Work.")).await;
    reported(&mut run, "/a").await;
    idle(&mut run, "/a").await;
    let service = service_of(&kernels);
    for _ in 0..2 {
        let mail = Mail {
            id: MailId("m77".to_owned()),
            from: AgentPath::root(),
            to: path("/a"),
            kind: MailKind::Message,
            wake: false,
            text: "Twice.".to_owned(),
        };
        service.deliver(&path("/a"), mail).unwrap();
        tokio::time::sleep(Duration::from_secs(1)).await;
    }
    tokio::time::sleep(Duration::from_secs(10)).await;
    let run = run.quit().await;
    let logged = run
        .records("/a")
        .into_iter()
        .filter(|e| {
            matches!(e, LogEvent::InputAccepted { from: Some(from), .. } if from.mail.0 == "m77")
        })
        .count();
    assert_eq!(logged, 1);
    let heard = heard_events(&heard)
        .into_iter()
        .filter(|e| matches!(e, TreeEvent::AgentMail(m) if m.id == "m77"))
        .count();
    assert_eq!(heard, 1);
}

fn running_count(run: &Run, agent: &str) -> usize {
    run.seen
        .iter()
        .filter(|e| {
            matches!(
                e,
                AgentEvent::AgentStateChanged { agent: a, state: AgentState::Running, .. }
                    if a.as_str() == agent
            )
        })
        .count()
}

#[tokio::test(start_paused = true)]
async fn a_child_whose_typed_input_is_blocked_never_shows_as_started() {
    let calls = Calls::default();
    let script: kit::Script = Arc::new(|_, _, event| {
        let blocked = matches!(&event, Event::UserInput(input) if input.text.starts_with("block"));
        Box::pin(async move {
            if blocked {
                wisp_plugin_api::hooks::Reply::Block("not that".into())
            } else {
                wisp_plugin_api::hooks::Reply::None
            }
        })
    });
    let body = "\n[[hooks]]\nname = \"guard\"\npoint = \"user-input\"\n";
    let make: crate::plugin::Make = kit::instantiate(script, calls.clone());
    let mut run = start(
        "hooks-child-blocked",
        Setup {
            hooks: vec![(kit::manifest("hooked", body), make)],
            ..Setup::default()
        },
        vec![
            (
                "/c",
                vec![uses(&[report_call("r1", "Looked at it")]), done("bye")],
            ),
            ("/", vec![done("noted")]),
        ],
    )
    .await;
    run.spawn(spawn_request("c", "Work.")).await;
    run.child_ended("/c").await;
    run.root_ended().await;
    assert_eq!(calls.count("guard"), 0);
    let before = running_count(&run, "/c");
    run.send(ClientCommand::Child {
        agent: path("/c"),
        command: Box::new(ClientCommand::send("you-1", "block this")),
    })
    .await;
    let rejected = run
        .until("the child's rejection", |e| {
            matches!(
                inner(e, "/c"),
                Some(AgentEvent::Rejected {
                    kind: Some(RejectKind::InputBlocked),
                    request: None,
                    ..
                })
            )
        })
        .await;
    assert!(inner(&rejected, "/c").is_some());
    let run = run.quit().await;
    assert_eq!(calls.count("guard"), 1);
    assert_eq!(running_count(&run, "/c"), before);
    let child: Vec<&AgentEvent> = run.seen.iter().filter_map(|e| inner(e, "/c")).collect();
    assert_eq!(
        child
            .iter()
            .filter(|e| matches!(e, AgentEvent::TurnStarted { .. }))
            .count(),
        1,
        "{child:#?}"
    );
}

#[tokio::test(start_paused = true)]
async fn a_childs_continues_follow_its_nudges_stop_at_the_cap_and_it_reports_once() {
    let calls = Calls::default();
    let script: kit::Script = Arc::new(|_, _, event| {
        let child = matches!(&event, Event::BeforeEnd(end) if end.reply == "bye");
        Box::pin(async move {
            if child {
                wisp_plugin_api::hooks::Reply::Continue("Once more.".into())
            } else {
                wisp_plugin_api::hooks::Reply::None
            }
        })
    });
    let body = "\n[[hooks]]\nname = \"more\"\npoint = \"before-end\"\n";
    let make: crate::plugin::Make = kit::instantiate(script, calls.clone());
    let child: Vec<Step> = (0..11).map(|_| done("bye")).collect();
    let mut run = start(
        "hooks-child-continues",
        Setup {
            hooks: vec![(kit::manifest("hooked", body), make)],
            ..Setup::default()
        },
        vec![("/c", child), ("/", vec![done("noted")])],
    )
    .await;
    run.spawn(spawn_request("c", "Work.")).await;
    assert!(matches!(
        run.child_ended("/c").await,
        TurnOutcome::Completed { .. }
    ));
    run.root_ended().await;
    let run = run.quit().await;
    let requests = run.requests("/c");
    assert_eq!(requests.len(), 11);
    for (at, request) in requests.iter().enumerate().skip(1) {
        let last = request.last_user_texts();
        let continued = last.iter().any(|t| t == "Once more.");
        assert_eq!(continued, at > 2, "{at}: {last:?}");
    }
    let reported = run
        .seen
        .iter()
        .filter(|e| matches!(e, AgentEvent::AgentReported { agent, .. } if agent.as_str() == "/c"))
        .count();
    assert_eq!(reported, 1);
    let applied = run
        .records("/c")
        .into_iter()
        .filter(|e| matches!(e, LogEvent::HookApplied { .. }))
        .count();
    assert_eq!(applied, 8);
    assert!(run.records("/c").iter().any(|e| matches!(
        e,
        LogEvent::HookRan { outcome: wisp_protocol::HookOutcome::Rejected { rule }, .. } if rule == "cap_reached"
    )));
}

#[test]
fn a_spawn_during_a_before_tool_hook_builds_the_child_and_the_call_goes_on() {
    on_alarm_stack(Timer::Paused, || async {
        let calls = Calls::default();
        let first = first_only();
        let (script, gate, started) = gated_hook(move |_, _| (first(), Reply::None));
        let body = "\n[[hooks]]\nname = \"gate\"\npoint = \"before-tool\"\n";
        let make: crate::plugin::Make = kit::instantiate(script, calls.clone());
        let mut run = start(
            "hooks-spawn-before-tool",
            Setup {
                hooks: vec![(kit::manifest("hooked", body), make)],
                ..Setup::default()
            },
            vec![
                (
                    "/review",
                    vec![uses(&[report_call("r1", "Looked at it")]), done("bye")],
                ),
                (
                    "/",
                    vec![
                        uses(&[call("l1", "list_agents", json!({}))]),
                        done("noted"),
                        done("noted"),
                    ],
                ),
            ],
        )
        .await;
        run.say("go").await;
        within("the hook's start", started.notified()).await;
        run.spawn(spawn_request("review", "Review the change."))
            .await;
        run.until("the spawn's answer", |e| {
            matches!(e, AgentEvent::AgentSpawned { .. })
        })
        .await;
        gate.notify_one();
        assert!(matches!(
            run.root_ended().await,
            TurnOutcome::Completed { .. }
        ));
        assert!(matches!(
            run.child_ended("/review").await,
            TurnOutcome::Completed { .. }
        ));
        let run = run.quit().await;
        assert!(calls.count("gate") >= 2);
        assert!(run.requests("/").len() >= 2);
    });
}

#[test]
fn a_client_spawn_or_rename_during_an_after_tool_hook_answers_only_its_own_request() {
    on_alarm_stack(Timer::Paused, || async {
        let (script, gate, started) = gated_hook(|_, _| (true, Reply::None));
        let body = "\n[[hooks]]\nname = \"gate\"\npoint = \"after-tool\"\n";
        let make: crate::plugin::Make = kit::instantiate(script, Calls::default());
        let mut run = start(
            "hooks-client-command-after-tool",
            Setup {
                hooks: vec![(kit::manifest("hooked", body), make)],
                ..Setup::default()
            },
            vec![(
                "/",
                vec![
                    uses(&[spawn_call("s1", "a", "A.")]),
                    uses(&[spawn_call("s2", "b", "B.")]),
                    uses(&[rename_call("n1", "a", "ay")]),
                    done("spawned"),
                ],
            )],
        )
        .await;
        run.say("go").await;
        within("the first hook", started.notified()).await;
        let spawn = run.spawn(spawn_request("c", "C.")).await;
        run.until("the client's spawn", |e| super::messages::spawned(e, "/c"))
            .await;
        gate.notify_one();
        within("the second hook", started.notified()).await;
        let (rename, renamed) = run.renaming("/b", "bee");
        run.send(rename).await;
        run.until("the rename", |e| {
            matches!(e, AgentEvent::AgentRenamed { .. })
        })
        .await;
        gate.notify_one();
        within("the third hook", started.notified()).await;
        let (rename, sea) = run.renaming("/c", "sea");
        run.send(rename).await;
        run.until(
            "the second rename",
            |e| matches!(e, AgentEvent::AgentRenamed { request, .. } if *request == Some(sea)),
        )
        .await;
        gate.notify_one();
        assert!(matches!(
            run.root_ended().await,
            TurnOutcome::Completed { .. }
        ));
        let mut answers: Vec<(String, Option<RequestId>)> = run
            .seen
            .iter()
            .filter_map(|e| match e {
                AgentEvent::AgentSpawned { request, agent, .. } => {
                    Some((agent.path.to_string(), *request))
                }
                AgentEvent::AgentRenamed { request, to, .. } => Some((to.to_string(), *request)),
                _ => None,
            })
            .collect();
        answers.sort();
        assert_eq!(
            answers,
            [
                ("/a".to_owned(), None),
                ("/ay".to_owned(), None),
                ("/b".to_owned(), None),
                ("/bee".to_owned(), Some(renamed)),
                ("/c".to_owned(), Some(spawn)),
                ("/sea".to_owned(), Some(sea)),
            ]
        );
        let run = run.quit().await;
        let told: Vec<String> = notices(&run, "/").into_iter().map(|(_, t)| t).collect();
        assert_eq!(told.len(), 2, "{told:?}");
        assert!(told.iter().all(|t| t.contains("by the user.")), "{told:?}");
    });
}

#[tokio::test(start_paused = true)]
async fn a_plugin_command_sees_the_root_and_its_tree_before_and_inside_its_future() {
    let run_seen: kit::Run = Arc::new(|host, _, _| {
        let before = kit::seen(host.as_ref());
        Box::pin(async move {
            let inside = kit::seen(host.as_ref());
            wisp_plugin_api::commands::CommandReply::Note(format!("{before} | {inside}"))
        })
    });
    let make: crate::plugin::Make = kit::commanding(kit::none(), run_seen, Calls::default());
    let manifest = kit::manifest_in("0.2.0", "seer", &kit::command("look", ""));
    let mut run = start(
        "command-scope",
        Setup {
            hooks: vec![(manifest, make)],
            ..Setup::default()
        },
        vec![
            ("/", vec![done("noted")]),
            ("/w", vec![uses(&[report_call("r1", "done")])]),
        ],
    )
    .await;
    let request = run.request();
    let look = ClientCommand::RunPluginCommand {
        name: "seer:look".to_owned(),
        args: wisp_protocol::Secret::new(String::new()),
        request,
    };
    run.send(look.clone()).await;
    let done = run
        .until("the answer", |e| {
            matches!(e, AgentEvent::CommandDone { .. })
        })
        .await;
    let AgentEvent::CommandDone {
        answers,
        outcome: wisp_protocol::CommandOutcome::Note { text },
    } = done
    else {
        panic!("{done:?}");
    };
    assert_eq!(answers, request);
    let (before, inside) = text.split_once(" | ").unwrap();
    assert_eq!(before, inside);
    assert!(before.starts_with("current /;"), "{text}");
    run.spawn(spawn_request("w", "Work.")).await;
    run.child_ended("/w").await;
    run.send(child("/w", look)).await;
    let done = run
        .until("/w's answer", |e| {
            matches!(inner(e, "/w"), Some(AgentEvent::CommandDone { .. }))
        })
        .await;
    let Some(AgentEvent::CommandDone {
        answers,
        outcome: wisp_protocol::CommandOutcome::Note { text },
    }) = inner(&done, "/w")
    else {
        panic!("{done:?}");
    };
    assert_eq!(*answers, request);
    assert!(text.starts_with("current /w;"), "{text}");
    run.quit().await;
}
