use std::collections::BTreeSet;
use std::time::Duration;

use tokio::sync::Notify;
use wisp_plugin_testkit::hooks::{self as kit, Calls, Render, Script as HookScript};
use wisp_protocol::RequestId;
use wisp_protocol::log::LogEvent;
use wisp_protocol::settings::{SettingChange, SettingValue};

use super::*;
use crate::plugin::{KernelHost, Make, PluginConfig, Registry};

const SEC: &str = "aa/sec";
const BEFORE_TURN_POINT: &str = "before-turn";

fn shown(sections: &[&str]) -> ClientCommand {
    ClientCommand::SlotsShown {
        sections: sections.iter().map(|s| (*s).to_owned()).collect(),
        pages: Vec::new(),
    }
}

fn facts() -> Render {
    Arc::new(|host, _, _| {
        let fill = host.current_fill();
        let words = format!(
            "measured={:?} model={}",
            fill.measured,
            host.current().model
        );
        Box::pin(async move { Ok(kit::line(&words)) })
    })
}

fn source(body: &str, script: HookScript, render: Render, calls: &Calls) -> (String, Make) {
    let make: Make = kit::sections(script, render, calls.clone());
    (kit::slotted("aa", body), make)
}

struct Watch {
    events: mpsc::Receiver<AgentEvent>,
    seen: Vec<AgentEvent>,
}

impl Watch {
    fn new(events: mpsc::Receiver<AgentEvent>) -> Self {
        Self {
            events,
            seen: Vec::new(),
        }
    }

    async fn until(&mut self, pick: impl Fn(&AgentEvent) -> bool) -> AgentEvent {
        loop {
            let event = tokio::time::timeout(Duration::from_secs(600), self.events.recv())
                .await
                .unwrap_or_else(|_| panic!("timed out; seen {:#?}", self.seen))
                .unwrap();
            self.seen.push(event.clone());
            if pick(&event) {
                return event;
            }
        }
    }

    async fn content(&mut self) -> String {
        let event = self
            .until(|e| matches!(e, AgentEvent::SlotContent { .. }))
            .await;
        let AgentEvent::SlotContent { content, .. } = event else {
            unreachable!()
        };
        serde_json::to_string(&content).unwrap()
    }

    async fn ended(&mut self) {
        self.until(|e| matches!(e, AgentEvent::TurnEnded { .. }))
            .await;
    }

    async fn ended_and_content(&mut self) -> String {
        let from = self.seen.len();
        self.ended().await;
        let drawn = self.seen[from..].iter().rev().find_map(|e| match e {
            AgentEvent::SlotContent { content, .. } => {
                Some(serde_json::to_string(content).unwrap())
            }
            _ => None,
        });
        match drawn {
            Some(drawn) => drawn,
            None => self.content().await,
        }
    }

    async fn quiet(&mut self, ms: u64) {
        let until = tokio::time::Instant::now() + Duration::from_millis(ms);
        while let Ok(Some(e)) = tokio::time::timeout_at(until, self.events.recv()).await {
            self.seen.push(e);
        }
    }

    async fn close(mut self, commands: mpsc::Sender<ClientCommand>) -> Vec<AgentEvent> {
        drop(commands);
        while let Some(e) = self.events.recv().await {
            self.seen.push(e);
        }
        self.seen
    }
}

fn line_point(point: &str) -> bool {
    use crate::plugin::hooks::strategy::{ACCEPT_POINT, HEAD_POINT, INSTRUCTION_POINT};
    [
        BEFORE_TURN_POINT,
        crate::plugin::hooks::sections::HEAD_POINT,
        INSTRUCTION_POINT,
        ACCEPT_POINT,
        HEAD_POINT,
    ]
    .contains(&point)
}

fn runs(events: &[AgentEvent]) -> Vec<String> {
    events
        .iter()
        .filter_map(|e| match e {
            AgentEvent::HookRan { hook, point, .. } if !line_point(point) => Some(hook.clone()),
            _ => None,
        })
        .collect()
}

fn logged_runs(store: &MemoryStore) -> Vec<String> {
    log_events(store)
        .into_iter()
        .filter_map(|e| match e {
            LogEvent::HookRan { hook, point, .. } if !line_point(&point) => Some(hook),
            _ => None,
        })
        .collect()
}

#[tokio::test(start_paused = true)]
async fn a_marks_point_marks_a_shown_section_with_no_hook_attached_and_writes_nothing() {
    let calls = Calls::default();
    let store = MemoryStore::default();
    let body = kit::section("sec", "marks = [\"after-response\"]\n");
    let agent = with_sections(
        logged(Fake::new(vec![ends("one"), ends("two")]), &store),
        vec![source(&body, kit::none(), facts(), &calls)],
    )
    .await;
    let (commands, events, _task) = spawn(agent);
    let mut watch = Watch::new(events);
    commands.send(shown(&[SEC])).await.unwrap();
    let first = watch.content().await;
    assert!(first.contains("measured=None"), "{first}");
    commands.send(ClientCommand::send("a", "a")).await.unwrap();
    let after = watch.ended_and_content().await;
    assert!(after.contains("measured=Some("), "{after}");
    watch.quiet(1_000).await;
    assert_eq!(
        calls.rendered("sec", wisp_plugin_api::slots::SlotView::Panel),
        2
    );
    commands.send(shown(&[])).await.unwrap();
    commands.send(ClientCommand::send("b", "b")).await.unwrap();
    watch.ended().await;
    watch.quiet(1_000).await;
    assert_eq!(calls.renders().len(), 2);
    let seen = watch.close(commands).await;
    assert!(runs(&seen).is_empty(), "{seen:?}");
    assert!(logged_runs(&store).is_empty());
    let records = format!("{:?}", records(&store));
    assert!(
        !records.contains("measured=") && !records.contains(SEC),
        "{records}"
    );
}

#[tokio::test(start_paused = true)]
async fn a_background_hook_that_fires_its_own_point_redraws_its_section_and_logs_only_its_run() {
    let calls = Calls::default();
    let store = MemoryStore::default();
    let body = "\n[[points]]\nname = \"ready\"\nkind = \"observe\"\nversion = \"1\"\n\n[[hooks]]\nname = \"gather\"\npoint = \"after-turn\"\nbackground = true\ntimeout_ms = 60000\n".to_owned()
        + &kit::section("sec", "marks = [\"aa/ready\"]\n");
    let agent = with_sections(
        logged(Fake::new(vec![ends("one")]), &store),
        vec![source(&body, kit::fires("ready"), facts(), &calls)],
    )
    .await;
    let (commands, events, _task) = spawn(agent);
    let mut watch = Watch::new(events);
    commands.send(shown(&[SEC])).await.unwrap();
    watch.content().await;
    commands.send(ClientCommand::send("a", "a")).await.unwrap();
    watch.ended().await;
    let redrawn = watch.content().await;
    assert!(redrawn.contains("measured=Some("), "{redrawn}");
    watch.quiet(1_000).await;
    assert_eq!(calls.renders().len(), 2);
    let seen = watch.close(commands).await;
    assert_eq!(runs(&seen), ["aa/gather"]);
    assert_eq!(logged_runs(&store), ["aa/gather"]);
}

fn model(name: &str) -> ClientCommand {
    ClientCommand::ChangeSettings {
        request: RequestId(1),
        changes: vec![SettingChange::named("model", SettingValue::Text(name.into())).unwrap()],
        save: false,
    }
}

#[tokio::test(start_paused = true)]
async fn with_no_hook_a_model_switched_mark_renders_the_new_model() {
    let calls = Calls::default();
    let body = kit::section("sec", "marks = [\"model-switched\"]\n");
    let agent = with_sections(
        Agent::new(Fake::new(vec![ends("one")]), default_session())
            .with_clock(crate::clock::TickingClock::new()),
        vec![source(&body, kit::none(), facts(), &calls)],
    )
    .await;
    let before = agent.session.model.clone();
    let (commands, events, _task) = spawn(agent);
    let mut watch = Watch::new(events);
    commands.send(shown(&[SEC])).await.unwrap();
    let first = watch.content().await;
    assert!(first.contains(&format!("model={before}")), "{first}");
    commands.send(model("claude-sonnet-5-5")).await.unwrap();
    commands.send(ClientCommand::send("a", "a")).await.unwrap();
    let switched = watch.ended_and_content().await;
    assert!(switched.contains("model=claude-sonnet-5-5"), "{switched}");
    watch.close(commands).await;
}

fn fill_words() -> Render {
    Arc::new(|host, _, _| {
        let fill = host.current_fill();
        let words = format!("epoch={} estimate={}", fill.epoch, fill.estimate);
        Box::pin(async move { Ok(kit::line(&words)) })
    })
}

fn native_compaction(summary: &str) -> Step {
    let raw = serde_json::json!({"type": "compaction", "content": summary});
    let mut script = vec![Ok(start(1_200, 0))];
    script.extend(opaque(0, raw));
    script.extend(ended(
        StopReason::Compaction,
        usage(1_200, 0, 300),
        Vec::new(),
    ));
    Step::from(script)
}

#[tokio::test(start_paused = true)]
async fn with_no_hook_an_after_compact_mark_renders_the_new_fill_after_a_compact() {
    let calls = Calls::default();
    let body = kit::section("sec", "marks = [\"after-compact\"]\n");
    let provider = Fake::new(vec![ends("one"), native_compaction("So far.")]);
    let agent = with_sections(
        Agent::new(provider, default_session()).with_clock(crate::clock::TickingClock::new()),
        vec![source(&body, kit::none(), fill_words(), &calls)],
    )
    .await;
    let (commands, events, _task) = spawn(agent);
    let mut watch = Watch::new(events);
    commands.send(shown(&[SEC])).await.unwrap();
    let first = watch.content().await;
    assert!(first.contains("epoch=0"), "{first}");
    commands.send(ClientCommand::send("a", "a")).await.unwrap();
    watch.ended().await;
    watch.quiet(1_000).await;
    assert_eq!(calls.renders().len(), 1);
    commands
        .send(ClientCommand::Compact {
            focus: None,
            request: RequestId(2),
        })
        .await
        .unwrap();
    let compacted = watch.content().await;
    assert!(compacted.contains("epoch=1"), "{compacted}");
    assert!(
        watch
            .seen
            .iter()
            .any(|e| matches!(e, AgentEvent::Compacted { .. })),
        "{:?}",
        watch.seen
    );
    let seen = watch.close(commands).await;
    assert!(runs(&seen).is_empty(), "{seen:?}");
}

#[tokio::test(start_paused = true)]
async fn with_no_hook_a_rewound_mark_renders_the_fill_after_a_rewind() {
    let calls = Calls::default();
    let body = kit::section("sec", "marks = [\"rewound\"]\n");
    let provider = Fake::new(vec![ends("one"), ends("two")]);
    let agent = with_sections(
        Agent::new(provider, default_session()).with_clock(crate::clock::TickingClock::new()),
        vec![source(&body, kit::none(), fill_words(), &calls)],
    )
    .await;
    let (commands, events, _task) = spawn(agent);
    let mut watch = Watch::new(events);
    commands.send(shown(&[SEC])).await.unwrap();
    let first = watch.content().await;
    for (id, text) in [("a", "a"), ("b", "b")] {
        commands.send(ClientCommand::send(id, text)).await.unwrap();
        watch.ended().await;
    }
    watch.quiet(1_000).await;
    assert_eq!(calls.renders().len(), 1);
    commands
        .send(ClientCommand::Rewind {
            to: wisp_protocol::RewindPoint::Turn { turn: 2 },
            request: RequestId(3),
        })
        .await
        .unwrap();
    let rewound = watch.content().await;
    assert!(
        watch
            .seen
            .iter()
            .any(|e| matches!(e, AgentEvent::Rewound { .. })),
        "{:?}",
        watch.seen
    );
    assert_ne!(rewound, first);
    let seen = watch.close(commands).await;
    assert!(runs(&seen).is_empty(), "{seen:?}");
}

#[tokio::test(start_paused = true)]
async fn a_section_shown_after_a_resume_renders_the_logged_fill_and_one_reshown_reads_the_last_turn()
 {
    let calls = Calls::default();
    let store = MemoryStore::default();
    let body = kit::section("sec", "");
    let agent = with_sections(
        logged(Fake::new(vec![ends("one"), ends("two")]), &store),
        vec![source(&body, kit::none(), facts(), &calls)],
    )
    .await;
    let (commands, events, _task) = spawn(agent);
    let mut watch = Watch::new(events);
    commands.send(shown(&[SEC])).await.unwrap();
    let hidden_turn = watch.content().await;
    assert!(hidden_turn.contains("measured=None"), "{hidden_turn}");
    commands.send(shown(&[])).await.unwrap();
    commands.send(ClientCommand::send("a", "a")).await.unwrap();
    watch.ended().await;
    commands.send(shown(&[SEC])).await.unwrap();
    let reshown = watch.content().await;
    assert!(reshown.contains("measured=Some("), "{reshown}");
    watch.close(commands).await;
    let records = records(&store);
    let resumed = with_sections(
        logged(Fake::new(Vec::new()), &store)
            .resume(&records, 0)
            .unwrap(),
        vec![source(&body, kit::none(), facts(), &calls)],
    )
    .await;
    let (commands, events, _task) = spawn(resumed);
    let mut watch = Watch::new(events);
    commands.send(shown(&[SEC])).await.unwrap();
    let first = watch.content().await;
    assert_eq!(first, reshown);
    watch.close(commands).await;
}

fn sleeping(ms: u64, started: Arc<Mutex<Option<tokio::time::Instant>>>) -> Render {
    Arc::new(move |_, _, _| {
        let started = started.clone();
        Box::pin(async move {
            *started.lock().unwrap() = Some(tokio::time::Instant::now());
            tokio::time::sleep(Duration::from_millis(ms)).await;
            Ok(kit::line("slow"))
        })
    })
}

#[tokio::test(start_paused = true)]
async fn a_slow_render_beside_its_plugins_before_turn_hook_doesnt_delay_the_first_request() {
    let calls = Calls::default();
    let started = Arc::new(Mutex::new(None));
    let body = "\n[[hooks]]\nname = \"lead\"\npoint = \"before-turn\"\n".to_owned()
        + &kit::section("sec", kit::every_point().as_str());
    let provider = Fake::new(vec![ends("one")]);
    let agent = with_sections(
        Agent::new(provider.clone(), default_session())
            .with_clock(crate::clock::TickingClock::new()),
        vec![source(
            &body,
            kit::none(),
            sleeping(400, started.clone()),
            &calls,
        )],
    )
    .await;
    let (commands, events, _task) = spawn(agent);
    let mut watch = Watch::new(events);
    commands.send(shown(&[SEC])).await.unwrap();
    while started.lock().unwrap().is_none() {
        tokio::task::yield_now().await;
    }
    let began = started.lock().unwrap().unwrap();
    commands.send(ClientCommand::send("a", "a")).await.unwrap();
    watch.ended().await;
    assert!(tokio::time::Instant::now() - began < Duration::from_millis(400));
    assert_eq!(provider.sent().len(), 1);
    assert_eq!(calls.count("lead"), 1);
    watch.close(commands).await;
}

#[tokio::test(start_paused = true)]
async fn slots_shown_during_a_hook_is_applied_while_the_hook_still_runs() {
    let calls = Calls::default();
    let gate = Arc::new(Notify::new());
    let started = Arc::new(Notify::new());
    let body = "\n[[hooks]]\nname = \"lead\"\npoint = \"before-turn\"\n".to_owned()
        + &kit::section("sec", "");
    let agent = with_sections(
        Agent::new(Fake::new(vec![ends("one")]), default_session())
            .with_clock(crate::clock::TickingClock::new()),
        vec![source(
            &body,
            gated(gate.clone(), started.clone()),
            kit::views(),
            &calls,
        )],
    )
    .await;
    let (commands, events, _task) = spawn(agent);
    let mut watch = Watch::new(events);
    commands.send(ClientCommand::send("a", "a")).await.unwrap();
    within("the hook's start", started.notified()).await;
    commands.send(shown(&[SEC, "zz/nope"])).await.unwrap();
    watch.content().await;
    assert!(
        !watch.seen.iter().any(|e| matches!(
            e,
            AgentEvent::TurnEnded { .. } | AgentEvent::Rejected { .. }
        )),
        "{:?}",
        watch.seen
    );
    gate.notify_one();
    watch.ended().await;
    let seen = watch.close(commands).await;
    assert!(
        !seen
            .iter()
            .any(|e| matches!(e, AgentEvent::Rejected { .. })),
        "{seen:?}"
    );
}

#[tokio::test(start_paused = true)]
async fn a_render_that_calls_every_refused_import_answers_at_once_with_their_unavailable_values() {
    let calls = Calls::default();
    let seen_imports: kit::Seen = Arc::default();
    let body = "\n[[points]]\nname = \"x\"\nkind = \"observe\"\nversion = \"1\"\n\n[[hooks]]\nname = \"heard\"\npoint = \"aa/x\"\n".to_owned()
        + &kit::section("sec", "");
    let agent = with_sections(
        Agent::new(Fake::new(Vec::new()), default_session())
            .with_clock(crate::clock::TickingClock::new()),
        vec![source(
            &body,
            kit::none(),
            kit::refused_imports(seen_imports.clone(), "toolu_any"),
            &calls,
        )],
    )
    .await;
    let (commands, events, _task) = spawn(agent);
    let mut watch = Watch::new(events);
    commands.send(shown(&[SEC])).await.unwrap();
    let at = tokio::time::Instant::now();
    let content = watch.content().await;
    assert!(content.contains("refused"), "{content}");
    assert!(tokio::time::Instant::now() - at < Duration::from_millis(500));
    assert_eq!(calls.count("heard"), 0);
    let answers = seen_imports.lock().unwrap().clone();
    assert_eq!(
        answers,
        [
            "fire 0",
            "count Unsupported",
            "slept",
            "capture true",
            "spawn true",
            "adopt true",
            "todos true",
            "list true",
            "spawn_agent true",
            "report true",
            "send true",
            "stop true",
            "fetch true",
            "facts true true true true",
        ]
    );
    let seen = watch.close(commands).await;
    assert!(runs(&seen).is_empty());
}

const CONTEXT_BEFORE_SECTIONS: &str = r#"id = "context"
version = "0.1.0"
world = "0.2.0"
summary = "Tells the model how full its context is"
settings = ["context.context_note"]

[system]
text = "A 'Context:' line, when present, gives how much of your context window the conversation fills; at the stated point Wisp replaces older turns with a summary."
after = ["time"]

[[hooks]]
name = "fill"
point = "before-turn"
in = ["core/notes"]
after = ["time/sent"]
switch = "context.context_note"
"#;

const TIMES: [&str; 4] = ["at", "started_at", "duration_ms", "first_byte_ms"];

fn timeless(value: &mut serde_json::Value) {
    match value {
        serde_json::Value::Object(map) => {
            for (key, inner) in map.iter_mut() {
                if TIMES.contains(&key.as_str()) {
                    *inner = serde_json::Value::Null;
                } else {
                    timeless(inner);
                }
            }
        }
        serde_json::Value::Array(items) => items.iter_mut().for_each(timeless),
        _ => {}
    }
}

async fn default_session_with(context_manifest: &str) -> (Vec<serde_json::Value>, Vec<AgentEvent>) {
    let bundled = |b: wisp_plugin_api::Bundled, manifest: &str| {
        let make: Make = Arc::new(move |host| (b.instantiate)(host));
        (manifest.to_owned(), make)
    };
    let sources = vec![
        bundled(
            wisp_plugin_time::bundled(),
            wisp_plugin_time::bundled().manifest,
        ),
        bundled(wisp_plugin_context::bundled(), context_manifest),
    ];
    let (registry, problems) = Registry::from_sources(sources, &PluginConfig::default());
    assert!(problems.is_empty(), "{problems:?}");
    let started = registry
        .start(&BTreeSet::new(), Arc::new(KernelHost::new()))
        .await;
    let store = MemoryStore::default();
    let agent = logged(Fake::new(vec![ends("one"), ends("two")]), &store)
        .with_hooks(started.hooks, started.host);
    let (commands, events, _task) = spawn(agent);
    let mut watch = Watch::new(events);
    for (id, text) in [("a", "a"), ("b", "b")] {
        commands.send(ClientCommand::send(id, text)).await.unwrap();
        watch.ended().await;
    }
    watch.quiet(1_000).await;
    let seen = watch.close(commands).await;
    let records = records(&store)
        .iter()
        .map(|record| {
            let mut value = serde_json::to_value(record).unwrap();
            timeless(&mut value);
            value
        })
        .collect();
    (records, seen)
}

#[tokio::test(start_paused = true)]
async fn off_by_default_the_context_section_leaves_the_log_as_before_it_and_renders_nothing() {
    assert_eq!(
        wisp_plugin_context::MANIFEST.split("\n[[slots]]").next(),
        Some(CONTEXT_BEFORE_SECTIONS)
    );
    let (before, _) = default_session_with(CONTEXT_BEFORE_SECTIONS).await;
    let (now, seen) = default_session_with(wisp_plugin_context::MANIFEST).await;
    assert_eq!(before, now);
    assert!(
        serde_json::to_string(&before)
            .unwrap()
            .contains("Context: "),
        "{before:#?}"
    );
    assert!(
        !seen.iter().any(|e| matches!(
            e,
            AgentEvent::SlotContent { .. } | AgentEvent::SlotFailed { .. }
        )),
        "{seen:?}"
    );
}

#[tokio::test(start_paused = true)]
async fn a_section_of_a_plugin_off_at_start_gets_no_render_until_its_plugin_is_on() {
    let calls = Calls::default();
    let body = kit::section("sec", "marks = [\"after-turn\"]\n");
    let layers = crate::settings::host::StartLayers {
        flags: vec![crate::plugin::testing::plugin_flag(
            "aa",
            wisp_protocol::settings::ENABLED_KEY,
            false,
        )],
        ..Default::default()
    };
    let agent = with_sections(
        Agent::new(Fake::new(vec![ends("one")]), default_session())
            .with_clock(TickingClock::new())
            .with_start_layers(layers),
        vec![source(&body, kit::none(), facts(), &calls)],
    )
    .await;
    let (commands, events, _task) = spawn(agent);
    let mut watch = Watch::new(events);
    commands.send(shown(&[SEC])).await.unwrap();
    watch.quiet(5_000).await;
    assert!(calls.renders().is_empty());
    commands
        .send(crate::plugin::testing::plugin_switch(
            1,
            "aa",
            wisp_protocol::settings::ENABLED_KEY,
            true,
        ))
        .await
        .unwrap();
    watch.quiet(1_000).await;
    assert!(calls.renders().is_empty());
    commands.send(ClientCommand::send("a", "a")).await.unwrap();
    watch.ended_and_content().await;
    assert!(!calls.renders().is_empty());
    watch.close(commands).await;
}

#[tokio::test(start_paused = true)]
async fn a_plugin_added_back_or_updated_draws_its_shown_section_again_from_its_new_instance() {
    use crate::plugin::testing::{Disk, plugin_switch};
    use crate::plugin::{Live, Source};
    use wisp_protocol::settings::INSTALLED_KEY;
    let calls = Calls::default();
    let body = kit::section("sec", "marks = [\"after-turn\"]\n");
    let section = |sha: &str| {
        let (text, make) = source(&body, kit::none(), facts(), &calls);
        Source::wasm(text, make, sha.repeat(64))
    };
    let (registry, _) = Registry::from_sources(
        crate::plugin::testing::turn_line_sources(),
        &PluginConfig::default(),
    );
    let registry = Arc::new(registry.with_installed(vec![section("a")]).0);
    let started = registry
        .start(&BTreeSet::new(), Arc::new(KernelHost::new()))
        .await;
    let mut agent = Agent::new(
        Fake::new(vec![ends("one"), ends("two"), ends("three"), ends("four")]),
        default_session(),
    )
    .with_clock(TickingClock::new())
    .with_hooks(started.hooks, started.host);
    let disk = Arc::new(Disk::default());
    let installed: Arc<dyn crate::plugin::Installed> = disk.clone();
    agent.hooks.live = Some(Arc::new(Live::new(registry, Some(installed))));
    let (commands, events, _task) = spawn(agent);
    let mut watch = Watch::new(events);
    commands.send(shown(&[SEC])).await.unwrap();
    watch.content().await;
    let mut drawn = Vec::new();
    for (request, on, text) in [
        (1, Some(false), "a"),
        (2, Some(true), "b"),
        (3, None, "c"),
        (4, None, "d"),
    ] {
        if let Some(on) = on {
            commands
                .send(plugin_switch(request, "aa", INSTALLED_KEY, on))
                .await
                .unwrap();
        }
        if request == 4 {
            disk.put(vec![section("b")], Vec::new());
        }
        commands
            .send(ClientCommand::send(text, text))
            .await
            .unwrap();
        watch.ended().await;
        watch.quiet(1_000).await;
        drawn.push(calls.render_instances());
    }
    assert_eq!(drawn[0], [1]);
    assert_eq!(drawn[1], [1, 2]);
    assert_eq!(drawn[2], [1, 2, 2]);
    assert_eq!(drawn[3], [1, 2, 2, 3]);
    watch.close(commands).await;
}
