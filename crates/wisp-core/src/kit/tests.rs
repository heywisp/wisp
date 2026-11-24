use std::sync::Mutex;
use std::sync::atomic::{AtomicUsize, Ordering};

use wisp_protocol::log::LogEvent;

use super::*;
use crate::provider::anthropic::{AnthropicProvider, Credential};
use crate::session::MemoryStore;
use crate::test_guard::{log_context, protocol_fixture};
use futures::FutureExt;

fn provider(base: &str) -> Arc<dyn Provider> {
    Arc::new(AnthropicProvider::new(&Credential::ApiKey("sk-kit".into()), base).unwrap())
}

#[derive(Default)]
struct CountedLogs {
    opened: AtomicUsize,
    store: MemoryStore,
}

impl OpenLog for CountedLogs {
    fn open(&self, path: &str) -> Result<Option<OpenedLog>, String> {
        self.opened.fetch_add(1, Ordering::SeqCst);
        Ok(Some(OpenedLog {
            log: SessionLog::new(Box::new(self.store.clone()), path),
            path: PathBuf::from(path),
            unlocked: Some("no locks here".into()),
        }))
    }
}

struct Failing;

impl OpenLog for Failing {
    fn open(&self, _path: &str) -> Result<Option<OpenedLog>, String> {
        Err("the disk is full".into())
    }
}

fn workspace() -> Workspace {
    let root = std::env::temp_dir();
    Workspace {
        root: root.clone(),
        home: None,
        temp: root,
    }
}

fn tools(on: &[&str]) -> crate::plugin::Tools {
    let (registry, problems) = crate::plugin::Registry::new(
        wisp_plugins::bundled(),
        &crate::plugin::PluginConfig::default(),
    );
    assert!(problems.is_empty(), "{problems:?}");
    crate::plugin::Tools::new(
        Arc::new(registry),
        on.iter().map(|s| (*s).to_owned()).collect(),
    )
}

fn kit(connect: Arc<dyn Connect>, tools: crate::plugin::Tools, logs: Arc<dyn OpenLog>) -> Kit {
    Kit {
        connect,
        tools: Arc::new(tools),
        logs,
        log_context: log_context(),
        models: ModelSettings::shipped_only(),
        options: RequestOptions {
            refusal_fallback: true,
            debug_cache: false,
            message_threads: true,
        },
        retry_budget: None,
        count_tokens: true,
        auto_compact: true,
        user_input: false,
        naming: false,
        workspace: Ok(workspace()),
        pin_test_environment: false,
        instructions: InstructionsDirs::default(),
        plugin_data: None,
        start_layers: StartLayers::default(),
        settings_host: None,
        endpoint_host: None,
        clock: Arc::new(crate::clock::SystemClock),
        plan_usage: crate::plan_usage::PlanUsageCache::default(),
        usage_display: crate::plan_usage::Display::Disabled,
        warmth: crate::warmth::WarmthTable::default(),
        lanes: crate::lanes::Lanes::default(),
        mcp: crate::mcp::Service::default(),
    }
}

fn spec(path: &str) -> Spec<'static> {
    Spec {
        path: path.into(),
        choices: Choices {
            model: "claude-sonnet-4-5".into(),
            effort: Effort::Low,
            cache_ttl: CacheTtl::FiveMinutes,
            message_time: MessageTime::Off,
            context_note: false,
        },
        instructions: InstructionsChoice {
            source: InstructionsSource::None,
            warn_tokens: 8_000,
        },
        instructions_flag: None,
        extra: None,
        extra_flag: None,
        preset: None,
        start: Start::Fresh,
    }
}

fn connected(path: &str) -> Connected {
    let base = format!("http://127.0.0.1:9{}", path.trim_end_matches('/'));
    Connected {
        provider: provider(&base),
        ready: vec![Connection {
            name: "env".into(),
            provider: provider(&base),
            context: log_context(),
            priced: true,
        }],
        unavailable: Vec::new(),
        active: "env".into(),
        env_account: None,
    }
}

#[tokio::test]
async fn the_root_agent_is_built_after_stored_mcp_keys_are_held() {
    use crate::secrets::StoreKind;
    use crate::secrets::keyring::{MemoryKeyring, MemoryMode};

    let folder = tempfile::tempdir().unwrap();
    let config = folder.path().join("config.toml");
    let server = crate::config::McpServer {
        command: "/dev/null/not-a-program".into(),
        args: Vec::new(),
        keys: vec!["WISP_TEST_TOKEN".into()],
    };
    let path = crate::oauth::mcp_profile(&config, "keys/keyed").unwrap();
    let memory = MemoryKeyring::new();
    crate::oauth::memory_store(&path, &memory);
    const KEY: &str = "CANARY-kept-mcp-key-held-before-root-agent";
    let kept = serde_json::json!({
        "identity": crate::session::sha256_hex(
            &serde_json::json!(["keyed", server.command, server.args]).to_string()
        ),
        "keys": {"WISP_TEST_TOKEN": KEY},
    });
    let locks = crate::oauth::lock_profile(&path).unwrap();
    crate::oauth::persist_value_locked(&path, &kept, StoreKind::Keyring, &locks).unwrap();
    drop(locks);
    assert_eq!(crate::session::held().mask(KEY), KEY);
    let logs = Arc::new(CountedLogs::default());
    let mut kit = kit(Arc::new(connected(ROOT)), tools(&[]), logs.clone());
    kit.mcp = crate::mcp::Service::new(
        [("keyed".into(), server)].into(),
        folder.path().to_owned(),
        None,
    )
    .with_config(config, crate::endpoints::PathShown::Given)
    .with_store(StoreKind::Keyring);
    memory.set_mode(MemoryMode::Locked);
    let mut build = kit.build(spec(ROOT));
    let waiting = tokio::time::timeout(Duration::from_millis(100), &mut build)
        .await
        .is_err();
    memory.set_mode(MemoryMode::Working);
    assert!(waiting, "the root agent must wait for stored MCP keys");
    assert_eq!(logs.opened.load(Ordering::SeqCst), 0);
    build.await.unwrap();
    assert_eq!(crate::session::held().mask(KEY), crate::session::REDACTED);
}

#[test]
fn a_spec_without_a_logged_session_keeps_its_choices() {
    let session = spec(ROOT).session();
    assert_eq!(session.model, "claude-sonnet-4-5");
    assert_eq!(session.effort, Effort::Low);
    assert_eq!(session.cache_ttl, CacheTtl::FiveMinutes);
    assert_eq!(session.message_time, MessageTime::Off);
    assert!(!session.context_note);
    assert_eq!(session.max_tokens, agent::DEFAULT_MAX_TOKENS);
    assert!(spec(ROOT).logged().is_none());
    let mut variant = spec(ROOT);
    variant.choices.model = "gemini-4-carbon-medium".into();
    variant.choices.effort = Effort::Max;
    assert_eq!(variant.session().effort, Effort::Medium);
}

#[test]
fn each_path_gets_its_own_connections_and_log() {
    let asked = Arc::new(Mutex::new(Vec::new()));
    let seen = asked.clone();
    let connect = Arc::new(move |path: &str| {
        seen.lock().unwrap().push(path.to_owned());
        Ok(connected(path))
    });
    let logs = Arc::new(CountedLogs::default());
    let kit = kit(connect, tools(&[]), logs.clone());
    let root = kit.build(spec(ROOT)).now_or_never().unwrap().unwrap();
    let child = kit.build(spec("/review")).now_or_never().unwrap().unwrap();
    assert_eq!(*asked.lock().unwrap(), [ROOT, "/review"]);
    assert_eq!(logs.opened.load(Ordering::SeqCst), 2);
    assert_eq!(
        child.log,
        LogOutcome::Opened {
            path: PathBuf::from("/review"),
            unlocked: Some("no locks here".into())
        }
    );
    assert_eq!(root.agent.session().model, "claude-sonnet-4-5");
}

#[test]
fn an_agent_on_a_connection_with_a_model_list_takes_its_window_from_the_list() {
    use crate::agent::testing::{Fake, Knobs};
    let connect = Arc::new(|path: &str| {
        let knobs = Knobs {
            list: Some(Arc::new(crate::models::list::tests::sample())),
            ..Knobs::default()
        };
        let listed: Arc<dyn Provider> = Fake::with(knobs, []);
        let mut connected = connected(path);
        connected.provider = listed.clone();
        connected.ready[0].provider = listed;
        Ok(connected)
    });
    let kit = kit(connect, tools(&[]), Arc::new(CountedLogs::default()));
    let mut gpt = spec(ROOT);
    gpt.choices.model = "gpt-6.1-sol".into();
    let built = kit.build(gpt).now_or_never().unwrap().unwrap();
    let wisp_protocol::AgentEvent::ContextChanged { window, .. } = built.agent.context_now() else {
        panic!("not a context event");
    };
    assert_eq!(window, Some(500_000));
}

#[test]
fn a_failed_connection_builds_nothing() {
    let logs = Arc::new(CountedLogs::default());
    let connect = Arc::new(|_: &str| Err("no key for env".to_owned()));
    let kit = kit(connect, tools(&[]), logs.clone());
    let Err(e) = kit.build(spec(ROOT)).now_or_never().unwrap() else {
        panic!("built without a connection");
    };
    assert_eq!(e, KitError::Connect("no key for env".into()));
    assert_eq!(e.to_string(), "no key for env");
    assert_eq!(logs.opened.load(Ordering::SeqCst), 0);
}

#[tokio::test]
async fn a_tool_that_asks_with_no_one_to_answer_opens_no_log() {
    let logs = Arc::new(CountedLogs::default());
    for tools in [
        tools(&["ask_user"]),
        tools(&["ask_user"]).with_preload(vec!["none".into()]),
    ] {
        let asking = kit(Arc::new(connected(ROOT)), tools, logs.clone());
        let Err(e) = asking.build(spec(ROOT)).await else {
            panic!("built a session that asks with no one to answer");
        };
        assert!(matches!(e, KitError::Channel(_)), "{e:?}");
        assert_eq!(
            e.to_string(),
            "ask_user asks the user, so the session needs a client that can answer"
        );
        assert_eq!(logs.opened.load(Ordering::SeqCst), 0);
    }

    #[cfg(unix)]
    {
        let headless = kit(
            Arc::new(connected(ROOT)),
            tools(&["bash", "process"]),
            logs.clone(),
        );
        let built = headless.build(spec(ROOT)).await.unwrap();
        let toolbox = built.agent.toolbox();
        assert_eq!(toolbox.names(), ["bash", "process"]);
        assert!(toolbox.questions().iter().any(|name| name == "ask_user"));
        assert_eq!(logs.opened.load(Ordering::SeqCst), 1);
    }
}

#[test]
fn a_missing_workspace_is_a_note_without_tools_and_an_error_with_them() {
    let logs = Arc::new(CountedLogs::default());
    let mut chat = kit(Arc::new(connected(ROOT)), tools(&[]), logs.clone());
    chat.workspace = Err("could not find the working directory: gone".into());
    let built = chat.build(spec(ROOT)).now_or_never().unwrap().unwrap();
    assert_eq!(
        built.workspace,
        Some(WorkspaceNote::Missing(
            "could not find the working directory: gone".into()
        ))
    );
    assert!(built.instructions.is_none());

    let tools = tools(&["ask_user"]);
    let mut tooled = kit(Arc::new(connected(ROOT)), tools, logs.clone());
    tooled.user_input = true;
    tooled.workspace = Err("could not find the working directory: gone".into());
    let Err(e) = tooled.build(spec(ROOT)).now_or_never().unwrap() else {
        panic!("built tools with no workspace");
    };
    assert_eq!(
        e,
        KitError::Workspace("could not find the working directory: gone".into())
    );
    assert_eq!(logs.opened.load(Ordering::SeqCst), 1);
}

#[test]
fn a_log_that_fails_to_open_still_builds_the_agent() {
    let kit = kit(Arc::new(connected(ROOT)), tools(&[]), Arc::new(Failing));
    let built = kit.build(spec(ROOT)).now_or_never().unwrap().unwrap();
    assert_eq!(built.log, LogOutcome::Failed("the disk is full".into()));
    assert!(built.instructions.is_some());
}

fn child_spec(endpoint: &str) -> ChildSpec {
    ChildSpec {
        path: AgentPath::new("/review"),
        model: "claude-sonnet-4-5".into(),
        effort: Effort::Low,
        cache_ttl: CacheTtl::OneHour,
        message_time: MessageTime::Off,
        context_note: false,
        instructions: InstructionsChoice {
            source: InstructionsSource::None,
            warn_tokens: 8_000,
        },
        cwd: std::env::temp_dir(),
        tools: Vec::new(),
        parent: ParentLink {
            session_id: "s-root".into(),
            agent: AgentPath::new("/review"),
            depth: 1,
            config: wisp_protocol::SpawnChoices::default(),
            todos: Vec::new(),
        },
        endpoint: endpoint.into(),
        preset_instructions: None,
        plugins: crate::plugin::PluginSwitches::default(),
    }
}

fn two_endpoints(path: &str) -> Connected {
    let mut connected = connected(path);
    let mut other = connected.ready[0].clone();
    other.name = "manifest".into();
    connected.ready.push(other);
    connected
}

#[tokio::test]
async fn a_child_uses_its_callers_endpoint_and_refuses_one_it_cannot_reach() {
    let connect = Arc::new(|path: &str| Ok(two_endpoints(path)));
    let kit = kit(connect, tools(&[]), Arc::new(CountedLogs::default()));
    let child = kit.build_child(child_spec("manifest")).await.unwrap();
    assert_eq!(child.endpoint_name(), "manifest");
    let child = kit.build_child(child_spec("env")).await.unwrap();
    assert_eq!(child.endpoint_name(), "env");
    let Err(e) = kit.build_child(child_spec("gone")).await else {
        panic!("built a child on an endpoint it can't reach")
    };
    assert!(matches!(e, KitError::Connect(_)), "{e:?}");
}

#[tokio::test]
async fn a_child_whose_log_fails_to_open_is_not_built() {
    let connect = Arc::new(|path: &str| Ok(connected(path)));
    let kit = kit(connect, tools(&[]), Arc::new(Failing));
    let Err(e) = kit.build_child(child_spec("env")).await else {
        panic!("built a child without its log")
    };
    assert_eq!(e, KitError::Log("the disk is full".into()));
}

#[tokio::test]
async fn the_root_and_its_children_share_the_kits_warmth_table() {
    use crate::provider::PrefixBytes;
    use crate::warmth::{Fingerprint, Heat};
    let connect = Arc::new(|path: &str| Ok(connected(path)));
    let kit = kit(connect, tools(&[]), Arc::new(CountedLogs::default()));
    let print = |tools: &[u8]| {
        Fingerprint::new(
            "env",
            &PrefixBytes {
                model: b"m".to_vec(),
                effort: b"e".to_vec(),
                thinking: b"t".to_vec(),
                tools: tools.to_vec(),
                system: b"s".to_vec(),
            },
        )
    };
    let root = kit.build(spec(ROOT)).await.unwrap().agent;
    let child = kit.build_child(child_spec("env")).await.unwrap();
    let from_child = child.warmth_table().sent(print(b"child"));
    let from_root = root.warmth_table().sent(print(b"root"));
    assert_eq!(kit.warmth.heat(print(b"child").key()), Heat::Warming);
    assert_eq!(kit.warmth.heat(print(b"root").key()), Heat::Warming);
    drop((from_child, from_root));
    assert_eq!(kit.warmth.heat(print(b"child").key()), Heat::Cold);
}

async fn logged_plugins(built: Built, logs: &CountedLogs) -> Vec<String> {
    started_plugins(built.agent, ROOT, logs).await
}

async fn session_started(mut agent: Agent, path: &str, logs: &CountedLogs) -> LogEvent {
    use crate::session::SessionStore;
    let (tx, mut rx) = tokio::sync::mpsc::channel(64);
    agent.start_log(&tx).await;
    drop(tx);
    while rx.recv().await.is_some() {}
    let started: Vec<LogEvent> = logs
        .store
        .read(path)
        .unwrap()
        .into_iter()
        .map(|record| record.event)
        .filter(|event| matches!(event, LogEvent::SessionStarted { .. }))
        .collect();
    let [started] = started.as_slice() else {
        panic!("{started:?}");
    };
    started.clone()
}

async fn started_plugins(agent: Agent, path: &str, logs: &CountedLogs) -> Vec<String> {
    match session_started(agent, path, logs).await {
        LogEvent::SessionStarted { plugins, .. } => plugins.into_iter().map(|p| p.id).collect(),
        other => panic!("{other:?}"),
    }
}

#[cfg(unix)]
async fn logged_prefix(
    agent: Agent,
    path: &str,
    logs: &CountedLogs,
) -> (
    String,
    Vec<serde_json::Value>,
    Option<wisp_protocol::log::ToolContext>,
) {
    let canonical = agent.toolbox().canonical_json();
    match session_started(agent, path, logs).await {
        LogEvent::SessionStarted { whole, .. } => (
            canonical,
            whole.tool_definitions,
            whole.tool_context.map(|c| *c),
        ),
        other => panic!("{other:?}"),
    }
}

#[cfg(unix)]
fn names_in(definitions: &[serde_json::Value]) -> Vec<&str> {
    definitions
        .iter()
        .filter_map(|d| d["name"].as_str())
        .collect()
}

#[cfg(unix)]
#[tokio::test]
async fn the_session_start_logs_its_client_definitions_and_the_context_they_had() {
    let logs = Arc::new(CountedLogs::default());
    let mut on = kit(
        Arc::new(connected(ROOT)),
        tools(&["bash", "show_diff", "view_file", "web_search"]),
        logs.clone(),
    );
    on.user_input = true;
    let built = on.build(spec(ROOT)).await.unwrap();
    assert!(built.agent.toolbox().web_search());
    let (canonical, definitions, context) = logged_prefix(built.agent, ROOT, &logs).await;
    assert_eq!(serde_json::to_string(&definitions).unwrap(), canonical);
    assert_eq!(
        names_in(&definitions),
        ["bash", "process", "show_diff", "view_file"]
    );
    let context = context.unwrap();
    assert_eq!(context.platform, crate::plugin::PLATFORM);
    assert!(
        context.capabilities.contains(&"shell".to_owned()),
        "{context:?}"
    );

    let logs = Arc::new(CountedLogs::default());
    let off = kit(
        Arc::new(connected(ROOT)),
        tools(&["show_diff", "view_file"]),
        logs.clone(),
    );
    let built = off.build(spec(ROOT)).await.unwrap();
    let (canonical, definitions, context) = logged_prefix(built.agent, ROOT, &logs).await;
    assert_eq!(serde_json::to_string(&definitions).unwrap(), canonical);
    assert_eq!(names_in(&definitions), ["show_diff", "view_file"]);
    let context = context.unwrap();
    assert!(
        !context.capabilities.contains(&"shell".to_owned()),
        "{context:?}"
    );
}

#[cfg(unix)]
#[tokio::test]
async fn a_childs_session_start_logs_its_own_narrowed_definitions_and_context() {
    let logs = Arc::new(CountedLogs::default());
    let mut kit = kit(
        Arc::new(connected(ROOT)),
        tools(&["bash", "show_diff", "view_file"]),
        logs.clone(),
    );
    kit.user_input = true;
    let root = kit.build(spec(ROOT)).await.unwrap().agent;
    let (_, root_definitions, root_context) = logged_prefix(root, ROOT, &logs).await;
    assert!(
        root_context
            .unwrap()
            .capabilities
            .contains(&"shell".to_owned())
    );
    let child = kit
        .build_child(child_with("/review", &["view_file"], std::env::temp_dir()))
        .await
        .unwrap();
    let (canonical, definitions, context) = logged_prefix(child, "/review", &logs).await;
    assert_eq!(serde_json::to_string(&definitions).unwrap(), canonical);
    assert_eq!(names_in(&definitions), ["view_file"]);
    assert_ne!(definitions, root_definitions);
    let context = context.unwrap();
    assert!(
        !context.capabilities.contains(&"shell".to_owned()),
        "{context:?}"
    );
}

#[tokio::test]
async fn a_chat_only_session_still_logs_every_plugin_that_started() {
    let logs = Arc::new(CountedLogs::default());
    let chat = kit(Arc::new(connected(ROOT)), tools(&[]), logs.clone());
    let built = chat.build(spec(ROOT)).await.unwrap();
    assert!(built.plugin_problems.is_empty());
    let expected = crate::tool::testkit::bundled_ids();
    assert_eq!(logged_plugins(built, &logs).await, expected);

    let mut missing = kit(Arc::new(connected(ROOT)), tools(&[]), logs.clone());
    missing.workspace = Err("could not find the working directory: gone".into());
    let logs = Arc::new(CountedLogs::default());
    missing.logs = logs.clone();
    let built = missing.build(spec(ROOT)).await.unwrap();
    assert_eq!(logged_plugins(built, &logs).await, expected);
}

#[cfg(unix)]
#[tokio::test]
async fn a_plugin_left_out_at_start_is_a_problem_and_is_not_logged() {
    let ghosted = wisp_plugin_show_diff::bundled();
    let instantiate = ghosted.instantiate;
    let make: crate::plugin::Make = Arc::new(move |host| instantiate(host));
    let viewer = wisp_plugin_view_file::bundled();
    let view: crate::plugin::Make = Arc::new(move |host| (viewer.instantiate)(host));
    let sources = vec![
        (
            crate::plugin::testing::manifest(
                "show-diff",
                &names(&["show_code", "show_diff", "ghost"]),
                &[],
            ),
            make,
        ),
        (viewer.manifest.to_owned(), view),
    ];
    let (registry, problems) =
        crate::plugin::Registry::from_sources(sources, &crate::plugin::PluginConfig::default());
    assert!(problems.is_empty(), "{problems:?}");
    let tools = crate::plugin::Tools::new(
        Arc::new(registry),
        ["show_diff", "view_file"]
            .iter()
            .map(|s| (*s).to_owned())
            .collect(),
    );
    let logs = Arc::new(CountedLogs::default());
    let ghostly = kit(Arc::new(connected(ROOT)), tools, logs.clone());
    let built = ghostly.build(spec(ROOT)).await.unwrap();
    assert_eq!(
        built.plugin_problems,
        [crate::plugin::Problem {
            plugin: "show-diff".into(),
            message: "it doesn't describe its tool `ghost`; the plugin is left out".into(),
        }]
    );
    assert_eq!(
        built
            .plugin_problems
            .iter()
            .map(ToString::to_string)
            .collect::<Vec<_>>(),
        ["plugin show-diff: it doesn't describe its tool `ghost`; the plugin is left out"]
    );
    assert!(
        !built
            .agent
            .toolbox()
            .names()
            .contains(&"show_diff".to_owned())
    );
    assert_eq!(logged_plugins(built, &logs).await, ["view-file"]);
}

#[tokio::test]
async fn a_prestarted_toolbox_is_used_once_then_the_registry_starts_again() {
    let tools = tools(&["show_diff"]);
    let started = tools.start(ROOT, Arc::new(KernelHost::new())).await;
    let first = Arc::as_ptr(&started.toolbox.hosts()[0]);
    let prestarted = crate::plugin::Prestarted::new(started, tools);
    let again = prestarted.start(ROOT, Arc::new(KernelHost::new())).await;
    assert_eq!(Arc::as_ptr(&again.toolbox.hosts()[0]), first);
    let fresh = prestarted.start(ROOT, Arc::new(KernelHost::new())).await;
    assert_ne!(Arc::as_ptr(&fresh.toolbox.hosts()[0]), first);
    assert_eq!(fresh.toolbox.names(), ["show_diff"]);
}

fn names(list: &[&str]) -> Vec<String> {
    list.iter().map(|s| (*s).to_owned()).collect()
}

fn child_with(path: &str, tools: &[&str], cwd: PathBuf) -> ChildSpec {
    ChildSpec {
        path: AgentPath::new(path),
        cwd,
        tools: names(tools),
        ..child_spec("env")
    }
}

fn scratch(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("wisp-kit-{}-{name}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

#[tokio::test]
async fn a_child_starts_exactly_its_own_tools_from_the_catalog() {
    let mut kit = kit(
        Arc::new(connected(ROOT)),
        tools(crate::tool::testkit::ALL),
        Arc::new(CountedLogs::default()),
    );
    kit.user_input = true;
    let root = kit.build(spec(ROOT)).await.unwrap().agent;
    assert!(
        root.toolbox().names().contains(&"show_diff".to_owned()),
        "{:?}",
        root.toolbox().names()
    );
    let child = kit
        .build_child(child_with("/review", &["show_diff"], std::env::temp_dir()))
        .await
        .unwrap();
    assert_eq!(child.toolbox().names(), ["show_diff"]);
    assert!(!child.toolbox().web_search());
}

#[tokio::test]
async fn a_childs_web_search_follows_its_tool_names_like_any_tool() {
    let mut on: Vec<&str> = crate::tool::testkit::ALL.to_vec();
    on.push("web_search");
    let mut kit = kit(
        Arc::new(connected(ROOT)),
        tools(&on),
        Arc::new(CountedLogs::default()),
    );
    kit.user_input = true;
    let root = kit.build(spec(ROOT)).await.unwrap().agent;
    assert!(root.toolbox().web_search());
    assert!(root.caller().tools.contains(&"web_search".to_owned()));
    let kept = kit
        .build_child(child_with(
            "/kept",
            &["show_diff", "web_search"],
            std::env::temp_dir(),
        ))
        .await
        .unwrap();
    assert!(kept.toolbox().web_search());
    assert_eq!(kept.toolbox().names(), ["show_diff"]);
    let off = kit
        .build_child(child_with("/off", &["show_diff"], std::env::temp_dir()))
        .await
        .unwrap();
    assert!(!off.toolbox().web_search());
}

#[derive(Default)]
struct Counted {
    hosts: Mutex<Vec<Arc<dyn wisp_plugin_api::host::Host>>>,
}

impl crate::plugin::testing::TestTool for Counted {
    fn name(&self) -> String {
        "counted".into()
    }

    fn attach(&self, host: Arc<dyn wisp_plugin_api::host::Host>) {
        self.hosts.lock().unwrap().push(host);
    }

    fn prepare(&self, _input: &str) -> Result<Box<dyn wisp_plugin_api::tools::Prepared>, String> {
        Err("never run".into())
    }

    fn run<'a>(
        &'a self,
        _call: Box<dyn wisp_plugin_api::tools::Prepared>,
        _ctx: &'a dyn wisp_plugin_api::host::CallContext,
    ) -> futures::future::BoxFuture<'a, wisp_plugin_api::types::ToolRun> {
        futures::future::ready(wisp_plugin_api::types::ToolRun::completed()).boxed()
    }
}

async fn counted_kit(counted: &Arc<Counted>) -> (Kit, Arc<KernelHost>) {
    let tool: Arc<dyn crate::plugin::testing::TestTool> = counted.clone();
    let (registry, problems) = crate::plugin::Registry::from_sources(
        vec![crate::plugin::testing::source(tool)],
        &crate::plugin::PluginConfig::default(),
    );
    assert!(problems.is_empty(), "{problems:?}");
    let tools = crate::plugin::Tools::new(Arc::new(registry), ["counted".to_owned()].into());
    let started = tools.start(ROOT, Arc::new(KernelHost::new())).await;
    let prestarted = started.toolbox.hosts()[0].clone();
    let mut kit = kit(
        Arc::new(connected(ROOT)),
        tools.clone(),
        Arc::new(CountedLogs::default()),
    );
    kit.tools = Arc::new(crate::plugin::Prestarted::new(started, tools));
    (kit, prestarted)
}

#[tokio::test]
async fn each_child_starts_its_own_instance_and_the_root_keeps_the_prestarted_one() {
    let counted = Arc::new(Counted::default());
    let (mut kit, prestarted) = counted_kit(&counted).await;
    kit.mcp = crate::mcp::Service::new(
        [(
            "shared".into(),
            crate::config::McpServer {
                keys: Vec::new(),
                command: "/dev/null/not-a-program".into(),
                args: vec![],
            },
        )]
        .into(),
        std::env::temp_dir(),
        None,
    );
    let attached = || counted.hosts.lock().unwrap().len();
    assert_eq!(attached(), 1);
    let root = kit.build(spec(ROOT)).await.unwrap().agent;
    assert_eq!(attached(), 1);
    assert!(Arc::ptr_eq(&root.toolbox().hosts()[0], &prestarted));
    let one = kit
        .build_child(child_with("/one", &["counted"], std::env::temp_dir()))
        .await
        .unwrap();
    assert_eq!(attached(), 2);
    let two = kit
        .build_child(child_with("/two", &["counted"], std::env::temp_dir()))
        .await
        .unwrap();
    assert_eq!(attached(), 3);
    let hosts = [&root, &one, &two].map(|a| a.toolbox().hosts());
    assert!(hosts.iter().all(|h| h.len() == 1));
    assert!(!Arc::ptr_eq(&hosts[0][0], &hosts[1][0]));
    assert!(!Arc::ptr_eq(&hosts[0][0], &hosts[2][0]));
    assert!(!Arc::ptr_eq(&hosts[1][0], &hosts[2][0]));
    assert!(
        hosts
            .iter()
            .all(|host| host[0].mcp().rows()[0].state == crate::mcp::State::Stopped)
    );
    let error = hosts[1][0].mcp().connect("shared").await.unwrap_err();
    assert!(
        hosts
            .iter()
            .all(|host| host[0].mcp().rows()[0].state == crate::mcp::State::Failed(error.clone()))
    );
    assert_eq!(kit.mcp.rows()[0].state, crate::mcp::State::Failed(error));

    let counted = Arc::new(Counted::default());
    let (kit, prestarted) = counted_kit(&counted).await;
    let early = kit
        .build_child(child_with("/early", &["counted"], std::env::temp_dir()))
        .await
        .unwrap();
    assert!(!Arc::ptr_eq(&early.toolbox().hosts()[0], &prestarted));
    let root = kit.build(spec(ROOT)).await.unwrap().agent;
    assert!(Arc::ptr_eq(&root.toolbox().hosts()[0], &prestarted));
    assert_eq!(counted.hosts.lock().unwrap().len(), 2);
}

fn logged_child() -> Vec<crate::session::Line> {
    let mut record = protocol_fixture("log_session_started_child");
    record.seq = 0;
    vec![crate::session::Line::Record(Box::new(record))]
}

#[tokio::test]
async fn each_resumed_child_gets_its_own_plugin_instances_and_the_kits_cold_warmth_table() {
    let counted = Arc::new(Counted::default());
    let (kit, prestarted) = counted_kit(&counted).await;
    let mut resumed = Vec::new();
    for path in ["/one", "/two"] {
        let store = MemoryStore::default();
        let lines = logged_child();
        let provider = provider("https://api.anthropic.com");
        let next = u64::try_from(lines.len()).unwrap();
        let mut recovered =
            crate::resume::recover(lines, &store, &|_| Some(provider.clone())).unwrap();
        let (logged, rebuilt) = recovered.take_checked();
        let logged = logged.unwrap();
        let resume = Resume {
            log: SessionLog::continuing(Box::new(store.clone()), path, next),
            recovered: &recovered,
            truncated_bytes: 0,
            logged: Some(&logged),
            rebuilt,
        };
        let (agent, missing) = kit
            .resume_child(child_with(path, &["counted"], std::env::temp_dir()), resume)
            .await
            .unwrap();
        assert!(missing.is_empty(), "{missing:?}");
        resumed.push(agent);
    }
    let root = kit.build(spec(ROOT)).await.unwrap().agent;
    assert_eq!(counted.hosts.lock().unwrap().len(), 3);
    assert!(Arc::ptr_eq(&root.toolbox().hosts()[0], &prestarted));
    let [one, two] = [&resumed[0], &resumed[1]].map(|a| a.toolbox().hosts()[0].clone());
    assert!(!Arc::ptr_eq(&one, &prestarted));
    assert!(!Arc::ptr_eq(&two, &prestarted));
    assert!(!Arc::ptr_eq(&one, &two));
    let print = crate::warmth::Fingerprint::new(
        "env",
        &crate::provider::PrefixBytes {
            model: b"m".to_vec(),
            effort: b"e".to_vec(),
            thinking: b"t".to_vec(),
            tools: b"counted".to_vec(),
            system: b"s".to_vec(),
        },
    );
    assert_eq!(kit.warmth.heat(print.key()), crate::warmth::Heat::Cold);
    let sending = resumed[0].warmth_table().sent(print.clone());
    assert_eq!(kit.warmth.heat(print.key()), crate::warmth::Heat::Warming);
    drop(sending);
}

#[tokio::test]
async fn a_childs_host_never_moves_the_roots_workspace() {
    let counted = Arc::new(Counted::default());
    let (kit, _) = counted_kit(&counted).await;
    let root = kit.build(spec(ROOT)).await.unwrap().agent;
    let elsewhere = scratch("elsewhere");
    let child = kit
        .build_child(child_with("/away", &["counted"], elsewhere.clone()))
        .await
        .unwrap();
    let hosts = counted.hosts.lock().unwrap().clone();
    let [root_host, child_host] = hosts.as_slice() else {
        panic!("{} hosts", hosts.len());
    };
    assert_eq!(root_host.root(), workspace().root.display().to_string());
    assert_eq!(child_host.root(), elsewhere.display().to_string());
    drop((root, child));
    std::fs::remove_dir_all(elsewhere).unwrap();
}

#[tokio::test]
async fn a_child_logs_every_plugin_started_for_it_while_its_tools_are_narrowed() {
    let logs = Arc::new(CountedLogs::default());
    let kit = kit(
        Arc::new(connected(ROOT)),
        tools(&["show_diff"]),
        logs.clone(),
    );
    let built = kit.build(spec(ROOT)).await.unwrap();
    assert_eq!(built.agent.toolbox().names(), ["show_diff"]);
    let root = logged_plugins(built, &logs).await;
    let child = kit
        .build_child(child_with("/review", &["report"], std::env::temp_dir()))
        .await
        .unwrap();
    assert_eq!(child.toolbox().names(), ["report"]);
    assert_eq!(started_plugins(child, "/review", &logs).await, root);
    assert!(root.contains(&"show-diff".to_owned()), "{root:?}");
}

#[cfg(unix)]
#[tokio::test]
async fn a_plugin_left_out_for_a_child_is_missing_from_its_tools_and_its_log() {
    let ghosted = wisp_plugin_show_diff::bundled();
    let instantiate = ghosted.instantiate;
    let make: crate::plugin::Make = Arc::new(move |host| instantiate(host));
    let viewer = wisp_plugin_view_file::bundled();
    let view: crate::plugin::Make = Arc::new(move |host| (viewer.instantiate)(host));
    let sources = vec![
        (
            crate::plugin::testing::manifest(
                "show-diff",
                &names(&["show_code", "show_diff", "ghost"]),
                &[],
            ),
            make,
        ),
        (viewer.manifest.to_owned(), view),
    ];
    let (registry, problems) =
        crate::plugin::Registry::from_sources(sources, &crate::plugin::PluginConfig::default());
    assert!(problems.is_empty(), "{problems:?}");
    let tools = crate::plugin::Tools::new(Arc::new(registry), std::collections::BTreeSet::new());
    let logs = Arc::new(CountedLogs::default());
    let kit = kit(Arc::new(connected(ROOT)), tools, logs.clone());
    let child = kit
        .build_child(child_with(
            "/review",
            &["show_diff", "view_file"],
            std::env::temp_dir(),
        ))
        .await
        .unwrap();
    assert_eq!(child.toolbox().names(), ["view_file"]);
    assert_eq!(
        started_plugins(child, "/review", &logs).await,
        ["view-file"]
    );
}

#[cfg(unix)]
#[tokio::test]
async fn children_with_the_same_tools_share_their_prefix_with_a_root_started_with_them() {
    let mut on = vec!["show_diff", "view_file"];
    if cfg!(unix) {
        on.push("bash");
    }
    let mut root_kit = kit(
        Arc::new(connected(ROOT)),
        tools(&on),
        Arc::new(CountedLogs::default()),
    );
    root_kit.user_input = true;
    let root = root_kit.build(spec(ROOT)).await.unwrap().agent;
    let kit = kit(
        Arc::new(connected(ROOT)),
        tools(crate::tool::testkit::ALL),
        Arc::new(CountedLogs::default()),
    );
    let mut child_tools = on.clone();
    if cfg!(unix) {
        child_tools.push("process");
    }
    let elsewhere = scratch("prefix");
    let one = kit
        .build_child(child_with("/one", &child_tools, std::env::temp_dir()))
        .await
        .unwrap();
    let two = kit
        .build_child(child_with("/a/two", &child_tools, elsewhere.clone()))
        .await
        .unwrap();
    let print = |a: &Agent| (a.toolbox().canonical_json(), a.session().system.clone());
    assert_eq!(print(&one), print(&two));
    assert_eq!(print(&one), print(&root));
    assert!(print(&one).0.contains("\"view_file\""));
    drop((one, two));
    std::fs::remove_dir_all(elsewhere).unwrap();
}

fn max_age_flag(value: &str) -> StartLayers {
    let target = wisp_protocol::settings::parse_target(names::TOOL_OUTPUT_MAX_AGE_DAYS).unwrap();
    let value = target.spec().parse(value).unwrap();
    StartLayers {
        flags: vec![crate::settings::Given {
            target,
            value,
            origin: "--set".into(),
        }],
        ..StartLayers::default()
    }
}

#[test]
fn tool_outputs_are_kept_seven_days_unless_the_setting_says_otherwise() {
    assert_eq!(output_max_age(&StartLayers::default()), Some(DAY * 7));
    assert_eq!(output_max_age(&max_age_flag("30")), Some(DAY * 30));
    assert_eq!(output_max_age(&max_age_flag("none")), None);
    let config = crate::endpoints::ConfigFile::parse(
        Path::new("/h/config.toml"),
        crate::endpoints::PathShown::Default,
        "[tool_output]\nmax_age_days = 2\n",
    )
    .unwrap();
    let saved = StartLayers {
        saved: config.settings,
        ..StartLayers::default()
    };
    assert_eq!(output_max_age(&saved), Some(DAY * 2));
}

fn old_output_folder(temp: &Path, session: &str) -> PathBuf {
    let dir = crate::call::OutputDir::new(temp, session);
    dir.ensure().unwrap();
    let month_ago = SystemTime::now() - DAY * 30;
    std::fs::File::open(dir.path())
        .unwrap()
        .set_modified(month_ago)
        .unwrap();
    dir.path().to_owned()
}

fn logged_in(root: &Path, session: &str) {
    let logs = root.join("sessions");
    std::fs::create_dir_all(&logs).unwrap();
    std::fs::write(logs.join(format!("{session}.jsonl")), "").unwrap();
}

fn sweeping_kit(root: &Path, layers: StartLayers) -> Kit {
    let mut kit = kit(
        Arc::new(connected(ROOT)),
        tools(&[]),
        Arc::new(JsonlLogs {
            on: false,
            dir: Some(root.join("sessions")),
        }),
    );
    kit.workspace = Ok(Workspace {
        root: root.to_owned(),
        home: None,
        temp: root.join("tmp"),
    });
    kit.start_layers = layers;
    kit
}

#[test]
fn with_the_setting_none_no_folder_is_even_looked_at() {
    let root = tempfile::tempdir().unwrap();
    std::fs::create_dir(root.path().join("tmp")).unwrap();
    let old = old_output_folder(
        &root.path().join("tmp"),
        "0190a1b2-c3d4-7e5f-8a9b-0c1d2e3f4a5b",
    );
    let kit = sweeping_kit(root.path(), max_age_flag("none"));
    assert!(
        kit.remove_old_outputs("0190a1b2-c3d4-7e5f-8a9b-0c1d2e3f4a5c")
            .is_none()
    );
    assert!(old.exists());
}

#[test]
fn the_first_root_build_removes_old_folders_once_per_process_but_never_its_own() {
    let root = tempfile::tempdir().unwrap();
    let temp = root.path().join("tmp");
    std::fs::create_dir(&temp).unwrap();
    let running = "0190a1b2-c3d4-7e5f-8a9b-0c1d2e3f4a5c";
    let other = old_output_folder(&temp, "0190a1b2-c3d4-7e5f-8a9b-0c1d2e3f4a5b");
    let own = old_output_folder(&temp, running);
    let elsewhere = old_output_folder(&temp, "0190a1b2-c3d4-7e5f-8a9b-0c1d2e3f4a5e");
    logged_in(root.path(), "0190a1b2-c3d4-7e5f-8a9b-0c1d2e3f4a5b");
    logged_in(root.path(), running);
    let kit = sweeping_kit(root.path(), StartLayers::default());
    kit.remove_old_outputs(running).unwrap().join().unwrap();
    assert!(!other.exists());
    assert!(own.exists());
    assert!(elsewhere.exists());
    let later = old_output_folder(&temp, "0190a1b2-c3d4-7e5f-8a9b-0c1d2e3f4a5d");
    assert!(kit.remove_old_outputs(running).is_none());
    assert!(later.exists());
}

#[test]
fn a_kit_without_a_sessions_dir_removes_nothing() {
    let mut kit = kit(
        Arc::new(connected(ROOT)),
        tools(&[]),
        Arc::new(CountedLogs::default()),
    );
    kit.start_layers = StartLayers::default();
    assert!(
        kit.remove_old_outputs("0190a1b2-c3d4-7e5f-8a9b-0c1d2e3f4a5c")
            .is_none()
    );
}

struct UnopenedLog(PathBuf);

impl OpenLog for UnopenedLog {
    fn open(&self, _path: &str) -> Result<Option<OpenedLog>, String> {
        Err("the disk is full".into())
    }

    fn sessions_dir(&self) -> Option<PathBuf> {
        Some(self.0.clone())
    }
}

#[tokio::test]
async fn a_session_whose_own_log_failed_to_open_sweeps_nothing() {
    let root = tempfile::tempdir().unwrap();
    let temp = root.path().join("tmp");
    std::fs::create_dir(&temp).unwrap();
    let other = "0190a1b2-c3d4-7e5f-8a9b-0c1d2e3f4a5b";
    let old = old_output_folder(&temp, other);
    logged_in(root.path(), other);
    let mut kit = sweeping_kit(root.path(), StartLayers::default());
    kit.logs = Arc::new(UnopenedLog(root.path().join("sessions")));
    let built = kit.build(spec(ROOT)).await.unwrap();
    assert!(matches!(built.log, LogOutcome::Failed(_)));
    assert!(old.exists());
    let swept = kit.remove_old_outputs(built.agent.session_id());
    assert!(swept.is_some(), "the build had already swept");
    swept.unwrap().join().unwrap();
    assert!(!old.exists());
}

#[tokio::test]
async fn a_runner_only_toolbox_is_kept_for_the_root_and_child() {
    let (registry, problems) = crate::plugin::Registry::new(
        wisp_plugins::bundled()
            .into_iter()
            .filter(|bundle| bundle.manifest.lines().next() == Some("id = \"web-search\""))
            .collect(),
        &crate::plugin::PluginConfig::default(),
    );
    assert!(problems.is_empty(), "{problems:?}");
    let selected = crate::plugin::Tools::new(
        Arc::new(registry),
        [crate::search::WEB_SEARCH.to_owned()].into(),
    );
    let kit = kit(
        Arc::new(connected(ROOT)),
        selected,
        Arc::new(CountedLogs::default()),
    );
    let root = kit.build(spec(ROOT)).await.unwrap().agent;
    let child = kit
        .build_child(child_with(
            "/review",
            &[crate::search::WEB_SEARCH],
            std::env::temp_dir(),
        ))
        .await
        .unwrap();
    for agent in [&root, &child] {
        assert!(agent.toolbox().is_empty());
        assert!(agent.toolbox().held_names().is_empty());
        assert!(agent.toolbox().runner(crate::search::WEB_SEARCH).is_some());
    }
}
