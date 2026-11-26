use std::fs;
use std::path::PathBuf;

use wisp_protocol::log::LogEvent;

use super::*;
use crate::agent::AgentTask;
use crate::clock::ShiftedClock;
use crate::plugin::{PluginConfig, Registry, Tools};
use crate::test_guard::ScratchDir;
use crate::test_support::rooted::{self, Inputs, Logs, Rooted};

const NOTE: &str = "The fixture rosette is cobalt.";
const REPLACED: &str = "The fixture rosette is vermilion.";
const WISP: &str = "[models.\"claude-opus-5-5\"]\nnative_compaction = false\n";

fn remember(action: &str, name: &str, fact: Option<&str>) -> Value {
    let mut input = json!({"action":action,"scope":"you","name":name});
    if let Some(fact) = fact {
        input["fact"] = fact.into();
    }
    input
}

fn calling(id: &str, name: &str, input: Value, tokens: u64) -> Step {
    let mut script = vec![start(tokens, 0)];
    script.extend(tool_use(0, id, name, &[&input.to_string()]));
    script.extend(finish(StopReason::ToolUse, tokens, 0, 40));
    Step::from(script)
}

pub(in crate::agent) fn tools(memory: bool) -> Tools {
    let bundles = wisp_plugins::bundled()
        .into_iter()
        .filter(|bundle| {
            bundle.manifest == wisp_plugin_compaction::MANIFEST
                || (memory && bundle.manifest == wisp_plugin_memory::MANIFEST)
        })
        .chain([wisp_plugin_testkit::bundled()]);
    let (registry, problems) = Registry::new(bundles.collect(), &PluginConfig::default());
    assert!(problems.is_empty(), "{problems:?}");
    let on = ["echo"]
        .into_iter()
        .chain(memory.then_some("remember"))
        .map(str::to_owned)
        .collect();
    Tools::new(Arc::new(registry), on)
}

pub(in crate::agent) struct Session {
    pub(in crate::agent) commands: mpsc::Sender<ClientCommand>,
    events: mpsc::Receiver<AgentEvent>,
    task: AgentTask,
    seen: Vec<AgentEvent>,
    scratch: ScratchDir,
    inputs: Inputs,
    pub(in crate::agent) data: PathBuf,
    pub(in crate::agent) store: MemoryStore,
}

impl Session {
    pub(in crate::agent) async fn new(
        provider: Arc<dyn Provider>,
        tools: Tools,
        model_settings: ModelSettings,
        start_layers: crate::settings::host::StartLayers,
        auto: bool,
        user_input: bool,
    ) -> Self {
        let scratch = ScratchDir::new("wisp-memory-fixture-", "rooted");
        let data = scratch.root.join("plugin-data");
        let store = MemoryStore::default();
        let inputs = Inputs {
            connect: rooted::single_provider(provider, log_context()),
            tools: Arc::new(tools),
            logs: Arc::new(Logs {
                id: "s1",
                store: store.clone(),
            }),
            log_context: log_context(),
            workspace: scratch.workspace(),
            plugin_data: Some(data.clone()),
            clock: Arc::new(ShiftedClock::at(
                "2000-06-05T01:00:00Z".parse().unwrap(),
                None,
            )),
            models: model_settings,
            start_layers,
            auto_compact: auto,
            user_input,
        };
        Self::open(inputs, scratch, store, crate::kit::Start::Fresh).await
    }

    async fn open(
        inputs: Inputs,
        scratch: ScratchDir,
        store: MemoryStore,
        start: crate::kit::Start<'_>,
    ) -> Self {
        let session = default_session();
        let spec = crate::kit::Spec {
            start,
            ..rooted::spec(crate::kit::Choices {
                model: session.model,
                effort: session.effort,
                cache_ttl: session.cache_ttl,
                message_time: session.message_time,
                context_note: session.context_note,
            })
        };
        let data = inputs.plugin_data.clone().unwrap();
        let built = Rooted::build(inputs.clone().kit(), spec, rooted::tree())
            .await
            .unwrap();
        assert!(
            built.built.plugin_problems.is_empty(),
            "{:?}",
            built.built.plugin_problems
        );
        let (commands, events, task) = built.start();
        Self {
            commands,
            events,
            task,
            seen: Vec::new(),
            scratch,
            inputs,
            data,
            store,
        }
    }

    pub(in crate::agent) async fn resume(self) -> Self {
        let (inputs, scratch, store) = self.close().await;
        let provider = inputs.connect.connect(crate::kit::ROOT).unwrap().provider;
        let records = store.read("s1").unwrap();
        let providers = |_: Option<&str>| Some(provider.clone());
        let mut recovered = crate::resume::recover(
            crate::session::reread(&records).unwrap(),
            &store,
            &providers,
        )
        .unwrap();
        let (logged, rebuilt) = recovered.take_checked();
        let logged = logged.unwrap();
        let log = SessionLog::continuing(Box::new(store.clone()), "s1", records.len() as u64);
        let start = crate::kit::Start::Resume(crate::kit::Resume {
            log,
            recovered: &recovered,
            truncated_bytes: 0,
            logged: Some(&logged),
            rebuilt,
        });
        Self::open(inputs, scratch, store, start).await
    }

    pub(in crate::agent) fn repository(&self) {
        crate::instructions::make_repository(&self.scratch.root.join("work")).unwrap();
    }

    pub(in crate::agent) async fn send(&self, command: ClientCommand) {
        self.commands.send(command).await.unwrap();
    }

    pub(in crate::agent) async fn until(
        &mut self,
        pick: impl Fn(&AgentEvent) -> bool,
    ) -> AgentEvent {
        until_event(
            &mut self.events,
            &mut self.seen,
            "the rooted memory agent",
            pick,
        )
        .await
    }

    pub(in crate::agent) async fn turn(&mut self, id: &str, text: &str) {
        self.send(send(id, text)).await;
        let end = self
            .until(|e| matches!(e, AgentEvent::TurnEnded { .. }))
            .await;
        assert!(matches!(end, AgentEvent::TurnEnded { outcome, .. } if completed(&outcome)));
    }

    pub(in crate::agent) async fn compact(&mut self, request: u64) -> AgentEvent {
        self.send(compact(request, None)).await;
        self.until(|e| answers(e, request)).await
    }

    pub(in crate::agent) async fn finish(self) {
        self.close().await;
    }

    async fn close(mut self) -> (Inputs, ScratchDir, MemoryStore) {
        drop(self.commands);
        let (_, joined) = within("root shutdown", async move {
            tokio::join!(
                async { while self.events.recv().await.is_some() {} },
                self.task.join()
            )
        })
        .await;
        assert!(joined);
        (self.inputs, self.scratch, self.store)
    }
}

fn block(text: &str) -> Option<&str> {
    let (_, rest) = text.split_once("<memory>")?;
    Some(rest.split_once("</memory>")?.0)
}

fn assert_heads(store: &MemoryStore, wanted: CompactionEngine, facts: &[&str]) {
    let heads = pick!(log_events(store), LogEvent::ContextCompacted { engine, head, sections, placement, additions, .. } => (engine, head, sections, placement, additions));
    assert_eq!(heads.len(), facts.len());
    for ((engine, head, sections, placement, additions), fact) in heads.iter().zip(facts) {
        assert_eq!(*engine, wanted);
        let [section] = sections.as_slice() else {
            panic!("expected one memory snapshot")
        };
        assert_eq!(section.name, "memory/list");
        assert!(section.snapshot.contains("sha256"));
        let effective = head
            .content
            .iter()
            .chain(additions)
            .cloned()
            .collect::<Vec<_>>();
        let text = reply_text(&effective).unwrap();
        assert_eq!(text.matches("<memory>").count(), 1);
        assert!(block(&text).unwrap().contains(fact));
        match wanted {
            CompactionEngine::Native => {
                assert!(matches!(
                    head.content.first(),
                    Some(ContentBlock::Opaque { .. })
                ));
                assert!(
                    matches!(effective.get(1), Some(ContentBlock::Text { text }) if block(text).is_some_and(|list| list.contains(fact)))
                );
                assert!(!text.contains("<summary>"));
                if *placement == wisp_protocol::Placement::Leading {
                    assert_eq!(head.content.len(), 1);
                } else {
                    assert!(additions.is_empty());
                }
            }
            CompactionEngine::Wisp => {
                assert!(text.find("<memory>").unwrap() < text.find("<compacted_context").unwrap());
            }
        }
    }
}

async fn three_heads(native: bool) {
    let mut steps = vec![
        calling(
            "toolu_saved",
            "remember",
            remember("save", "rosette", Some(NOTE)),
            1_000,
        ),
        answer("Seeded.", 1_100),
    ];
    for i in 0..33 {
        steps.push(calling(
            &format!("toolu_fill_{i}"),
            "echo",
            json!({"text": format!("filler record {i}: {}", "crowded words ".repeat(140))}),
            1_500 + i * 1100,
        ));
        steps.push(answer("Filler handled.", 2_000 + i * 1100));
    }
    steps.extend([
        done(native),
        answer("First head.", 500),
        calling(
            "toolu_replace",
            "remember",
            remember("save", "rosette", Some(REPLACED)),
            600,
        ),
        calling(
            "toolu_other",
            "remember",
            remember("save", "border", Some("The border has painted triangles.")),
            800,
        ),
        answer("Replaced.", 1000),
        done(native),
        answer("Second head.", 500),
        calling(
            "toolu_delete",
            "remember",
            remember("forget", "rosette", None),
            700,
        ),
        answer("Removed.", 900),
        done(native),
        answer("Third head.", 500),
    ]);
    let provider = scripted(steps);
    let settings = if native {
        ModelSettings::shipped_only()
    } else {
        models(WISP)
    };
    let mut live = Session::new(
        provider.clone(),
        tools(true),
        settings,
        Default::default(),
        false,
        false,
    )
    .await;
    assert!(!live.data.exists());
    live.turn("seed", "Save the fixture note.").await;
    let file = live.data.join("memory/user/rosette.md");
    assert!(fs::read_to_string(&file).unwrap().contains(NOTE));
    for i in 0..33 {
        live.turn(&format!("fill-{i}"), "Add unrelated history.")
            .await;
    }
    for (id, next, expected) in [
        (1, "first", NOTE),
        (2, "second", REPLACED),
        (3, "third", "painted triangles"),
    ] {
        let event = live.compact(id).await;
        assert!(matches!(event, AgentEvent::Compacted { .. }), "{event:?}");
        live.turn(next, "Continue after shortening.").await;
        let sent = provider.sent();
        let body = sent.last().unwrap().body();
        let message = body["messages"].to_string();
        assert_eq!(message.matches("<memory>").count(), 1);
        assert!(block(&message).unwrap().contains(expected));
        if id == 3 {
            assert!(!block(&message).unwrap().contains("rosette"));
        }
        if id == 1 {
            live.turn(
                "replace",
                "Replace the note then save one about the border.",
            )
            .await;
        }
        if id == 2 {
            live.turn("forget", "Forget the rosette.").await;
        }
    }
    assert!(!file.exists());
    assert_heads(
        &live.store,
        if native {
            CompactionEngine::Native
        } else {
            CompactionEngine::Wisp
        },
        &[NOTE, REPLACED, "painted triangles"],
    );
    live.finish().await;
}

#[tokio::test]
async fn saved_memory_survives_three_wisp_heads() {
    three_heads(false).await;
}

#[tokio::test]
async fn saved_memory_survives_three_native_heads() {
    three_heads(true).await;
}

#[tokio::test]
async fn saved_memory_survives_three_installed_background_heads() {
    use super::background::{NEAR, POINT, extends, saved, small_band, swapped_in};

    let mut steps = vec![
        calling(
            "toolu_saved",
            "remember",
            remember("save", "rosette", Some(NOTE)),
            1_000,
        ),
        answer("Seeded.", 1_100),
    ];
    for i in 0..31 {
        steps.push(calling(
            &format!("toolu_filler_{i}"),
            "echo",
            json!({"text":format!("Record {i} {}", "background fixture words ".repeat(90))}),
            5_000,
        ));
        steps.push(answer("Filler.", 6_000));
    }
    for _ in 0..3 {
        steps.extend([
            answer("Ready.", NEAR),
            compaction(SUMMARY),
            answer("After swap.", 650),
            answer("After append.", 750),
        ]);
    }
    let provider = documented(steps);
    let mut live = Session::new(
        provider.clone(),
        tools(true),
        models(POINT),
        saved(small_band()),
        true,
        true,
    )
    .await;
    live.turn("seed", "Save the fixture note.").await;
    let file = live.data.join("memory/user/rosette.md");
    assert!(fs::read_to_string(&file).unwrap().contains(NOTE));
    for i in 0..31 {
        live.turn(&format!("old-{i}"), "Make some history.").await;
    }
    for i in 0..3 {
        live.turn(&format!("near-{i}"), "Pack the context.").await;
        live.until(prepared).await;
        live.send(send(
            &format!("swap-{i}"),
            "Continue after the new context.",
        ))
        .await;
        live.until(|event| swapped_in(std::slice::from_ref(event), 34 + 3 * i))
            .await;
        live.until(|e| matches!(e, AgentEvent::TurnEnded { .. }))
            .await;
        live.turn(&format!("after-{i}"), "One more turn.").await;
        let n = provider.sent().len();
        extends(&provider, n - 2, n - 1);
        let sent = provider.body(n - 1)["messages"].to_string();
        assert_eq!(sent.matches("<memory>").count(), 1);
        assert!(!sent.contains("<memory-update>"));
        assert!(block(&sent).unwrap().contains(NOTE));
    }
    assert_heads(&live.store, CompactionEngine::Native, &[NOTE; 3]);
    live.finish().await;
}

#[tokio::test]
async fn refusing_a_save_never_includes_it_in_compacted_memory() {
    let prefix = wisp_protocol::KEY_PREFIXES[0];
    let canary = format!("{prefix}CANARYabcdefghijklmnopqrst");
    assert!(crate::secrets::scan::contains_secret_or_identity(
        &canary, None
    ));
    let provider = scripted(vec![
        calling(
            "toolu_good",
            "remember",
            remember("save", "rosette", Some(NOTE)),
            1000,
        ),
        calling(
            "toolu_refused",
            "remember",
            remember("save", "never-stored", Some(&canary)),
            1200,
        ),
        answer("Finished.", 1400),
        answer(WRITTEN, 1400),
        answer("Next.", 900),
    ]);
    let mut live = Session::new(
        provider.clone(),
        tools(true),
        models(WISP),
        Default::default(),
        false,
        true,
    )
    .await;
    live.turn("seed", "Use memory for the safe fixture.").await;
    assert!(!live.data.join("memory/user/never-stored.md").exists());
    let mut chars = canary.chars();
    let first = u32::from(chars.next().unwrap());
    let encoded = format!("\\u{first:04x}{}", chars.as_str());
    let hand_edit = format!(
        "---\nname: escaped-marker\ndescription: \"{encoded} <petal>\"\nsaved: 2000-01-02\n---\n"
    );
    assert!(!crate::secrets::scan::contains_secret_or_identity(
        &hand_edit, None
    ));
    fs::write(live.data.join("memory/user/escaped-marker.md"), hand_edit).unwrap();
    assert!(matches!(
        live.compact(1).await,
        AgentEvent::Compacted { .. }
    ));
    live.turn("next", "Continue.").await;
    let sent = provider.body(4)["messages"].to_string();
    let memory = block(&sent).unwrap();
    assert!(memory.contains(NOTE));
    assert!(
        !memory.contains("never-stored")
            && !memory.contains("escaped-marker")
            && !memory.contains("CANARY")
    );
    live.finish().await;
}

#[tokio::test]
async fn a_disabled_memory_plugin_adds_no_section_even_with_existing_notes() {
    let provider = scripted(vec![
        answer("Ready.", 1200),
        answer(WRITTEN, 1200),
        answer("Next.", 900),
    ]);
    let mut live = Session::new(
        provider.clone(),
        tools(false),
        models(WISP),
        Default::default(),
        false,
        true,
    )
    .await;
    let dir = live.data.join("memory/user");
    crate::secrets::create_folder(&dir).unwrap();
    fs::write(
        dir.join("rosette.md"),
        format!("---\nname: rosette\ndescription: {NOTE}\nsaved: 2000-01-01\n---\n"),
    )
    .unwrap();
    live.turn("first", "Hello.").await;
    assert!(matches!(
        live.compact(1).await,
        AgentEvent::Compacted { .. }
    ));
    live.turn("next", "Continue.").await;
    for sent in provider.sent() {
        assert!(block(&sent.body()["messages"].to_string()).is_none());
        assert!(!sent.tools.iter().any(|tool| tool.name == "remember"));
    }
    live.finish().await;
}

#[tokio::test]
async fn true_turn_head_and_doctor_callbacks_cannot_mutate_but_a_command_can() {
    use std::sync::Mutex;
    use std::sync::atomic::{AtomicBool, Ordering};
    use tokio::sync::Notify;
    use wisp_plugin_api::commands::CommandReply;
    use wisp_plugin_api::host::Host;
    use wisp_plugin_api::private_files::{ChangeAction, Expected, PrivateFileError};
    use wisp_plugin_api::prompt::{At, Prior, Rendered};
    use wisp_plugin_api::tools::{BuildContext, DoctorRow};
    use wisp_plugin_api::{BoxFuture, Bundled, Plugin};

    type Attempts = Arc<Mutex<Vec<(String, Result<(), PrivateFileError>)>>>;
    #[derive(Default)]
    struct Gate {
        armed: AtomicBool,
        begun: Notify,
    }
    struct Probe {
        host: Arc<dyn Host>,
        tries: Attempts,
        gate: Arc<Gate>,
    }
    impl Probe {
        fn change(&self, purpose: &str) {
            let result = self.host.private_files().change(
                purpose,
                Expected::Absent,
                ChangeAction::Write(b"fixture".to_vec()),
                None,
            );
            self.tries
                .lock()
                .unwrap()
                .push((purpose.to_owned(), result));
        }
    }
    impl Plugin for Probe {
        fn doctor<'a>(&'a self, _: &'a BuildContext) -> BoxFuture<'a, Vec<DoctorRow>> {
            self.change("doctor");
            Box::pin(std::future::ready(Vec::new()))
        }
        fn render_section<'a>(
            &'a self,
            _: &'a str,
            at: At,
            _: Prior,
            _: Option<&'a str>,
        ) -> BoxFuture<'a, Rendered> {
            self.change(match at {
                At::Start => "start",
                At::Turn => "turn",
                At::Head => "head",
            });
            Box::pin(async move {
                if at == At::Head && self.gate.armed.swap(false, Ordering::SeqCst) {
                    self.gate.begun.notify_one();
                    std::future::pending::<()>().await;
                }
                Rendered::default()
            })
        }
        fn run_command<'a>(&'a self, _: &'a str, _: &'a str) -> BoxFuture<'a, CommandReply> {
            self.change("command");
            Box::pin(std::future::ready(CommandReply::Note("Done.".into())))
        }
    }
    let tries: Attempts = Arc::default();
    let made = tries.clone();
    let gate: Arc<Gate> = Arc::default();
    let plugin_gate = gate.clone();
    let manifest = r#"id = "write-probe"
version = "0.1.0"
world = "0.4.0"
summary = "Exercise scoped file writes"
[[commands]]
name = "probe"
summary = "Try a write"
[[prompt_sections]]
name = "probe"
place = "lead"
in = ["core/state"]
before = ["memory/list"]
"#;
    let mut sources: Vec<(String, crate::plugin::Make)> = wisp_plugins::bundled()
        .into_iter()
        .map(|bundle: Bundled| {
            let make: crate::plugin::Make = Arc::new(move |host| (bundle.instantiate)(host));
            (bundle.manifest.to_owned(), make)
        })
        .collect();
    sources.push((
        manifest.to_owned(),
        Arc::new(move |host| {
            Arc::new(Probe {
                host,
                tries: made.clone(),
                gate: plugin_gate.clone(),
            })
        }),
    ));
    let (registry, problems) = Registry::from_sources(sources, &PluginConfig::default());
    assert!(problems.is_empty());
    let registry = Arc::new(registry);
    let provider = scripted(vec![
        calling(
            "toolu_saved",
            "remember",
            remember("save", "rosette", Some(NOTE)),
            900,
        ),
        answer("Ready.", 1200),
        answer(WRITTEN, 1200),
        answer("Next.", 900),
    ]);
    let mut live = Session::new(
        provider.clone(),
        Tools::new(registry.clone(), BTreeSet::from(["remember".into()])),
        models(WISP),
        Default::default(),
        false,
        true,
    )
    .await;
    let kernel = Arc::new(crate::plugin::KernelHost::new());
    kernel.set_data_dir(Some(live.data.clone()));
    crate::plugin::caller::Caller::default()
        .allow_private_write()
        .scope(registry.doctor_of("write-probe", kernel))
        .await;
    live.turn("first", "Save the fixture.").await;
    gate.armed.store(true, Ordering::SeqCst);
    let commands = live.commands.clone();
    let cancel = tokio::spawn(async move {
        within("cancelled head began", gate.begun.notified()).await;
        commands.send(ClientCommand::Cancel).await.unwrap();
    });
    assert!(matches!(
        live.compact(1).await,
        AgentEvent::Compacted { .. }
    ));
    cancel.await.unwrap();
    assert!(
        pick!(log_events(&live.store), LogEvent::ContextCompacted { sections, .. } => sections)
            .iter()
            .all(|s| s.iter().all(|s| s.name != "memory/list"))
    );
    live.turn("next", "Continue.").await;
    assert!(
        block(&provider.body(3)["messages"].to_string())
            .unwrap()
            .contains(NOTE)
    );
    assert!(!live.data.join("write-probe").exists());
    live.send(ClientCommand::RunPluginCommand {
        name: "write-probe:probe".into(),
        args: wisp_protocol::Secret::new(""),
        request: RequestId(2),
    })
    .await;
    live.until(|e| {
        matches!(
            e,
            AgentEvent::CommandDone {
                answers: RequestId(2),
                ..
            }
        )
    })
    .await;
    let dir = live.data.join("write-probe");
    assert_eq!(fs::read(dir.join("command")).unwrap(), b"fixture");
    for purpose in ["doctor", "turn", "head"] {
        assert!(!dir.join(purpose).exists());
        assert!(
            tries
                .lock()
                .unwrap()
                .contains(&(purpose.to_owned(), Err(PrivateFileError::Unavailable)))
        );
    }
    live.finish().await;
}

#[tokio::test]
async fn an_authorized_invocation_cannot_replay_writes_in_nested_index_callbacks() {
    use wisp_plugin_api::host::Host;
    use wisp_plugin_api::index::{Entry, Kind};
    use wisp_plugin_api::private_files::{ChangeAction, Expected, PrivateFileError};
    use wisp_plugin_api::{BoxFuture, Plugin, Scope};

    fn write(host: &dyn Host, path: &str) -> Result<(), PrivateFileError> {
        host.private_files().change(
            path,
            Expected::Absent,
            ChangeAction::Write(b"fixture".to_vec()),
            None,
        )
    }
    struct IndexProbe(Arc<crate::plugin::PluginHost>, Arc<dyn Scope>);
    impl IndexProbe {
        async fn denied(&self, path: &str) {
            tokio::task::yield_now().await;
            assert_eq!(
                write(self.0.as_ref(), path),
                Err(PrivateFileError::Unavailable)
            );
            let mut again = || {
                assert_eq!(
                    write(self.0.as_ref(), path),
                    Err(PrivateFileError::Unavailable)
                )
            };
            self.1.run(&mut again);
            self.1
                .wrap(Box::pin(async {
                    tokio::task::yield_now().await;
                    assert_eq!(
                        write(self.0.as_ref(), path),
                        Err(PrivateFileError::Unavailable)
                    );
                }))
                .await;
        }
    }
    impl Plugin for IndexProbe {
        fn entries(&self) -> BoxFuture<'_, Vec<Entry>> {
            assert_eq!(
                write(self.0.as_ref(), "entries-sync"),
                Err(PrivateFileError::Unavailable)
            );
            Box::pin(async move {
                self.denied("entries-async").await;
                vec![Entry {
                    kind: Kind::Document,
                    name: "fixture-entry".into(),
                    summary: "Fixture".into(),
                    text: "Fixture".into(),
                }]
            })
        }
        fn open_entry<'a>(&'a self, _: &'a str) -> BoxFuture<'a, Result<String, String>> {
            assert_eq!(
                write(self.0.as_ref(), "open-sync"),
                Err(PrivateFileError::Unavailable)
            );
            Box::pin(async move {
                self.denied("open-async").await;
                Ok("Fixture.".into())
            })
        }
    }
    let scratch = ScratchDir::new("wisp-index-scope-", "private-files");
    let data = scratch.root.join("plugin-data");
    let kernel = Arc::new(crate::plugin::KernelHost::new());
    kernel.set_data_dir(Some(data.clone()));
    let host = Arc::new(
        crate::plugin::PluginHost::new("scope-probe", "0.4.0", &[], kernel.clone())
            .origin(wisp_plugin_api::manifest::Origin::Bundled),
    );
    let retained =
        crate::plugin::caller::Caller::call(tokio_util::sync::CancellationToken::new(), None)
            .allow_private_write()
            .scope(async {
                assert_eq!(write(host.as_ref(), "before"), Ok(()));
                let retained = host.kernel().unwrap().caller();
                let probe: Arc<dyn Plugin> = Arc::new(IndexProbe(host.clone(), retained.clone()));
                let sources = [("scope-probe".into(), probe.clone())]
                    .into_iter()
                    .collect();
                assert!(
                    kernel
                        .gather_index(&crate::tool::Toolbox::default(), &BTreeSet::new(), sources)
                        .await
                        .is_empty()
                );
                assert_eq!(
                    host.index().open("fixture-entry").await.unwrap(),
                    "Fixture."
                );
                assert_eq!(write(host.as_ref(), "after"), Ok(()));
                retained
            })
            .await;
    retained.run(&mut || {
        assert_eq!(
            write(host.as_ref(), "retained"),
            Err(PrivateFileError::Unavailable)
        )
    });
    let folder = data.join("scope-probe");
    for name in [
        "entries-sync",
        "entries-async",
        "open-sync",
        "open-async",
        "retained",
    ] {
        assert!(!folder.join(name).exists());
    }
    for name in ["before", "after"] {
        assert_eq!(fs::read(folder.join(name)).unwrap(), b"fixture");
    }
}

#[tokio::test]
async fn a_project_save_does_not_touch_a_symlinked_data_root() {
    use std::os::unix::fs::{PermissionsExt, symlink};

    let provider = scripted(vec![
        calling(
            "toolu_project",
            "remember",
            json!({"action":"save","scope":"project","name":"fixture-note","fact":NOTE}),
            500,
        ),
        answer("Done.", 650),
    ]);
    let mut live = Session::new(
        provider.clone(),
        tools(true),
        models(WISP),
        Default::default(),
        false,
        true,
    )
    .await;
    live.repository();
    let referent = live.scratch.dir("referent");
    let sentinel = referent.join("marker");
    fs::write(&sentinel, b"untouched").unwrap();
    fs::set_permissions(&referent, fs::Permissions::from_mode(0o755)).unwrap();
    fs::set_permissions(&sentinel, fs::Permissions::from_mode(0o644)).unwrap();
    symlink(&referent, &live.data).unwrap();
    live.turn("save", "Save the fixture note for this project.")
        .await;
    assert!(
        provider.body(1)["messages"]
            .to_string()
            .contains("This chat has no project memory.")
    );
    assert_eq!(fs::read(&sentinel).unwrap(), b"untouched");
    assert_eq!(
        referent.metadata().unwrap().permissions().mode() & 0o777,
        0o755
    );
    assert_eq!(
        sentinel.metadata().unwrap().permissions().mode() & 0o777,
        0o644
    );
    assert_eq!(fs::read_dir(&referent).unwrap().count(), 1);
    live.finish().await;
}

#[tokio::test]
async fn a_rejected_held_remember_call_never_echoes_unknown_input_keys() {
    let provider = scripted(vec![
        calling(
            "toolu_held",
            "remember",
            json!({"fixture-unexpected-label":"ignored"}),
            500,
        ),
        answer("Done.", 650),
    ]);
    let selected = tools(true).with_preload(vec!["echo".into()]);
    let mut live = Session::new(
        provider.clone(),
        selected,
        models(WISP),
        Default::default(),
        false,
        true,
    )
    .await;
    live.turn("save", "Check the fixture.").await;
    let cards =
        pick!(&live.seen, AgentEvent::ToolCallRequested { tool, display, .. } => (tool, display));
    let filtered: Vec<_> = cards
        .into_iter()
        .filter(|(tool, _)| tool.as_str() == "remember")
        .collect();
    let [(_, display)] = filtered.as_slice() else {
        panic!("expected one rejected tool card");
    };
    let shown = serde_json::to_string(display).unwrap();
    assert!(!shown.contains("fixture-unexpected-label"));
    assert!(shown.contains("Updating saved notes") && display.target.is_empty());
    assert!(
        !provider.sent()[0]
            .tools
            .iter()
            .any(|tool| tool.name == "remember")
    );
    assert!(!live.data.exists());
    live.finish().await;
}
