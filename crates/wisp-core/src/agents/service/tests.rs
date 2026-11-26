use std::collections::BTreeMap;
use std::fs;
use std::sync::Mutex as StdMutex;

use futures::FutureExt;
use futures::future::BoxFuture;
use jiff::Timestamp;
use serde_json::json;
use tokio::sync::Semaphore;
use wisp_plugin_testkit::hooks::{self as kit, Calls};
use wisp_protocol::agents::{RENAME_AGENT, SEND_MESSAGE, STOP_AGENT};
use wisp_protocol::log::{CredentialSource, LogEvent};
use wisp_protocol::{
    ContentBlock, ErrorKind, FailurePhase, IdleOutcome, InstructionsSource, ReportOutcome,
    RequestId, RetryClass, Role, SessionView, StopReason, TurnOutcome, Usage,
};

use super::*;
use crate::agent::LogContext;
use crate::agent::testing::{
    Caps, Fake, Knobs, Moment, PREFIX_TOKENS, Prefix, Prints, Script, Sent, Step, Wait, Wire,
    classified, ended, mid_stream, opaque, started, text, tool_use,
};
use crate::clock::{Clock, ShiftedClock};
use crate::context::ModelSettings;
use crate::kit::{Built, Choices, Connected, OpenLog, OpenedLog, Spec, Start};
use crate::provider::anthropic::capabilities;
use crate::provider::{Provider, ProviderError, ProviderEvent};
use crate::session::{MemoryStore, Redactions, SessionLog, SessionStore};
use crate::settings::host::StartLayers;
use crate::test_guard::{
    ScratchDir, catalog_names, hold, log_context, pick, until, until_event, wait_for, within,
};

mod chats;
mod endpoints;
mod follow;
mod guarantee;
mod hooks;
mod idle_wake;
mod instructions;
mod keep_warm;
mod limits;
mod messages;
mod openai;
mod plugin;
mod prefix;
mod refusals;
mod rename;
mod restart;
mod resume;
mod resumed_tools;
mod routing;
mod self_check;
mod sign_in;
mod spawn;
mod states;
mod subscription_auth;
mod switches;
mod watch;

const MODEL: &str = wisp_protocol::settings::DEFAULT_MODEL;

impl Sent {
    fn prefix(&self) -> (Option<String>, Option<String>, String, String, Effort) {
        (
            self.session.system.clone(),
            self.session.instructions.clone(),
            crate::tool::canonical_specs(&self.tools),
            self.session.model.clone(),
            self.session.effort,
        )
    }

    fn tool_names(&self) -> Vec<String> {
        crate::tool::tool_names(&self.tools, &[])
    }

    fn first_user_texts(&self) -> Vec<String> {
        self.messages
            .iter()
            .find(|m| m.role == Role::User)
            .map(|m| texts(&m.content))
            .unwrap_or_default()
    }

    fn last_user_texts(&self) -> Vec<String> {
        self.messages
            .iter()
            .rev()
            .find(|m| m.role == Role::User)
            .map(|m| texts(&m.content))
            .unwrap_or_default()
    }

    fn all_text(&self) -> String {
        self.messages
            .iter()
            .flat_map(|m| texts(&m.content))
            .collect::<Vec<_>>()
            .join("\n")
    }

    fn server_names(&self) -> Vec<String> {
        self.server_tools.iter().map(|t| t.name.clone()).collect()
    }
}

fn todos_call(id: &str, done: &str, doing: &str) -> B {
    call(
        id,
        WRITE_TODOS,
        json!({"todos": [
            {"text": done, "status": "completed"},
            {"text": doing, "status": "in_progress"}
        ]}),
    )
}

fn grows_from(next: &Sent, earlier: &Sent, what: &str) {
    assert_eq!(next.prefix(), earlier.prefix(), "{what}");
    assert!(next.messages.starts_with(&earlier.messages), "{what}");
}

fn ended_line(path: &str, below: u32) -> String {
    wisp_tool_text::agents::stopped(&wisp_plugin_api::host::Stopped {
        path: path.to_owned(),
        below,
        ended: true,
    })
}

fn one_running() -> Setup {
    Setup {
        limits: Limits {
            max_running: Some(1),
            ..Limits::default()
        },
        ..Setup::default()
    }
}

fn result_of(request: &Sent, id: &str) -> (bool, String) {
    request
        .messages
        .iter()
        .flat_map(|m| &m.content)
        .find_map(|b| match b {
            ContentBlock::ToolResult {
                tool_use_id,
                content,
                is_error,
            } if tool_use_id == id => Some((*is_error, texts(content).join("\n"))),
            _ => None,
        })
        .unwrap_or_else(|| panic!("no result for {id}"))
}

fn texts(content: &[ContentBlock]) -> Vec<String> {
    content
        .iter()
        .flat_map(|b| match b {
            ContentBlock::Text { text } => vec![text.clone()],
            ContentBlock::ToolResult { content, .. } => texts(content),
            _ => Vec::new(),
        })
        .collect()
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
enum Table {
    #[default]
    Anthropic,
    Only(&'static [&'static str]),
    OpenAi,
    Gemini,
}

#[derive(Clone, Copy, Debug)]
struct FakeEndpoint {
    name: &'static str,
    table: Table,
    priced: bool,
    billing: Option<wisp_protocol::Billing>,
    list: Option<fn() -> crate::models::list::ModelList>,
    voice: bool,
}

fn endpoint(name: &'static str) -> FakeEndpoint {
    FakeEndpoint {
        name,
        table: Table::Anthropic,
        priced: true,
        billing: None,
        list: None,
        voice: false,
    }
}

fn openai(name: &'static str, billing: wisp_protocol::Billing) -> FakeEndpoint {
    FakeEndpoint {
        name,
        table: Table::OpenAi,
        priced: billing.is_metered(),
        billing: Some(billing),
        list: None,
        voice: false,
    }
}

const GPT: &str = "gpt-6.1-sol";

const ENV: &str = "env";

fn on(endpoint: &str, path: &str) -> String {
    if endpoint == ENV {
        path.to_owned()
    } else {
        format!("{endpoint}:{path}")
    }
}

fn scripted(steps: impl IntoIterator<Item = Step>) -> Arc<Fake> {
    let knobs = Knobs {
        media: true,
        hang_when_out: true,
        documented: Some(false),
        billing: Some(wisp_protocol::Billing::default()),
        ..Knobs::default()
    };
    let fake = Fake::with(knobs, steps);
    table(&fake, Table::Anthropic);
    fake
}

fn table(fake: &Fake, table: Table) {
    fake.knobs(|k| {
        k.caps = Some(match table {
            Table::Anthropic => Caps::Fn(capabilities::row),
            Table::Only(models) => Caps::Only(models),
            Table::OpenAi => Caps::Fn(crate::provider::openai::model_capabilities),
            Table::Gemini => Caps::Fn(crate::provider::gemini::model_capabilities),
        });
        k.keeps_warm = Some(table != Table::OpenAi);
        k.kind = Some(match table {
            Table::OpenAi => crate::endpoints::ProviderKind::OpenAi,
            Table::Gemini => crate::endpoints::ProviderKind::Gemini,
            _ => crate::endpoints::ProviderKind::Anthropic,
        });
    });
}

#[derive(Default)]
struct Providers {
    each: StdMutex<BTreeMap<String, Arc<Fake>>>,
    prints: Option<Prints>,
    digests: bool,
    documented: bool,
    wire: Arc<Wire>,
    tables: BTreeMap<String, Table>,
    billings: BTreeMap<String, wisp_protocol::Billing>,
}

fn endpoint_of(path: &str) -> &str {
    path.split_once(':').map_or(ENV, |(endpoint, _)| endpoint)
}

impl Providers {
    fn get(&self, path: &str) -> Arc<Fake> {
        let endpoint = endpoint_of(path);
        let rows = self.tables.get(endpoint).copied().unwrap_or_default();
        let billing = self.billings.get(endpoint).copied().unwrap_or_default();
        self.each
            .lock()
            .unwrap()
            .entry(path.to_owned())
            .or_insert_with(|| {
                let fake = scripted([]);
                fake.knobs(|k| {
                    k.prefix_bytes = self.prints;
                    if self.digests {
                        k.prefix = Prefix::Sent;
                    }
                    k.wire = Some((self.wire.clone(), path.to_owned()));
                    k.documented = Some(self.documented);
                    k.billing = Some(billing);
                });
                table(&fake, rows);
                fake
            })
            .clone()
    }

    fn on_endpoint(&self, endpoint: &FakeEndpoint, path: &str) -> Arc<Fake> {
        let scripted = self.get(&on(endpoint.name, path));
        table(&scripted, endpoint.table);
        scripted.knobs(|k| k.list = endpoint.list.map(|list| Arc::new(list())));
        if endpoint.voice {
            let route = Arc::new(crate::voice::testing::Route::default());
            scripted.knobs(|k| k.voice = Some(route));
        }
        if let Some(billing) = endpoint.billing {
            scripted.knobs(|k| k.billing = Some(billing));
        }
        scripted
    }

    fn timeline(&self) -> Vec<Moment> {
        self.wire.timeline.lock().unwrap().clone()
    }

    fn script(&self, path: &str, steps: Vec<Step>) {
        let provider = self.get(path);
        let billing = steps
            .iter()
            .flat_map(|step| &step.script)
            .filter_map(|event| match event {
                Ok(
                    ProviderEvent::MessageStart { usage, .. }
                    | ProviderEvent::MessageDelta { usage, .. },
                ) => Some(usage.billing),
                _ => None,
            })
            .reduce(wisp_protocol::Billing::combined)
            .unwrap_or_default();
        if !self.billings.contains_key(endpoint_of(path)) {
            provider.knobs(|k| k.billing = Some(billing));
        }
        provider.push(steps);
    }

    fn requests(&self, path: &str) -> Vec<Sent> {
        self.get(path).sent()
    }
}

const LISTED_ROOT: &str = "0009eeee-0000-7000-8000-000000000000";

fn session_name(path: &str) -> String {
    if path == "/" {
        "s-root".to_owned()
    } else {
        format!("s{}", path.replace('/', "-"))
    }
}

struct Logs {
    store: MemoryStore,
    busy: Vec<String>,
    sessions: Option<PathBuf>,
    kept: PathBuf,
}

impl OpenLog for Logs {
    fn reopen(
        &self,
        session_id: &str,
        _lock: crate::session::ResumeLock,
    ) -> Result<crate::kit::ReopenedLog, String> {
        if self.busy.iter().any(|b| b == session_id) {
            return Err(format!("session {session_id} is open in another Wisp"));
        }
        let records = self.store.read(session_id).map_err(|e| e.to_string())?;
        let lines = crate::session::reread(&records).map_err(|e| e.to_string())?;
        let next = u64::try_from(lines.len()).unwrap();
        Ok(crate::kit::ReopenedLog {
            log: SessionLog::continuing(Box::new(self.store.clone()), session_id, next),
            lines,
            truncated_bytes: 0,
            unlocked: None,
        })
    }

    fn read(&self, session_id: &str) -> Result<Vec<wisp_protocol::log::LogRecord>, String> {
        self.store.read(session_id).map_err(|e| e.to_string())
    }

    fn open(&self, path: &str) -> Result<Option<OpenedLog>, String> {
        if path.ends_with("/unloggable") {
            return Err(
                "could not open /home/someone/.local/share/wisp/sessions: No space left on device"
                    .to_owned(),
            );
        }
        let id = match (&self.sessions, path) {
            (Some(_), "/") => LISTED_ROOT.to_owned(),
            _ => session_name(path),
        };
        Ok(Some(OpenedLog {
            log: SessionLog::new(Box::new(self.store.clone()), &id),
            path: PathBuf::from(format!("{id}.jsonl")),
            unlocked: None,
            fresh: None,
        }))
    }

    fn stamp(&self, session_id: &str) -> Option<crate::session::Stamp> {
        let records = self.store.read(session_id).ok()?;
        let bytes = serde_json::to_vec(&records).ok()?;
        let hash = crate::session::sha256_bytes_hex(&bytes);
        Some(crate::session::Stamp {
            len: bytes.len() as u64,
            modified: Duration::from_secs(u64::from_str_radix(&hash[..16], 16).ok()?),
        })
    }

    fn ended(&self, session_id: &str) -> Option<String> {
        fs::read_to_string(self.kept.join(session_id)).ok()
    }

    fn keep_ended(&self, session_id: &str, facts: &str) {
        fs::create_dir_all(&self.kept).unwrap();
        fs::write(self.kept.join(session_id), facts).unwrap();
    }

    fn sessions_dir(&self) -> Option<PathBuf> {
        self.sessions.clone()
    }
}

fn context_with(redactions: &[&str]) -> LogContext {
    LogContext {
        redactions: Redactions::new(redactions.iter().map(|r| (*r).to_owned())),
        ..log_context()
    }
}

#[derive(Clone)]
enum B {
    Text(&'static str),
    Call(String, &'static str, Value),
}

fn call(id: &str, name: &'static str, input: Value) -> B {
    B::Call(id.into(), name, input)
}

fn script(content: &[B], stop: StopReason) -> Script {
    let usage = |output| Usage {
        input_tokens: 100,
        output_tokens: output,
        ..Usage::default()
    };
    let mut script = vec![
        Ok(ProviderEvent::Sending),
        Ok(started(Some("msg"), MODEL, usage(0))),
    ];
    for (index, b) in content.iter().enumerate() {
        script.extend(match b {
            B::Text(words) => text(index, words),
            B::Call(id, name, input) => {
                tool_use(index, id, name, &[&serde_json::to_string(input).unwrap()])
            }
        });
    }
    script.extend(ended(stop, usage(10), Vec::new()));
    script
}

fn done(text: &'static str) -> Step {
    Step::from(script(&[B::Text(text)], StopReason::EndTurn))
}

fn uses(content: &[B]) -> Step {
    Step::from(script(content, StopReason::ToolUse))
}

fn hanging(text: &'static str) -> Step {
    let mut s = script(&[B::Text(text)], StopReason::EndTurn);
    s.truncate(4);
    Step::from(s).hanging()
}

fn gated(step: Step, gate: &Arc<Semaphore>) -> Step {
    step.held(Wait::Take(gate.clone()))
}

fn refused_request() -> Step {
    let error = classified(
        ErrorKind::InvalidRequest,
        "messages: something is wrong for organization org-7f3a9c",
        FailurePhase::Response,
        RetryClass::Never,
    );
    Step::from(vec![Ok(ProviderEvent::Sending), Err(error)])
}

fn spawn_call(id: &str, name: &str, task: &str) -> B {
    call(id, SPAWN_AGENT, json!({"name": name, "task": task}))
}

fn report_call(id: &str, summary: &str) -> B {
    call(id, REPORT, json!({"text": summary}))
}

const AGENT_TOOLS: [&str; 5] = [
    "list_agents",
    "rename_agent",
    "send_message",
    "spawn_agent",
    "stop_agent",
];

struct Setup {
    shell: bool,
    agents: bool,
    without: Vec<&'static str>,
    plugins: Vec<&'static str>,
    removed: Vec<&'static str>,
    child_gate: Option<Arc<tokio::sync::Notify>>,
    presets: CheckedPresets,
    limits: Limits,
    prints: Option<Prints>,
    documented: bool,
    models: Option<&'static str>,
    busy: Vec<String>,
    project_instructions: bool,
    redactions: Vec<&'static str>,
    home_files: Vec<(&'static str, &'static str)>,
    preset_config: Option<&'static str>,
    flags: Vec<crate::settings::Given>,
    hooks: Vec<(String, crate::plugin::Make)>,
    prompt_sections: bool,
    extra: Option<crate::instructions::Extra>,
    clock: Option<Arc<dyn Clock>>,
    kernels: Kernels,
    tool_starts: ToolStarts,
    mcp: crate::mcp::Service,
    saved: Option<crate::config::SavedSettings>,
    lanes: crate::lanes::Lanes,
    keep_warm_main: bool,
    restarted: bool,
    digests: bool,
    endpoints: Vec<FakeEndpoint>,
    unavailable: Vec<(&'static str, &'static str)>,
    endpoint_host: Option<Arc<dyn crate::endpoints::host::EndpointHost>>,
    full_screen: bool,
    remembered: crate::settings::remembered::RememberedStore,
    root_model: &'static str,
    root_effort: Option<Effort>,
    sessions: bool,
    follow: Vec<String>,
    start: Option<&'static str>,
    unusable: Vec<wisp_protocol::subscription_auth::Unusable>,
    plan_usage: crate::plan_usage::PlanUsageCache,
}

fn removed_saved(removed: &[&str]) -> crate::config::SavedSettings {
    let mut saved = crate::config::SavedSettings::default();
    for (line, id) in removed.iter().enumerate() {
        let path = crate::plugin::testing::plugin_path(id, wisp_protocol::settings::INSTALLED_KEY);
        saved.insert(
            path,
            wisp_protocol::settings::SettingValue::Bool(false),
            line + 1,
        );
    }
    saved
}

type Kernels = Arc<StdMutex<Vec<Arc<crate::plugin::KernelHost>>>>;
type ToolStarts = Arc<StdMutex<BTreeMap<String, Vec<String>>>>;

fn service_of(kernels: &Kernels) -> AgentsService {
    kernels
        .lock()
        .unwrap()
        .iter()
        .find_map(|k| k.tree().service)
        .expect("a host with the tree")
}

fn since_ms(service: &AgentsService, agent: &str) -> u64 {
    service.tree().entries[&path(agent)].state_since_ms
}

fn pause_at(agent: &str, point: &'static str) -> Arc<tokio::sync::Barrier> {
    let barrier = Arc::new(tokio::sync::Barrier::new(2));
    crate::agent::PAUSES
        .lock()
        .unwrap()
        .push((path(agent), point, barrier.clone()));
    barrier
}

impl Default for Setup {
    fn default() -> Self {
        Self {
            shell: false,
            agents: true,
            without: Vec::new(),
            plugins: Vec::new(),
            removed: Vec::new(),
            child_gate: None,
            presets: CheckedPresets::default(),
            limits: Limits::default(),
            prints: None,
            documented: false,
            models: None,
            busy: Vec::new(),
            project_instructions: false,
            redactions: Vec::new(),
            home_files: Vec::new(),
            preset_config: None,
            flags: Vec::new(),
            hooks: Vec::new(),
            prompt_sections: true,
            extra: None,
            clock: None,
            kernels: Kernels::default(),
            tool_starts: ToolStarts::default(),
            mcp: crate::mcp::Service::default(),
            saved: None,
            lanes: crate::lanes::Lanes::default(),
            keep_warm_main: false,
            restarted: false,
            digests: false,
            endpoints: vec![endpoint(ENV)],
            unavailable: Vec::new(),
            endpoint_host: None,
            full_screen: false,
            remembered: Default::default(),
            root_model: MODEL,
            root_effort: None,
            sessions: false,
            follow: Vec::new(),
            start: None,
            unusable: Vec::new(),
            plan_usage: Default::default(),
        }
    }
}

fn sidebar_plugin() -> (String, crate::plugin::Make) {
    use wisp_plugin_testkit::hooks as kit;
    let body = kit::section("sec", &kit::every_point());
    let make: crate::plugin::Make = kit::sections(kit::none(), kit::busy(), kit::Calls::default());
    (kit::slotted("sidebar", &body), make)
}

fn sidebar_hooks(shown: bool) -> Vec<(String, crate::plugin::Make)> {
    if shown {
        vec![sidebar_plugin()]
    } else {
        Vec::new()
    }
}

fn sidebar_shown() -> ClientCommand {
    let name = vec!["sidebar/sec".to_owned()];
    ClientCommand::SlotsShown {
        sections: name.clone(),
        pages: name,
    }
}

struct Run {
    commands: Option<mpsc::Sender<ClientCommand>>,
    events: mpsc::Receiver<AgentEvent>,
    task: Option<AgentTask>,
    providers: Arc<Providers>,
    store: MemoryStore,
    env: ScratchDir,
    seen: Vec<AgentEvent>,
    matched: std::collections::BTreeSet<usize>,
    next_request: u64,
}

fn env_view() -> EndpointView {
    let provider: Arc<dyn Provider> = scripted([]);
    EndpointView::new(
        &[agent::Connection {
            name: ENV.into(),
            provider,
            context: context_with(&[]),
            priced: true,
        }],
        &[],
        None,
    )
}

fn presets(text: &str) -> CheckedPresets {
    let file = crate::endpoints::ConfigFile::parse(
        Path::new("/h/config.toml"),
        crate::endpoints::PathShown::Default,
        text,
    )
    .unwrap();
    file.presets.checked(
        Some(&env_view()),
        &catalog_names(),
        &crate::agents::PresetPlace::default(),
    )
}

fn kit_view(kit: &Kit) -> EndpointView {
    let connected = kit.connect.connect(crate::kit::ROOT).unwrap();
    EndpointView::new(
        &connected.ready,
        &connected.unavailable,
        connected.env_account.as_deref(),
    )
}

struct Rooted {
    kit: Kit,
    agent: Agent,
    tree: TreeSetup,
    providers: Arc<Providers>,
    store: MemoryStore,
    env: ScratchDir,
}

fn kit_for(
    env: &ScratchDir,
    providers: &Arc<Providers>,
    store: &MemoryStore,
    setup: &Setup,
) -> Kit {
    let connect = {
        let providers = providers.clone();
        let redactions = setup.redactions.clone();
        let endpoints = setup.endpoints.clone();
        let unavailable: Vec<(String, String)> = setup
            .unavailable
            .iter()
            .map(|(n, r)| ((*n).to_owned(), (*r).to_owned()))
            .collect();
        move |path: &str| -> Result<Connected, String> {
            let ready: Vec<agent::Connection> = endpoints
                .iter()
                .map(|e| {
                    let provider: Arc<dyn Provider> = providers.on_endpoint(e, path);
                    let mut context = context_with(&redactions);
                    if let Some(billing) = e.billing {
                        context.credential_source = CredentialSource::Mode {
                            billing,
                            source: Box::new(CredentialSource::Environment),
                        };
                    }
                    agent::Connection {
                        name: e.name.into(),
                        provider,
                        context,
                        priced: e.priced,
                    }
                })
                .collect();
            let first = ready.first().ok_or("no endpoints")?;
            Ok(Connected {
                provider: first.provider.clone(),
                active: first.name.clone(),
                ready,
                unavailable: unavailable.clone(),
                env_account: None,
            })
        }
    };
    let mut on = setup.plugins.clone();
    if setup.agents {
        on.extend(AGENT_TOOLS.iter().filter(|t| !setup.without.contains(t)));
    }
    let tools = ToolsFn {
        shell: setup.shell,
        plugins: bundled_tools(&on, &setup.hooks, setup.prompt_sections, &setup.removed),
        exact: !setup.without.is_empty(),
        gate: setup.child_gate.clone(),
        kernels: setup.kernels.clone(),
        tool_starts: setup.tool_starts.clone(),
    };
    let mut kit = crate::test_support::rooted::Inputs {
        connect: setup
            .endpoint_host
            .as_ref()
            .and_then(|host| host.catalog())
            .map(|catalog| Arc::new(catalog) as Arc<dyn crate::kit::Connect>)
            .unwrap_or_else(|| Arc::new(connect)),
        tools: Arc::new(tools),
        logs: Arc::new(Logs {
            store: store.clone(),
            busy: setup.busy.clone(),
            sessions: setup.sessions.then(|| env.dir("sessions")),
            kept: env.root.join("ended"),
        }),
        log_context: context_with(&setup.redactions),
        models: match setup.models {
            Some(config) => ModelSettings::new(
                crate::models::ModelTable::shipped().unwrap(),
                crate::models::ConfigModels::parse(config).unwrap(),
            ),
            None => ModelSettings::shipped_only(),
        },
        auto_compact: setup.models.is_some(),
        user_input: true,
        workspace: env.workspace(),
        plugin_data: None,
        start_layers: StartLayers {
            remembered: setup.remembered.clone(),
            flags: setup.flags.clone(),
            saved: setup
                .saved
                .clone()
                .unwrap_or_else(|| removed_saved(&setup.removed)),
            full_screen: setup.full_screen,
            endpoints: setup
                .endpoints
                .iter()
                .map(|e| wisp_protocol::settings::EndpointSummary {
                    name: e.name.into(),
                    ..Default::default()
                })
                .collect(),
            ..StartLayers::default()
        },
        clock: setup.clock.clone().unwrap_or_else(|| {
            Arc::new(ShiftedClock::at(
                "2026-10-28T06:00:00Z".parse().unwrap(),
                None,
            ))
        }),
    }
    .kit();
    kit.endpoint_host = setup.endpoint_host.clone();
    kit.plan_usage = setup.plan_usage.clone();
    kit.lanes = setup.lanes.clone();
    kit.mcp = setup.mcp.clone();
    kit
}

fn instructions() -> InstructionsChoice {
    tree_instructions(false)
}

fn tree_instructions(project: bool) -> InstructionsChoice {
    InstructionsChoice {
        source: if project {
            InstructionsSource::Auto
        } else {
            InstructionsSource::None
        },
        warn_tokens: 8_000,
    }
}

fn presets_at(env: &ScratchDir, text: &str, view: &EndpointView) -> CheckedPresets {
    let config = env.root.join(".config/wisp/config.toml");
    let file =
        crate::endpoints::ConfigFile::parse(&config, crate::endpoints::PathShown::Default, text)
            .unwrap();
    file.presets.checked(
        Some(view),
        &catalog_names(),
        &crate::agents::PresetPlace::of_config(Some(&config), Some(env.root.clone()), 8_000),
    )
}

fn setup_presets(env: &ScratchDir, setup: &mut Setup, view: &EndpointView) {
    for (file, text) in &setup.home_files {
        let path = env.root.join(file);
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(path, text).unwrap();
    }
    if let Some(text) = setup.preset_config {
        setup.presets = presets_at(env, text, view);
    }
}

fn root_spec(start: Start<'_>) -> Spec<'_> {
    Spec {
        path: crate::kit::ROOT.into(),
        choices: Choices {
            model: MODEL.into(),
            effort: Effort::High,
            cache_ttl: CacheTtl::OneHour,
            message_time: MessageTime::Off,
            context_note: false,
        },
        instructions: instructions(),
        instructions_flag: None,
        extra: None,
        extra_flag: None,
        preset: None,
        start,
    }
}

async fn rooted(name: &str, mut setup: Setup, scripts: Vec<(&str, Vec<Step>)>) -> Rooted {
    let env = ScratchDir::new("wisp-agents-test-", name);
    let providers = Arc::new(Providers {
        prints: setup.prints,
        digests: setup.digests,
        documented: setup.documented,
        tables: setup
            .endpoints
            .iter()
            .map(|e| (e.name.to_owned(), e.table))
            .collect(),
        billings: setup
            .endpoints
            .iter()
            .filter_map(|e| Some((e.name.to_owned(), e.billing?)))
            .collect(),
        ..Providers::default()
    });
    for (path, steps) in scripts {
        providers.script(path, steps);
    }
    let store = MemoryStore::default();
    let kit = kit_for(&env, &providers, &store, &setup);
    setup_presets(&env, &mut setup, &kit_view(&kit));
    let instructions = tree_instructions(setup.project_instructions);
    let mut spec = Spec {
        extra: setup.extra,
        ..root_spec(Start::Fresh)
    };
    spec.choices.model = setup.root_model.into();
    spec.choices.effort = setup.root_effort.unwrap_or(spec.choices.effort);
    let tree = TreeSetup {
        presets: setup.presets,
        tools_off: catalog_names(),
        followers: tools_off_followers(),
        limits: setup.limits,
        instructions,
        loaded: LoadedTree::default(),
        keep_warm_main: setup.keep_warm_main,
        restarted: setup.restarted,
        start: None,
        unusable: Vec::new(),
    };
    let crate::test_support::rooted::Rooted { kit, built, tree } =
        crate::test_support::rooted::Rooted::build(kit, spec, tree)
            .await
            .unwrap();
    Rooted {
        kit,
        agent: built.agent,
        tree,
        providers,
        store,
        env,
    }
}

async fn start(name: &str, setup: Setup, scripts: Vec<(&str, Vec<Step>)>) -> Run {
    serve(rooted(name, setup, scripts).await)
}

fn serve(rooted: Rooted) -> Run {
    let Rooted {
        kit,
        agent,
        tree,
        providers,
        store,
        env,
    } = rooted;
    let (commands, events, task) = crate::test_support::rooted::serve(kit, tree, agent);
    Run {
        commands: Some(commands),
        events,
        task: Some(task),
        providers,
        store,
        env,
        seen: Vec::new(),
        matched: std::collections::BTreeSet::new(),
        next_request: 0,
    }
}

struct ToolsFn {
    shell: bool,
    plugins: crate::plugin::Tools,
    exact: bool,
    gate: Option<Arc<tokio::sync::Notify>>,
    kernels: Kernels,
    tool_starts: ToolStarts,
}

pub(super) fn tools_off_followers() -> Vec<(String, String)> {
    let (registry, _) = crate::plugin::Registry::new(
        wisp_plugins::bundled(),
        &crate::plugin::PluginConfig::default(),
    );
    wisp_protocol::agents::tools_off_followers(&registry.catalog(&registry.default_on(), &[]))
}

fn bundled_tools(
    on: &[&str],
    hooks: &[(String, crate::plugin::Make)],
    prompt_sections: bool,
    removed: &[&str],
) -> crate::plugin::Tools {
    let mut bundled = wisp_plugins::bundled();
    if !prompt_sections {
        bundled.retain(|b| {
            b.manifest != wisp_plugin_workspace::MANIFEST
                && b.manifest != wisp_plugin_instructions::MANIFEST
        });
    }
    bundled.push(wisp_plugin_testkit::bundled());
    let mut sources: Vec<(String, crate::plugin::Make)> = bundled
        .into_iter()
        .map(|b| {
            let make: crate::plugin::Make = Arc::new(move |host| (b.instantiate)(host));
            (b.manifest.to_owned(), make)
        })
        .collect();
    sources.extend(hooks.iter().cloned());
    let config = crate::plugin::PluginConfig {
        switches: crate::plugin::PluginSwitches {
            off: std::collections::BTreeSet::new(),
            removed: removed.iter().map(|id| (*id).to_owned()).collect(),
        },
        ..crate::plugin::PluginConfig::default()
    };
    let (registry, problems) = crate::plugin::Registry::from_sources(sources, &config);
    assert!(problems.is_empty(), "{problems:?}");
    crate::plugin::Tools::new(
        Arc::new(registry),
        on.iter().map(|s| (*s).to_owned()).collect(),
    )
}

impl crate::kit::BuildTools for ToolsFn {
    fn start<'a>(
        &'a self,
        path: &'a str,
        host: Arc<crate::plugin::KernelHost>,
    ) -> BoxFuture<'a, crate::plugin::Started> {
        self.kernels.lock().unwrap().push(host.clone());
        async move {
            let stand_ins = if self.shell {
                crate::agents::testing::shell_entries(None, &host).await
            } else {
                Vec::new()
            };
            let started = if self.exact {
                let on: Vec<String> = self.plugins.on.iter().cloned().collect();
                self.plugins
                    .start_only(path, &on, &crate::plugin::PluginSwitches::default(), host)
                    .await
            } else {
                self.plugins.start(path, host).await
            };
            crate::plugin::Started {
                toolbox: started.toolbox.with_entries(stand_ins),
                problems: started.problems,
                catalog: started.catalog,
                tool_context: started.tool_context,
                hooks: started.hooks,
                host: started.host,
            }
        }
        .boxed()
    }

    fn start_only<'a>(
        &'a self,
        path: &'a str,
        tools: &'a [String],
        switches: &'a crate::plugin::PluginSwitches,
        host: Arc<crate::plugin::KernelHost>,
    ) -> BoxFuture<'a, crate::plugin::Started> {
        self.kernels.lock().unwrap().push(host.clone());
        self.tool_starts
            .lock()
            .unwrap()
            .insert(path.to_owned(), tools.to_vec());
        async move {
            if let Some(gate) = &self.gate {
                gate.notified().await;
            }
            let kept = if self.shell {
                crate::agents::testing::shell_entries(Some(tools), &host).await
            } else {
                Vec::new()
            };
            let started = self.plugins.start_only(path, tools, switches, host).await;
            crate::plugin::Started {
                toolbox: started.toolbox.with_entries(kept),
                problems: started.problems,
                catalog: started.catalog,
                tool_context: started.tool_context,
                hooks: started.hooks,
                host: started.host,
            }
        }
        .boxed()
    }
}

fn path(p: &str) -> AgentPath {
    AgentPath::new(p)
}

fn child(agent: &str, command: ClientCommand) -> ClientCommand {
    ClientCommand::Child {
        agent: path(agent),
        command: Box::new(command),
    }
}

fn inner<'a>(event: &'a AgentEvent, agent: &str) -> Option<&'a AgentEvent> {
    match event {
        AgentEvent::Agent { agent: from, event } if from.as_str() == agent => Some(event),
        _ => None,
    }
}

fn main_spends(run: &Run) -> Vec<&BTreeMap<String, wisp_protocol::AccountSpend>> {
    pick!(&run.seen, AgentEvent::SpendChanged { accounts, .. } => accounts)
}

fn main_spend_follows_each_cost(run: &Run) {
    let mut last = None;
    for event in &run.seen {
        if let AgentEvent::SpendChanged { total, .. } = event {
            last = Some(*total);
        }
        if let Some(c) = turn_ended(event).and_then(TurnOutcome::cost) {
            let (usd, plan, lower) = (c.session_usd, c.session_plan_usd, c.session_lower_bound);
            let kernel = wisp_protocol::AccountSpend::of(usd, plan, c.session_billing, lower);
            assert_eq!(last, Some(kernel), "{c:?}");
        }
    }
}

fn turn_ended(event: &AgentEvent) -> Option<&TurnOutcome> {
    match event {
        AgentEvent::TurnEnded { outcome, .. } => Some(outcome),
        _ => None,
    }
}

impl Run {
    fn request(&mut self) -> RequestId {
        self.next_request += 1;
        RequestId(self.next_request)
    }

    async fn send(&mut self, command: ClientCommand) {
        self.commands.as_ref().unwrap().send(command).await.unwrap();
    }

    async fn describe(&mut self) -> (SessionView, Vec<AgentEvent>) {
        let request = self.request();
        self.send(ClientCommand::DescribeSession { request }).await;
        let AgentEvent::SessionSnapshot { view, state, .. } = self
            .until("the session description", |event| {
                matches!(event,
            AgentEvent::SessionSnapshot { answers, .. } if *answers == request)
            })
            .await
        else {
            panic!("not a description")
        };
        (view, state)
    }

    async fn spawn(&mut self, spawn: SpawnRequest) -> RequestId {
        let request = self.request();
        self.send(ClientCommand::SpawnAgent { request, spawn })
            .await;
        request
    }

    fn renaming(&mut self, agent: &str, name: &str) -> (ClientCommand, RequestId) {
        let request = self.request();
        let agent = path(agent);
        let name = name.into();
        let command = ClientCommand::RenameAgent {
            request,
            agent,
            name,
        };
        (command, request)
    }

    async fn say(&mut self, text: &str) {
        self.send(ClientCommand::send(format!("u{}", self.seen.len()), text))
            .await;
    }

    async fn until(&mut self, what: &str, want: impl Fn(&AgentEvent) -> bool) -> AgentEvent {
        let found =
            (0..self.seen.len()).find(|i| !self.matched.contains(i) && want(&self.seen[*i]));
        if let Some(i) = found {
            self.matched.insert(i);
            return self.seen[i].clone();
        }
        let event = until_event(&mut self.events, &mut self.seen, what, want).await;
        self.matched.insert(self.seen.len() - 1);
        event
    }

    async fn child_ended(&mut self, agent: &str) -> TurnOutcome {
        let event = self
            .until(&format!("{agent}'s turn to end"), |e| {
                inner(e, agent).and_then(turn_ended).is_some()
            })
            .await;
        inner(&event, agent).and_then(turn_ended).cloned().unwrap()
    }

    async fn root_ended(&mut self) -> TurnOutcome {
        let event = self
            .until("the root's turn to end", |e| turn_ended(e).is_some())
            .await;
        turn_ended(&event).cloned().unwrap()
    }

    fn requests(&self, path: &str) -> Vec<Sent> {
        self.providers.requests(path)
    }

    fn records(&self, path: &str) -> Vec<LogEvent> {
        self.store
            .read(&session_name(path))
            .unwrap_or_default()
            .into_iter()
            .map(|r| r.event)
            .collect()
    }

    fn mail_to(&self, path: &str) -> Vec<(MailKind, bool, String)> {
        pick!(
            self.records(path),
            LogEvent::InputAccepted {
                from: Some(from),
                text,
                ..
            } => (from.kind, from.wake, text)
        )
    }

    async fn quit(mut self) -> Self {
        self.commands = None;
        let task = self.task.take().unwrap();
        assert!(
            within("the tree's stop", task.join()).await,
            "the tree didn't stop"
        );
        while let Ok(Some(event)) =
            tokio::time::timeout(Duration::from_secs(5), self.events.recv()).await
        {
            self.seen.push(event);
        }
        self
    }
}

async fn rejected(
    run: &mut Run,
    command: ClientCommand,
    kind: &str,
) -> (String, Option<RequestId>) {
    run.send(command).await;
    let event = run
        .until("the rejection", |e| {
            matches!(e, AgentEvent::Rejected { .. })
        })
        .await;
    let AgentEvent::Rejected {
        reason,
        id,
        kind: given,
        request,
    } = event
    else {
        unreachable!()
    };
    assert_eq!(id, None);
    assert_eq!(given, Some(RejectKind::Other(kind.to_owned())));
    (reason, request)
}

fn spawn_request(name: &str, task: &str) -> SpawnRequest {
    SpawnRequest {
        name: name.into(),
        task: task.into(),
        ..SpawnRequest::default()
    }
}

#[test]
fn the_rebuild_warning_marks_its_estimate_with_a_tilde_and_a_lower_bound_with_a_plus() {
    use super::load::rebuild_cost;
    assert_eq!(rebuild_cost(&[Some(0.41)]), " (~$0.41)");
    assert_eq!(rebuild_cost(&[Some(0.41), None]), " (~$0.41+)");
    assert_eq!(rebuild_cost(&[None]), "");
}

fn sent_at(timeline: &[Moment], path: &str) -> Vec<(usize, tokio::time::Instant)> {
    timeline
        .iter()
        .enumerate()
        .filter_map(|(i, m)| match m {
            Moment::Sent(p, at) if p == path => Some((i, *at)),
            _ => None,
        })
        .collect()
}

fn quiet_call(id: &str, to: &str, text: &str) -> B {
    call(
        id,
        SEND_MESSAGE,
        json!({"to": to, "text": text, "wake": false}),
    )
}

fn refreshes(run: &Run, path: &str) -> usize {
    run.requests(path)
        .iter()
        .filter(|r| r.session.max_tokens == 1)
        .count()
}

fn stopped() -> AgentState {
    AgentState::Idle {
        outcome: Some(IdleOutcome::Stopped),
    }
}

fn hooked(calls: &Calls) -> (String, crate::plugin::Make) {
    let body = "\n[[hooks]]\nname = \"start\"\npoint = \"session-start\"\n\n[[hooks]]\nname = \"turn\"\npoint = \"before-turn\"\n";
    let make: crate::plugin::Make = kit::instantiate(kit::none(), calls.clone());
    (kit::manifest("hooked", body), make)
}

fn send_call(id: &str, to: &str, text: &str) -> B {
    call(id, SEND_MESSAGE, json!({"to": to, "text": text}))
}

fn stop_call(id: &str, name: &str, end: bool) -> B {
    call(id, STOP_AGENT, json!({"name": name, "end": end}))
}

fn rename_call(id: &str, name: &str, new_name: &str) -> B {
    call(
        id,
        RENAME_AGENT,
        json!({"name": name, "new_name": new_name}),
    )
}

fn notices(run: &Run, agent: &str) -> Vec<(bool, String)> {
    run.mail_to(agent)
        .into_iter()
        .filter(|(kind, _, _)| *kind == MailKind::Notice)
        .map(|(_, wake, text)| (wake, text))
        .collect()
}
