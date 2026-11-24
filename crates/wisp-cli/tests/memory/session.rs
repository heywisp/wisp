use std::collections::BTreeSet;
use std::future::Future;
use std::io::Cursor;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use tokio::sync::mpsc;
use wisp_core::agent::{AgentTask, Connection, LogContext};
use wisp_core::agents::{Limits, LoadedTree, TreeSetup};
use wisp_core::context::ModelSettings;
use wisp_core::kit::{self, Kit};
use wisp_core::models::{ConfigModels, ModelTable};
use wisp_core::plugin::{KernelHost, PluginConfig, Prestarted, Registry, Tools};
use wisp_core::provider::Provider;
use wisp_core::provider::anthropic::{AnthropicProvider, Credential};
use wisp_core::session::Redactions;
use wisp_core::settings::host::StartLayers;
use wisp_core::workspace::Workspace;
use wisp_protocol::log::CredentialSource;
use wisp_protocol::settings::{Place, SettingValue, SitePath, find};
use wisp_protocol::{
    AgentEvent, ClientCommand, DeliveryMode, ImageData, InputId, InstructionsSource, RequestId,
};

use super::client::Client;
use super::scratch::ScratchDir;
use super::script::{Scenario, Turn};
use super::server::{Script, Seen, Server};

const WAIT: Duration = Duration::from_secs(120);
const AGENTS_MD_BYTES: usize = 20 * 1024;
const TESTKIT_TOOLS: [&str; 3] = ["echo", "snap", "starter"];

pub(crate) fn on_stack<F: Future<Output = ()>>(work: impl FnOnce() -> F + Send) {
    std::thread::scope(|scope| {
        std::thread::Builder::new()
            .name("memory".to_owned())
            .stack_size(wisp_core::runtime::THREAD_STACK_BYTES)
            .spawn_scoped(scope, move || {
                let runtime =
                    wisp_core::runtime::build(&mut tokio::runtime::Builder::new_current_thread())
                        .unwrap();
                runtime.block_on(work());
            })
            .unwrap()
            .join()
            .unwrap();
    });
}

fn scratch(name: &str) -> ScratchDir {
    let scratch = ScratchDir::new("wisp-memory-", name);
    if let Some(repo) =
        wisp_core::instructions::repository(&scratch.root, &wisp_core::instructions::Disk)
    {
        panic!(
            "{} is inside a repository at {}; point TMPDIR outside any repository",
            scratch.root.display(),
            repo.display()
        );
    }
    for dir in ["sessions", "work", "tmp", "home"] {
        scratch.dir(dir);
    }
    let mut agents = String::from("# Project notes\n\n");
    agents.push_str(&super::script::filler(AGENTS_MD_BYTES, 99));
    scratch.write("work/AGENTS.md", &agents);
    scratch
}

pub(crate) struct Rig {
    pub(crate) scratch: ScratchDir,
    pub(crate) script: Arc<Script>,
    pub(crate) server: Server,
    pub(crate) turns: Vec<Turn>,
    pub(crate) images: Vec<Option<ImageData>>,
    pub(crate) models: String,
    pub(crate) background: bool,
    pub(crate) children: Vec<String>,
}

impl Rig {
    pub(crate) fn new(name: &str, scenario: Scenario) -> Rig {
        let Scenario {
            turns,
            script,
            models,
            background,
            children,
        } = scenario;
        let script = Arc::new(script);
        let server = super::server::serve(Arc::clone(&script));
        let images = turns
            .iter()
            .map(|t| {
                t.image.map(|seed| ImageData {
                    data: wisp_media::encode_base64(&noise_png(seed)).into(),
                })
            })
            .collect();
        Rig {
            scratch: scratch(name),
            script,
            server,
            turns,
            images,
            models,
            background,
            children,
        }
    }

    pub(crate) fn seen(&self) -> &Seen {
        &self.server.seen
    }

    fn provider(&self) -> Arc<dyn Provider> {
        Arc::new(
            AnthropicProvider::new(
                &Credential::ApiKey("memory-made-up-key".into()),
                &self.server.url,
            )
            .unwrap()
            .on_official_origin(),
        )
    }

    async fn kit(&self) -> (Kit, TreeSetup) {
        let mut bundled = wisp_plugins::bundled();
        bundled.push(wisp_plugin_testkit::bundled());
        let (registry, problems) = Registry::new(bundled, &PluginConfig::default());
        assert!(problems.is_empty(), "{problems:?}");
        let registry = Arc::new(registry);
        let mut on: BTreeSet<String> = registry.default_on();
        on.extend(TESTKIT_TOOLS.iter().map(|t| (*t).to_owned()));
        let catalog = registry.catalog(&registry.default_on(), &[]);
        let tools = Tools::new(Arc::clone(&registry), on);
        let started =
            wisp_core::kit::BuildTools::start(&tools, kit::ROOT, Arc::new(KernelHost::new())).await;
        let provider = self.provider();
        let context = log_context();
        let mut start_layers = StartLayers::default();
        if !self.background {
            start_layers.saved.insert(
                SitePath::new(
                    Place::Global,
                    find("context.background_compaction").unwrap(),
                ),
                SettingValue::Bool(false),
                1,
            );
        }
        let kit = Kit {
            connect: Arc::new(kit::Connected {
                provider: Arc::clone(&provider),
                ready: vec![Connection {
                    name: "env".into(),
                    provider,
                    context: context.clone(),
                    priced: true,
                }],
                unavailable: Vec::new(),
                active: "env".into(),
                env_account: None,
            }),
            tools: Arc::new(Prestarted::new(started, tools)),
            logs: Arc::new(kit::JsonlLogs {
                root_id: None,
                on: true,
                dir: Some(self.scratch.root.join("sessions")),
            }),
            log_context: context,
            models: ModelSettings::new(
                ModelTable::shipped().unwrap(),
                ConfigModels::parse(&self.models).unwrap(),
            ),
            options: wisp_core::provider::RequestOptions {
                refusal_fallback: true,
                debug_cache: false,
                message_threads: true,
            },
            retry_budget: None,
            count_tokens: true,
            auto_compact: true,
            user_input: true,
            naming: false,
            workspace: Ok(Workspace {
                root: self.scratch.root.join("work"),
                home: None,
                temp: self.scratch.root.join("tmp"),
            }),
            instructions: kit::InstructionsDirs {
                config_dir: None,
                home: None,
            },
            plugin_data: None,
            start_layers,
            settings_host: None,
            endpoint_host: None,
            clock: Arc::new(wisp_core::clock::SystemClock),
            plan_usage: wisp_core::plan_usage::PlanUsageCache::default(),
            usage_display: wisp_core::plan_usage::Display::Enabled,
            warmth: wisp_core::warmth::WarmthTable::default(),
            lanes: wisp_core::lanes::Lanes::default(),
            mcp: wisp_core::mcp::Service::default(),
        };
        let tree = TreeSetup {
            presets: Default::default(),
            tools_off: wisp_protocol::agents::tools_off_names(&catalog),
            followers: wisp_protocol::agents::tools_off_followers(&catalog),
            limits: Limits::default(),
            instructions: instructions(),
            loaded: LoadedTree::default(),
            keep_warm_main: false,
            restarted: false,
            start: None,
            unusable: Vec::new(),
        };
        (kit, tree)
    }

    pub(crate) async fn fresh(&self, client: bool) -> Live {
        let (kit, tree) = self.kit().await;
        let built = kit.build(spec(kit::Start::Fresh)).await.unwrap();
        assert!(built.instructions.is_some(), "no instructions were loaded");
        let log = match &built.log {
            kit::LogOutcome::Opened { path, .. } => path.clone(),
            other => panic!("the session log didn't open: {other:?}"),
        };
        let (commands, events, task) = wisp_core::agents::start_root(kit, tree, built.agent);
        Live {
            commands,
            events,
            task,
            client: client.then(Client::new),
            log,
            running: 0,
            request: 0,
            compactions: 0,
        }
    }

    pub(crate) async fn resume(&self, log: &Path, client: bool) -> Live {
        let (kit, mut tree) = self.kit().await;
        let id = log.file_stem().unwrap().to_str().unwrap().to_owned();
        let provider = self.provider();
        let providers = move |_: Option<&str>| Some(Arc::clone(&provider));
        let logs = kit::JsonlLogs {
            root_id: None,
            on: true,
            dir: Some(self.scratch.root.join("sessions")),
        };
        let request = wisp_core::resume::Request {
            id: Some(&id),
            unlocked: false,
        };
        let wisp_core::resume::Resumed {
            log: session_log,
            path,
            recovered,
            truncated_bytes,
            logged,
            rebuilt,
            warnings,
            ..
        } = wisp_core::resume::open(&logs, &request, None, &providers).unwrap();
        assert!(warnings.is_empty(), "{warnings:?}");
        assert!(rebuilt.is_some());
        let built = kit
            .build(spec(kit::Start::Resume(kit::Resume {
                log: session_log,
                recovered: &recovered,
                truncated_bytes,
                logged: logged.as_ref(),
                rebuilt,
            })))
            .await
            .unwrap();
        let agent = built.agent;
        let lock = wisp_core::resume::Request {
            id: None,
            unlocked: false,
        }
        .lock();
        tree.loaded =
            wisp_core::agents::load_tree(&kit, &tree, &agent, &recovered.records, lock, &[]).await;
        let shown = client.then(|| wisp_tui::app::resumed::Resumed {
            view: agent.view(&recovered.records),
            ..Default::default()
        });
        drop(logged);
        drop(recovered);
        let (commands, events, task) = wisp_core::agents::start_root(kit, tree, agent);
        let mut live = Live {
            commands,
            events,
            task,
            client: client.then(Client::new),
            log: path,
            running: 0,
            request: 0,
            compactions: 0,
        };
        if let (Some(client), Some(shown)) = (live.client.as_mut(), shown) {
            client.resumed(&shown);
            client.paint();
        }
        live
    }
}

fn log_context() -> LogContext {
    LogContext {
        endpoint: "https://api.anthropic.com".into(),
        endpoint_name: "env".into(),
        credential_source: CredentialSource::Environment,
        redactions: Redactions::default(),
    }
}

fn instructions() -> kit::InstructionsChoice {
    kit::InstructionsChoice {
        source: InstructionsSource::Auto,
        warn_tokens: 8_000,
    }
}

fn spec(start: kit::Start<'_>) -> kit::Spec<'_> {
    let session = wisp_core::agent::default_session();
    kit::Spec {
        path: kit::ROOT.to_owned(),
        choices: kit::Choices {
            model: session.model,
            effort: session.effort,
            cache_ttl: session.cache_ttl,
            message_time: session.message_time,
            context_note: session.context_note,
        },
        instructions: instructions(),
        instructions_flag: None,
        extra: None,
        extra_flag: None,
        preset: None,
        start,
    }
}

fn noise_png(seed: u64) -> Vec<u8> {
    let mut state = seed.wrapping_add(0x9e37_79b9_7f4a_7c15);
    let image = image::RgbImage::from_fn(600, 400, |_, _| {
        state = state
            .wrapping_mul(6_364_136_223_846_793_005)
            .wrapping_add(1_442_695_040_888_963_407);
        let [r, g, b, ..] = (state >> 32).to_le_bytes();
        image::Rgb([r, g, b])
    });
    let mut bytes = Cursor::new(Vec::new());
    image::DynamicImage::ImageRgb8(image)
        .write_to(&mut bytes, image::ImageFormat::Png)
        .unwrap();
    bytes.into_inner()
}

pub(crate) struct Live {
    pub(crate) commands: mpsc::Sender<ClientCommand>,
    pub(crate) events: mpsc::Receiver<AgentEvent>,
    pub(crate) task: AgentTask,
    pub(crate) client: Option<Client>,
    pub(crate) log: PathBuf,
    pub(crate) running: i64,
    pub(crate) request: u64,
    pub(crate) compactions: usize,
}

impl Live {
    pub(crate) async fn turn(&mut self, n: usize, rig: &Rig, waits: &[String]) {
        let turn = &rig.turns[n];
        let image = rig.images[n].clone();
        if let Some(client) = self.client.as_mut() {
            client.typed(&turn.text, image.is_some());
        }
        self.commands
            .send(ClientCommand::Send {
                id: InputId(format!("input-{n}")),
                text: turn.text.clone(),
                images: image.into_iter().collect(),
                mode: DeliveryMode::Queue,
                turn: None,
                replaces: None,
            })
            .await
            .unwrap();
        let mut wanted = vec![turn.last.as_str()];
        wanted.extend(waits.iter().map(String::as_str));
        self.settle(&rig.script, &wanted).await;
        if let Some(client) = self.client.as_mut() {
            client.paint();
        }
    }

    async fn settle(&mut self, script: &Script, wanted: &[&str]) {
        let done = |script: &Script| wanted.iter().all(|m| script.served(m));
        let mut asked = false;
        loop {
            let event = tokio::time::timeout(WAIT, self.events.recv())
                .await
                .unwrap_or_else(|_| panic!("no event within {WAIT:?} while waiting for {wanted:?}"))
                .unwrap_or_else(|| panic!("the agent stopped while waiting for {wanted:?}"));
            if let Some(client) = self.client.as_mut() {
                client.apply(&event);
            }
            match &event {
                AgentEvent::TurnStarted { .. } => self.running += 1,
                AgentEvent::TurnEnded { .. } => self.running -= 1,
                AgentEvent::RewindPoints { answers, .. } if *answers == RequestId(self.request) => {
                    asked = false;
                    if self.running == 0 && done(script) {
                        return;
                    }
                }
                AgentEvent::Compacted { .. } => self.compactions += 1,
                AgentEvent::CompactionFailed { .. } => panic!("a compaction failed: {event:?}"),
                _ => {}
            }
            if !asked && self.running == 0 && done(script) {
                self.request += 1;
                self.commands
                    .send(ClientCommand::RewindPoints {
                        request: RequestId(self.request),
                    })
                    .await
                    .unwrap();
                asked = true;
            }
        }
    }

    pub(crate) fn drop_client(&mut self) {
        self.client = None;
    }

    pub(crate) async fn close(self) {
        let Live {
            commands,
            mut events,
            task,
            client,
            ..
        } = self;
        drop(client);
        drop(commands);
        while tokio::time::timeout(WAIT, events.recv())
            .await
            .unwrap()
            .is_some()
        {}
        tokio::time::timeout(WAIT, task.handle)
            .await
            .unwrap()
            .unwrap();
    }
}
