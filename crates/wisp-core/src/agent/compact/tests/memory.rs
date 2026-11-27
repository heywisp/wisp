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

fn remember(action: &str, name: &str, fact: Option<&str>) -> Value {
    let mut input = json!({"action":action,"scope":"you","name":name});
    if let Some(fact) = fact {
        input["fact"] = fact.into();
    }
    input
}

fn calling(id: &str, name: &str, input: Value, tokens: u64) -> Step {
    super::attachments::named_call_with(id, name, input, tokens, 40)
}

pub(in crate::agent) fn tools(memory: bool) -> Tools {
    tools_with(memory, None)
}

pub(in crate::agent) fn tools_with(memory: bool, extra: Option<(&str, &str)>) -> Tools {
    let bundles = wisp_plugins::bundled()
        .into_iter()
        .filter(|bundle| {
            bundle.manifest == wisp_plugin_compaction::MANIFEST
                || (memory && bundle.manifest == wisp_plugin_memory::MANIFEST)
                || extra.is_some_and(|(manifest, _)| bundle.manifest == manifest)
        })
        .chain([wisp_plugin_testkit::bundled()]);
    let (registry, problems) = Registry::new(bundles.collect(), &PluginConfig::default());
    assert!(problems.is_empty(), "{problems:?}");
    let on = ["echo"]
        .into_iter()
        .chain(memory.then_some("remember"))
        .chain(extra.map(|(_, tool)| tool))
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
        Self::new_seeded(
            provider,
            tools,
            model_settings,
            start_layers,
            (auto, user_input),
            None,
            rooted::tree(),
        )
        .await
    }

    pub(in crate::agent) async fn new_seeded(
        provider: Arc<dyn Provider>,
        tools: Tools,
        model_settings: ModelSettings,
        start_layers: crate::settings::host::StartLayers,
        (auto, user_input): (bool, bool),
        user_note: Option<(&str, &str)>,
        tree: crate::agents::TreeSetup,
    ) -> Self {
        let scratch = ScratchDir::new("wisp-memory-fixture-", "rooted");
        let data = scratch.root.join("plugin-data");
        if let Some((name, raw)) = user_note {
            let dir = data.join("memory/user");
            crate::secrets::create_folder(&dir).unwrap();
            fs::write(dir.join(name), raw).unwrap();
        }
        let store = MemoryStore::default();
        let start = "2000-06-05T01:00:00Z".parse().unwrap();
        let clock = if tree.keep_warm_main {
            ShiftedClock::paused(start, None)
        } else {
            ShiftedClock::at(start, None)
        };
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
            clock: Arc::new(clock),
            models: model_settings,
            start_layers,
            auto_compact: auto,
            user_input,
        };
        Self::open(inputs, scratch, store, crate::kit::Start::Fresh, tree).await
    }

    async fn open(
        inputs: Inputs,
        scratch: ScratchDir,
        store: MemoryStore,
        start: crate::kit::Start<'_>,
        tree: crate::agents::TreeSetup,
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
        let built = Rooted::build(inputs.clone().kit(), spec, tree)
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
        Self::open(inputs, scratch, store, start, rooted::tree()).await
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

#[tokio::test]
async fn rooted_show_uses_the_local_transcript_without_provider_calls() {
    let provider = scripted(vec![]);
    let raw =
        "---\nname: sample\ndescription: Square tiles.\nsaved: 2000-01-01\n---\nKeep the top edge.";
    let mut live = Session::new_seeded(
        provider.clone(),
        tools(true),
        models(WISP),
        Default::default(),
        (false, true),
        Some(("sample.md", raw)),
        rooted::tree(),
    )
    .await;
    let request = wisp_protocol::RequestId(1);
    live.send(ClientCommand::RunPluginCommand {
        name: "memory:memory".into(),
        args: wisp_protocol::Secret::new("show you sample"),
        request,
    })
    .await;
    let event = live
        .until(
            |event| matches!(event, AgentEvent::CommandDone { answers, .. } if *answers == request),
        )
        .await;
    let AgentEvent::CommandDone {
        outcome: wisp_protocol::CommandOutcome::Lines { lines },
        args_safe_to_echo: true,
        ..
    } = &event
    else {
        panic!("{event:?}");
    };
    let copy = serde_json::to_string(lines).unwrap();
    for expected in [
        "Memory",
        "Square tiles.",
        "Keep the top edge.",
        "No final newline",
        "/memory delete you sample",
    ] {
        assert!(copy.contains(expected));
    }
    assert!(provider.sent().is_empty());
    live.finish().await;
}

#[cfg(unix)]
#[tokio::test]
async fn standalone_doctor_uses_real_project_context_without_assuming_a_root_agent() {
    use crate::plugin::{DoctorWorkspace, doctor_rows};
    let scratch = ScratchDir::new("wisp-memory-doctor-", "standalone");
    let cwd = scratch.workspace().root;
    crate::instructions::make_repository(&cwd).unwrap();
    let data = scratch.root.join("plugin-data");
    let (registry, problems) = Registry::new(
        vec![wisp_plugin_memory::bundled()],
        &PluginConfig::default(),
    );
    assert!(problems.is_empty());
    let read = |plugin_data| {
        doctor_rows(
            &registry,
            &EMPTY_SET,
            Default::default(),
            &REDACTIONS,
            DoctorWorkspace {
                cwd: cwd.clone(),
                plugin_data,
            },
        )
    };
    static EMPTY_SET: std::sync::LazyLock<std::collections::BTreeSet<String>> =
        std::sync::LazyLock::new(Default::default);
    static REDACTIONS: std::sync::LazyLock<crate::session::Redactions> =
        std::sync::LazyLock::new(Default::default);
    let memory_rows = |rows: Vec<wisp_protocol::DoctorRow>| {
        rows.into_iter()
            .filter(|row| row.key.starts_with("memory"))
            .collect::<Vec<_>>()
    };
    assert_eq!(memory_rows(read(None).await)[0].key, "memory");
    crate::secrets::create_folder(&data.join("memory/projects")).unwrap();
    assert!(memory_rows(read(Some(data.clone())).await).is_empty());
    assert!(!data.join("core/project-secret").exists());
    let project = data
        .join("memory/projects")
        .join(crate::workspace::project_key(&cwd, &data).unwrap());
    crate::secrets::create_folder(&project).unwrap();
    let user = data.join("memory/user");
    crate::secrets::create_folder(&user).unwrap();
    let note = |dir: &std::path::Path, name: &str| {
        fs::write(
            dir.join(format!("{name}.md")),
            format!("---\nname: {name}\ndescription: Tile sample.\nsaved: 2000-01-01\n---\n"),
        )
        .unwrap()
    };
    note(&project, "project-sample");
    note(&user, "private-sample");
    fs::write(project.join("broken.md"), [255]).unwrap();
    const HELD: &str = "CANARY-doctor-refused-note-body";
    crate::session::register_secrets([HELD.to_owned()]);
    let refused = "refused-sample";
    fs::write(
        project.join(format!("{refused}.md")),
        format!("---\nname: {refused}\ndescription: Tile sample.\nsaved: 2000-01-01\n---\n{HELD}"),
    )
    .unwrap();
    for i in 0..125 {
        note(&project, &format!("tile-{i:03}"));
    }
    let rows = memory_rows(read(Some(data)).await);
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].key, "memory project");
    assert_eq!(
        rows[0].cells,
        ["126 saved", "1 refused", "1 unreadable", "scope at cap"]
            .map(|text| wisp_protocol::DoctorCell::Text { text: text.into() })
    );
}

#[cfg(any(target_os = "linux", target_os = "macos"))]
#[tokio::test]
async fn real_search_sees_each_current_disk_note_and_not_stale_or_unsafe_edits() {
    use wisp_plugin_api::manifest::Origin;
    use wisp_plugin_testkit::host::FakeCall;
    const HELD: &str = "CANARY-live-note-body";
    let scratch = ScratchDir::new("wisp-memory-search-", "live-index");
    let workspace = scratch.workspace();
    crate::instructions::make_repository(&workspace.root).unwrap();
    let data = scratch.root.join("plugin-data");
    let key = crate::workspace::project_key(&workspace.root, &data).unwrap();
    let dir = data.join("memory/projects").join(key);
    crate::secrets::create_folder(&dir).unwrap();
    let kernel = Arc::new(crate::plugin::KernelHost::new());
    kernel.bind(&crate::jobs::Processes::default(), &workspace);
    kernel.set_data_dir(Some(data));
    let memory = wisp_plugin_memory::bundled();
    let search = wisp_plugin_search::bundled();
    let make = |id, bundle: wisp_plugin_api::Bundled| {
        (bundle.instantiate)(Arc::new(
            crate::plugin::PluginHost::new(id, "0.4.0", &[], kernel.clone())
                .origin(Origin::Bundled),
        ))
    };
    let memory = make("memory", memory);
    let search = make("search", search);
    kernel
        .gather_index_sources(
            &crate::tool::Toolbox::default(),
            &Default::default(),
            crate::plugin::host::IndexSources {
                live: [("memory".into(), memory.clone())].into_iter().collect(),
                ..Default::default()
            },
        )
        .await;
    let run = |tool: &'static str, input: Value| {
        let search = search.clone();
        async move {
            let prepared = search.prepare(tool, &input.to_string()).await.unwrap();
            search
                .run(prepared, &FakeCall::default())
                .await
                .result
                .unwrap()
        }
    };
    let value = |name, body| {
        format!("---\nname: {name}\ndescription: Sample tiles.\nsaved: 2000-01-01\n---\n{body}")
    };
    let list = || run("search", json!({"query":"tiles"}));
    let open = |name: &str| run("use", json!({"name":format!("memory.project.{name}")}));
    let old = dir.join("old.md");
    let moved = dir.join("moved.md");
    fs::write(&old, value("old", "First sample.")).unwrap();
    assert!(open("old").await.contains("First sample."));
    fs::write(&old, value("old", "Edited sample.")).unwrap();
    assert!(open("old").await.contains("Edited sample."));
    fs::rename(&old, &moved).unwrap();
    fs::write(&moved, value("moved", "Moved sample.")).unwrap();
    assert_eq!(open("old").await, "entry not found");
    assert!(list().await.contains("memory.project.moved"));
    crate::session::register_secrets([HELD.to_owned()]);
    for bytes in [value("moved", HELD).into_bytes(), vec![255]] {
        fs::write(&moved, bytes).unwrap();
        assert_eq!(list().await, "nothing found");
        assert_eq!(open("moved").await, "entry not found");
    }
}
