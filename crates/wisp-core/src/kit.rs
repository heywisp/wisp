use std::fmt;
use std::future::Future;
use std::path::{Path, PathBuf};
use std::pin::Pin;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::thread::JoinHandle;
use std::time::{Duration, SystemTime};

use futures::future::BoxFuture;
use wisp_protocol::agents::Preset;
use wisp_protocol::log::{LogRecord, Rebuilt};
use wisp_protocol::settings::{SettingValue, find, names};
use wisp_protocol::{
    AgentPath, CacheTtl, Effort, InstructionsSource, MessageTime, ParentLink, SessionConfig,
    valid_id,
};

use crate::agent::{self, Agent, Connection, LogContext};
use crate::agent::{InstructionParts, InstructionsFacts};
use crate::call::remove_old_outputs;
use crate::clock::Clock;
use crate::context::ModelSettings;
use crate::endpoints::host::EndpointHost;
use crate::instructions::{self, Extra, Holder, PresetFiles};
use crate::lanes::Lanes;
use crate::plugin::{KernelHost, PluginSwitches, Problem, Started};
use crate::pricing::ModelInfo;
use crate::provider::{Provider, RequestOptions};
use crate::resume::{LoggedSession, Recovered, ended_records};
use crate::session::{
    FreshLog, JsonlStore, Line, Reopened, ResumeLock, SessionLog, Stamp, create_private_dir,
    new_session_id, read_lines, short_id,
};
use crate::settings::host::{SettingsHost, StartLayers};
use crate::settings::{Current, Layers, resolve_one};
use crate::warmth::WarmthTable;
use crate::workspace::{Workspace, home_shown};

pub const ROOT: &str = "/";
const DAY: Duration = Duration::from_secs(24 * 60 * 60);
static OLD_OUTPUTS_SWEPT: AtomicBool = AtomicBool::new(false);

#[derive(Clone)]
pub struct Connected {
    pub provider: Arc<dyn Provider>,
    pub ready: Vec<Connection>,
    pub unavailable: Vec<(String, String)>,
    pub active: String,
    pub env_account: Option<String>,
}

impl Connected {
    fn priced(&self) -> bool {
        self.ready
            .iter()
            .find(|c| c.name == self.active)
            .is_none_or(|c| c.priced)
    }
}

pub trait Connect: Send + Sync {
    fn connect(&self, path: &str) -> Result<Connected, String>;
}

impl Connect for Connected {
    fn connect(&self, _path: &str) -> Result<Connected, String> {
        Ok(self.clone())
    }
}

impl<F> Connect for F
where
    F: Fn(&str) -> Result<Connected, String> + Send + Sync,
{
    fn connect(&self, path: &str) -> Result<Connected, String> {
        self(path)
    }
}

pub trait BuildTools: Send + Sync {
    fn start<'a>(&'a self, path: &'a str, host: Arc<KernelHost>) -> BoxFuture<'a, Started>;

    fn start_only<'a>(
        &'a self,
        path: &'a str,
        tools: &'a [String],
        switches: &'a PluginSwitches,
        host: Arc<KernelHost>,
    ) -> BoxFuture<'a, Started>;
}

pub struct OpenedLog {
    pub log: SessionLog,
    pub path: PathBuf,
    pub unlocked: Option<String>,
    pub fresh: Option<FreshLog>,
}

pub struct ReopenedLog {
    pub log: SessionLog,
    pub lines: Vec<Line>,
    pub truncated_bytes: u64,
    pub unlocked: Option<String>,
}

pub trait OpenLog: Send + Sync {
    fn open(&self, path: &str) -> Result<Option<OpenedLog>, String>;

    fn reopen(&self, session_id: &str, _lock: ResumeLock) -> Result<ReopenedLog, String> {
        Err(format!(
            "session {} can't be reopened here",
            short_id(session_id)
        ))
    }

    fn read(&self, session_id: &str) -> Result<Vec<LogRecord>, String> {
        Err(format!(
            "session {} can't be read here",
            short_id(session_id)
        ))
    }

    fn stamp(&self, _session_id: &str) -> Option<Stamp> {
        None
    }

    fn ended(&self, _session_id: &str) -> Option<String> {
        None
    }

    fn keep_ended(&self, _session_id: &str, _facts: &str) {}

    fn sessions_dir(&self) -> Option<PathBuf> {
        None
    }
}

pub struct JsonlLogs {
    pub on: bool,
    pub dir: Option<PathBuf>,
    pub root_id: Option<String>,
}

pub(crate) fn ended_lines(bytes: &[u8]) -> Result<Vec<LogRecord>, String> {
    ended_records(read_lines(bytes).lines)
}

impl JsonlLogs {
    pub(crate) fn continuing(
        &self,
        store: JsonlStore,
        session_id: &str,
        reopened: Reopened,
    ) -> ReopenedLog {
        let Reopened {
            lines,
            next_seq,
            truncated_bytes,
            lock,
        } = reopened;
        ReopenedLog {
            log: SessionLog::continuing(Box::new(store), session_id, next_seq),
            lines,
            truncated_bytes,
            unlocked: lock.into_unlocked(),
        }
    }

    fn ended_file(&self, session_id: &str) -> Option<PathBuf> {
        valid_id(session_id).then_some(())?;
        Some(
            self.dir
                .as_ref()?
                .with_file_name("ended")
                .join(format!("{session_id}.jsonl")),
        )
    }

    pub(crate) fn store(&self) -> Result<JsonlStore, String> {
        let dir = self
            .dir
            .as_deref()
            .ok_or("no data directory (set HOME or XDG_DATA_HOME)")?;
        JsonlStore::open(dir).map_err(|e| format!("could not open {}: {e}", home_shown(dir)))
    }
}

impl OpenLog for JsonlLogs {
    fn open(&self, path: &str) -> Result<Option<OpenedLog>, String> {
        if !self.on {
            return Ok(None);
        }
        let id = (path == ROOT).then_some(self.root_id.as_deref()).flatten();
        let id = id.map_or_else(new_session_id, str::to_owned);
        let mut store = self.store()?;
        let path = store.path(&id);
        let (lock, fresh) = store
            .create_session(&id)
            .map_err(|e| format!("could not open {}: {e}", home_shown(&path)))?;
        Ok(Some(OpenedLog {
            log: SessionLog::new(Box::new(store), id),
            path,
            unlocked: lock.into_unlocked(),
            fresh,
        }))
    }

    fn reopen(&self, session_id: &str, lock: ResumeLock) -> Result<ReopenedLog, String> {
        let mut store = self.store()?;
        let reopened = store.reopen(session_id, lock).map_err(|e| e.to_string())?;
        Ok(self.continuing(store, session_id, reopened))
    }

    fn read(&self, session_id: &str) -> Result<Vec<LogRecord>, String> {
        let store = self.store()?;
        let bytes = store.read_bytes(session_id).map_err(|e| e.to_string())?;
        ended_lines(&bytes)
    }

    fn stamp(&self, session_id: &str) -> Option<Stamp> {
        self.store().ok()?.stamp(session_id)
    }

    fn ended(&self, session_id: &str) -> Option<String> {
        std::fs::read_to_string(self.ended_file(session_id)?).ok()
    }

    fn keep_ended(&self, session_id: &str, facts: &str) {
        if let Some(file) = self.ended_file(session_id).filter(|_| self.on)
            && let Some(folder) = file.parent()
            && create_private_dir(folder).is_ok()
        {
            let _ = crate::secrets::replace_atomic(&file, facts.as_bytes(), 0o600, || Ok(()));
        }
    }

    fn sessions_dir(&self) -> Option<PathBuf> {
        self.dir.clone()
    }
}

pub(crate) fn output_max_age(layers: &StartLayers) -> Option<Duration> {
    let resolved = find(names::TOOL_OUTPUT_MAX_AGE_DAYS).and_then(|spec| {
        let layers = Layers {
            session: Vec::new(),
            flags: layers.flags.clone(),
            env: layers.env.clone(),
            saved: Some(&layers.saved),
            shipped: None,
        };
        let current = Current {
            endpoint: None,
            model: String::new(),
        };
        resolve_one(spec, &layers, &current)
            .value()
            .cloned()
            .or_else(|| spec.default_value())
    });
    match resolved {
        Some(SettingValue::Int(days)) => u32::try_from(days)
            .ok()
            .and_then(|days| DAY.checked_mul(days)),
        _ => None,
    }
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct InstructionsDirs {
    pub config_dir: Option<PathBuf>,
    pub home: Option<PathBuf>,
}

pub struct Kit {
    pub plan_usage: crate::plan_usage::PlanUsageCache,
    pub usage_display: crate::plan_usage::Display,
    pub connect: Arc<dyn Connect>,
    pub tools: Arc<dyn BuildTools>,
    pub logs: Arc<dyn OpenLog>,
    pub log_context: LogContext,
    pub models: ModelSettings,
    pub options: RequestOptions,
    pub retry_budget: Option<f64>,
    pub count_tokens: bool,
    pub auto_compact: bool,
    pub user_input: bool,
    pub naming: bool,
    pub workspace: Result<Workspace, String>,
    #[cfg(test)]
    pub(crate) pin_test_environment: bool,
    pub instructions: InstructionsDirs,
    pub plugin_data: Option<PathBuf>,
    pub start_layers: StartLayers,
    pub settings_host: Option<Arc<dyn SettingsHost>>,
    pub endpoint_host: Option<Arc<dyn EndpointHost>>,
    pub clock: Arc<dyn Clock>,
    pub warmth: WarmthTable,
    pub lanes: Lanes,
    pub mcp: crate::mcp::Service,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Choices {
    pub model: String,
    pub effort: Effort,
    pub cache_ttl: CacheTtl,
    pub message_time: MessageTime,
    pub context_note: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InstructionsChoice {
    pub source: InstructionsSource,
    pub warn_tokens: u64,
}

pub struct Resume<'a> {
    pub log: SessionLog,
    pub recovered: &'a Recovered,
    pub truncated_bytes: u64,
    pub logged: Option<&'a LoggedSession>,
    pub rebuilt: Option<Box<Rebuilt>>,
}

#[derive(Debug, Clone)]
pub(crate) enum ChildResume {
    Legacy,
    Rejected,
    Pending {
        source: crate::agents::endpoints::ExactAccount,
        destination: crate::agents::endpoints::ExactAccount,
    },
    Unbound {
        source: Option<crate::agents::endpoints::ExactAccount>,
        destination: crate::agents::endpoints::ExactAccount,
    },
}

impl ChildResume {
    pub(crate) fn unbound(&self) -> bool {
        matches!(self, Self::Unbound { .. })
    }

    pub(crate) fn detach(&mut self) -> bool {
        if let Self::Pending {
            source,
            destination,
        } = self
        {
            *self = Self::Unbound {
                source: Some(source.clone()),
                destination: destination.clone(),
            };
            true
        } else {
            false
        }
    }

    pub(crate) fn reselect(&mut self, choice: crate::agents::endpoints::ExactAccount) {
        match self {
            Self::Pending { destination, .. } => *destination = choice,
            Self::Unbound {
                source,
                destination,
            } => {
                if source.is_none() && destination.name != choice.name {
                    *source = Some(destination.clone());
                }
                *destination = choice;
            }
            _ => {}
        }
    }

    pub(crate) fn account(&self, name: &str) -> Option<&crate::agents::endpoints::ExactAccount> {
        self.destination().filter(|c| c.name == name).or_else(|| {
            let source = match self {
                Self::Pending { source, .. } => Some(source),
                Self::Unbound { source, .. } => source.as_ref(),
                _ => None,
            };
            source.filter(|c| c.name == name)
        })
    }

    pub(crate) fn destination(&self) -> Option<&crate::agents::endpoints::ExactAccount> {
        match self {
            Self::Pending { destination, .. } | Self::Unbound { destination, .. } => {
                Some(destination)
            }
            _ => None,
        }
    }
}

type ChildBuilt = Result<(Agent, Vec<String>), KitError>;

type ChildAgent = Result<(Agent, Vec<wisp_protocol::TodoItem>, Vec<String>), KitError>;

pub enum Start<'a> {
    Fresh,
    Resume(Resume<'a>),
}

pub struct Spec<'a> {
    pub path: String,
    pub choices: Choices,
    pub instructions: InstructionsChoice,
    pub instructions_flag: Option<InstructionsSource>,
    pub extra: Option<Extra>,
    pub extra_flag: Option<Option<Extra>>,
    pub preset: Option<PresetSource>,
    pub start: Start<'a>,
}

impl Spec<'_> {
    pub fn logged(&self) -> Option<&LoggedSession> {
        match &self.start {
            Start::Fresh => None,
            Start::Resume(resume) => resume.logged,
        }
    }

    pub fn instructions(&self) -> InstructionsChoice {
        let logged = self.logged().and_then(|l| l.instructions_source.as_ref());
        match self.instructions_flag.as_ref().or(logged) {
            Some(source) => InstructionsChoice {
                source: source.clone(),
                ..self.instructions.clone()
            },
            None => self.instructions.clone(),
        }
    }

    pub fn extra(&self) -> Option<Extra> {
        if let Some(flag) = self.extra_flag {
            return flag;
        }
        match self.logged() {
            Some(logged) => logged.instructions_extra.as_deref().and_then(Extra::named),
            None => self.extra,
        }
    }

    pub fn session(&self) -> SessionConfig {
        let mut session = agent::default_session();
        session.model.clone_from(&self.choices.model);
        session.effort = self.choices.effort;
        session.cache_ttl = self.choices.cache_ttl;
        session.message_time = self.choices.message_time;
        session.context_note = self.choices.context_note;
        if let Some(logged) = self.logged() {
            let current = &logged.current;
            session.model.clone_from(&current.model);
            session.effort = current.effort;
            session.cache_ttl = current.cache_ttl;
            session.message_time = logged.started.message_time;
            session.context_note = logged.started.context_note;
        }
        session.effort =
            crate::models::ModelTable::shared().own_effort(&session.model, session.effort);
        session
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ChildSpec {
    pub path: AgentPath,
    pub model: String,
    pub effort: Effort,
    pub cache_ttl: CacheTtl,
    pub message_time: MessageTime,
    pub context_note: bool,
    pub instructions: InstructionsChoice,
    pub cwd: PathBuf,
    pub tools: Vec<String>,
    pub parent: ParentLink,
    pub endpoint: String,
    pub preset_instructions: Option<PresetSource>,
    pub plugins: PluginSwitches,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PresetSource {
    pub preset: String,
    pub path: PathBuf,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum WorkspaceNote {
    Missing(String),
    Gone(String),
    NoDirectory,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LogOutcome {
    Off,
    Opened {
        path: PathBuf,
        unlocked: Option<String>,
        fresh: Option<FreshLog>,
    },
    Failed(String),
    Resumed,
}

pub struct Built {
    pub agent: Agent,
    pub instructions: Option<InstructionsFacts>,
    pub workspace: Option<WorkspaceNote>,
    pub log: LogOutcome,
    pub plugin_problems: Vec<Problem>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum KitError {
    Connect(String),
    Workspace(String),
    Channel(String),
    Resume(String),
    Log(String),
    Preset(String),
}

impl fmt::Display for KitError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Connect(e)
            | Self::Workspace(e)
            | Self::Channel(e)
            | Self::Resume(e)
            | Self::Log(e)
            | Self::Preset(e) => f.write_str(e),
        }
    }
}

impl std::error::Error for KitError {}

pub fn logged_root(cwd: &str, home: Option<&Path>) -> PathBuf {
    match (cwd.strip_prefix('~'), home) {
        (Some(""), Some(home)) => home.to_owned(),
        (Some(rest), Some(home)) if rest.starts_with('/') => {
            home.join(rest.trim_start_matches('/'))
        }
        _ => PathBuf::from(cwd),
    }
}

#[cfg(test)]
pub(crate) fn pin_environment(agent: Agent) -> Agent {
    agent.with_platform("Linux").with_resources(None)
}

impl Kit {
    #[cfg(test)]
    fn with_test_environment(&self, agent: Agent) -> Agent {
        if self.pin_test_environment {
            pin_environment(agent)
        } else {
            agent
        }
    }

    pub(crate) fn preload(&self, preset: Option<&Preset>) -> Vec<String> {
        let resolved = find(names::PRELOAD).map(|setting| {
            crate::settings::resolve_one(
                setting,
                &crate::settings::Layers {
                    session: self.start_layers.forced.clone(),
                    flags: self.start_layers.flags.clone(),
                    env: self.start_layers.env.clone(),
                    saved: Some(&self.start_layers.saved),
                    shipped: None,
                },
                &crate::settings::Current {
                    endpoint: None,
                    model: String::new(),
                },
            )
        });
        wisp_protocol::settings::resolve_preload(
            preset.and_then(|preset| preset.preload.as_deref()),
            match resolved.as_ref().and_then(|resolved| resolved.value()) {
                Some(SettingValue::List(names)) => Some(names),
                _ => None,
            },
        )
    }

    pub fn build<'a>(
        &'a self,
        spec: Spec<'a>,
    ) -> Pin<Box<impl Future<Output = Result<Built, KitError>> + 'a>> {
        Box::pin(self.build_unboxed(spec))
    }

    async fn build_unboxed(&self, spec: Spec<'_>) -> Result<Built, KitError> {
        crate::session::register(&self.log_context.redactions);
        self.mcp.refresh_keys().await;
        let connected = self
            .connect
            .connect(&spec.path)
            .map_err(KitError::Connect)?;
        let kernel_host = Arc::new(KernelHost::new());
        kernel_host.set_mcp(self.mcp.clone());
        let Started {
            toolbox,
            problems: plugin_problems,
            catalog,
            tool_context,
            hooks,
            host,
        } = self.tools.start(&spec.path, kernel_host).await;
        host.set_data_dir(self.plugin_data.clone());
        host.set_mcp(self.mcp.clone());
        let plugin_records = toolbox.plugins().to_vec();
        let session = spec.session();
        let choice = spec.instructions();
        let extra = spec.extra();
        let price = self.price_on(&connected, &session.model);
        let session_model = session.model.clone();
        let fresh = matches!(spec.start, Start::Fresh);
        let Connected {
            provider,
            ready,
            unavailable,
            active,
            env_account: _,
        } = connected;
        let mut agent = Agent::new(provider, session)
            .with_clock(self.clock.clone())
            .with_warmth(self.warmth.clone())
            .with_lanes(self.lanes.clone())
            .with_request_options(self.options)
            .with_connections(ready, unavailable, &active)
            .with_plan_usage(self.plan_usage.clone(), self.usage_display)
            .with_model_settings(self.models.clone())
            .with_retry_budget(self.retry_budget)
            .with_count_tokens(self.count_tokens)
            .with_auto_compact(self.auto_compact)
            .with_natives(toolbox.natives().to_vec())
            .with_catalog(catalog)
            .with_tool_context(tool_context)
            .with_hooks(hooks, host)
            .with_user_input(self.user_input)
            .with_naming(self.naming);
        let (workspace, note) = self.workspace(spec.logged(), toolbox.is_empty())?;
        if let Some(workspace) = workspace {
            agent = if toolbox.is_empty()
                && toolbox.held_names().is_empty()
                && !toolbox.has_runners()
            {
                agent.with_workspace(workspace)
            } else {
                agent.with_tools(toolbox, workspace)
            }
            .with_instruction_parts(InstructionParts {
                source: choice.source,
                warn_tokens: choice.warn_tokens,
                extra,
                preset: self.preset_files(
                    spec.preset.as_ref(),
                    choice.warn_tokens,
                    price.as_ref().map(|info| (session_model.as_str(), info)),
                    Holder::Main,
                    fresh,
                )?,
            });
        }
        #[cfg(test)]
        let mut agent = self.with_test_environment(agent);
        agent = agent.with_plugin_records(plugin_records);
        agent = agent.with_start_layers(self.start_layers.clone());
        agent = agent.with_config_dir(self.instructions.config_dir.as_deref());
        if let Some(problem) = agent.channel_problem() {
            return Err(KitError::Channel(problem));
        }
        let warnings;
        let instructions;
        let (mut agent, log) = match spec.start {
            Start::Resume(resume) => {
                let agent = agent.with_log(resume.log, self.log_context.clone());
                let agent = if resume.recovered.fresh() {
                    let (agent, start) = agent
                        .start_over(resume.recovered)
                        .with_start_sections()
                        .await;
                    warnings = start;
                    instructions = agent.instructions_facts();
                    agent
                } else {
                    let (agent, start, facts) = agent
                        .resume_rendered(
                            resume.recovered,
                            resume.truncated_bytes,
                            resume.logged,
                            resume.rebuilt,
                        )
                        .await
                        .map_err(KitError::Resume)?;
                    warnings = start;
                    instructions = facts;
                    agent
                };
                (agent, LogOutcome::Resumed)
            }
            Start::Fresh => {
                let (started, start) = agent.with_start_sections().await;
                agent = started;
                warnings = start;
                instructions = agent.instructions_facts();
                match self.logs.open(&spec.path) {
                    Ok(Some(OpenedLog {
                        log,
                        path,
                        unlocked,
                        fresh,
                    })) => (
                        agent.with_log(log, self.log_context.clone()),
                        LogOutcome::Opened {
                            path,
                            unlocked,
                            fresh,
                        },
                    ),
                    Ok(None) => (agent, LogOutcome::Off),
                    Err(e) => (agent, LogOutcome::Failed(e)),
                }
            }
        };
        if !matches!(log, LogOutcome::Failed(_)) {
            self.remove_old_outputs(agent.session_id());
        }
        let mut plugin_problems = plugin_problems;
        plugin_problems.extend(warnings);
        if let Some(host) = &self.settings_host {
            agent = agent.with_settings_host(host.clone());
        }
        if let Some(host) = &self.endpoint_host {
            agent = agent.with_endpoint_host(host.clone());
        }
        if let Some(problem) = agent.binding_problem() {
            return Err(KitError::Connect(problem.to_owned()));
        }
        Ok(Built {
            agent,
            instructions,
            workspace: note,
            log,
            plugin_problems,
        })
    }

    fn remove_old_outputs(&self, running: &str) -> Option<JoinHandle<()>> {
        let max_age = output_max_age(&self.start_layers)?;
        let logs = self.logs.sessions_dir()?;
        let temp = self.workspace.as_ref().ok()?.temp.clone();
        if OLD_OUTPUTS_SWEPT.swap(true, Ordering::SeqCst) {
            return None;
        }
        let running = running.to_owned();
        let now = SystemTime::from(self.clock.now());
        std::thread::Builder::new()
            .name("wisp-old-outputs".into())
            .spawn(move || remove_old_outputs(&temp, &logs, &running, max_age, now))
            .ok()
    }

    pub fn build_child(
        &self,
        spec: ChildSpec,
    ) -> Pin<Box<impl Future<Output = Result<Agent, KitError>> + '_>> {
        Box::pin(async move {
            self.child(spec, Start::Fresh, None)
                .await
                .map(|(agent, _)| agent)
        })
    }

    pub async fn resume_child(
        &self,
        spec: ChildSpec,
        resume: Resume<'_>,
    ) -> Result<(Agent, Vec<String>), KitError> {
        self.child(spec, Start::Resume(resume), None).await
    }

    pub(crate) async fn resume_connected_child(
        &self,
        spec: ChildSpec,
        resume: Resume<'_>,
        connected: Connected,
        placement: ChildResume,
    ) -> ChildBuilt {
        self.child(spec, Start::Resume(resume), Some((connected, placement)))
            .await
    }

    fn child<'a>(
        &'a self,
        spec: ChildSpec,
        start: Start<'a>,
        connected: Option<(Connected, ChildResume)>,
    ) -> Pin<Box<impl Future<Output = ChildBuilt> + 'a>> {
        Box::pin(async move {
            let path = spec.path.as_str().to_owned();
            let (agent, checklist, missing) = self
                .child_agent(spec, matches!(start, Start::Fresh), connected)
                .await?;
            self.child_start(agent, start, checklist, &path, missing)
                .await
        })
    }

    fn child_agent(
        &self,
        spec: ChildSpec,
        fresh: bool,
        connected: Option<(Connected, ChildResume)>,
    ) -> Pin<Box<impl Future<Output = ChildAgent> + '_>> {
        Box::pin(self.child_agent_unboxed(spec, fresh, connected))
    }

    async fn child_agent_unboxed(
        &self,
        spec: ChildSpec,
        fresh: bool,
        connected: Option<(Connected, ChildResume)>,
    ) -> ChildAgent {
        let path = spec.path.as_str();
        let (mut connected, placement) = match connected {
            Some(checked) => checked,
            None => (
                self.connect.connect(path).map_err(KitError::Connect)?,
                ChildResume::Legacy,
            ),
        };
        if !connected.ready.is_empty() {
            if !connected.ready.iter().any(|c| c.name == spec.endpoint) {
                return Err(KitError::Connect(format!(
                    "account {} isn't available to agents",
                    spec.endpoint
                )));
            }
            connected.active.clone_from(&spec.endpoint);
        }
        let kernel_host = Arc::new(KernelHost::new());
        kernel_host.set_mcp(self.mcp.clone());
        let Started {
            toolbox,
            catalog,
            tool_context,
            hooks,
            host,
            ..
        } = self
            .tools
            .start_only(path, &spec.tools, &spec.plugins, kernel_host)
            .await;
        let preload = self.preload(spec.parent.config.preset_values.as_deref());
        let toolbox = if fresh {
            toolbox.preload(&catalog, &preload)
        } else {
            toolbox
        };
        host.set_data_dir(self.plugin_data.clone());
        host.set_mcp(self.mcp.clone());
        let plugin_records = toolbox.plugins().to_vec();
        let started: Vec<String> = toolbox
            .names()
            .into_iter()
            .chain(toolbox.native_names())
            .collect();
        let (kept, missing): (Vec<String>, Vec<String>) = spec
            .tools
            .iter()
            .cloned()
            .partition(|name| started.contains(name) || toolbox.is_enabled(name));
        let mut session = agent::default_session();
        session.model.clone_from(&spec.model);
        session.effort = spec.effort;
        session.cache_ttl = spec.cache_ttl;
        session.message_time = spec.message_time;
        session.context_note = spec.context_note;
        let price = self.price_on(&connected, &session.model);
        let session_model = session.model.clone();
        let checklist = spec.parent.todos.clone();
        let Connected {
            provider,
            ready,
            unavailable,
            active,
            env_account: _,
        } = connected;
        let base = self.workspace.clone().map_err(KitError::Workspace)?;
        let workspace = Workspace {
            root: spec.cwd.clone(),
            ..base
        };
        let preset = self.preset_files(
            spec.preset_instructions.as_ref(),
            spec.instructions.warn_tokens,
            price.as_ref().map(|info| (session_model.as_str(), info)),
            Holder::Child,
            fresh,
        )?;
        let parts = InstructionParts {
            source: spec.instructions.source.clone(),
            warn_tokens: spec.instructions.warn_tokens,
            extra: None,
            preset,
        };
        let agent = Agent::new_for_child_resume(provider, session, placement);
        let connected = move |agent: Agent| {
            agent
                .with_clock(self.clock.clone())
                .with_warmth(self.warmth.clone())
                .with_lanes(self.lanes.clone())
                .with_request_options(self.options)
                .with_connections(ready, unavailable, &active)
                .with_plan_usage(self.plan_usage.clone(), self.usage_display)
                .with_model_settings(self.models.clone())
                .with_retry_budget(self.retry_budget)
                .with_count_tokens(self.count_tokens)
        };
        let tooled = move |agent: Agent| {
            let agent = agent
                .with_auto_compact(self.auto_compact)
                .with_natives(toolbox.natives().to_vec())
                .with_catalog(catalog)
                .with_tool_context(tool_context)
                .with_hooks(hooks, host)
                .with_user_input(false);
            if toolbox.is_empty() && !toolbox.has_runners() {
                agent.with_workspace(workspace)
            } else {
                agent.with_tools(toolbox, workspace)
            }
        };
        let placed = move |agent: Agent| {
            agent
                .with_instruction_parts(parts)
                .with_plugin_records(plugin_records)
                .with_path(spec.path.clone())
                .with_start_layers(self.start_layers.clone())
                .with_config_dir(self.instructions.config_dir.as_deref())
                .with_parent(spec.parent)
                .with_plugin_switches(&spec.plugins)
        };
        let agent = placed(tooled(connected(agent)))
            .with_endpoint_catalog(self.endpoint_host.as_ref().and_then(|host| host.catalog()));
        #[cfg(test)]
        let agent = self.with_test_environment(agent);
        let agent = if fresh {
            agent
        } else {
            agent.with_fixed_tools(kept.into_iter().collect())
        };
        if let Some(problem) = agent.binding_problem() {
            return Err(KitError::Connect(problem.to_owned()));
        }
        Ok((agent, checklist, missing))
    }

    fn price_on(&self, connected: &Connected, model: &str) -> Option<ModelInfo> {
        let kind = connected.provider.kind();
        let info = self.models.price(&connected.active, model, kind);
        info.filter(|_| connected.priced())
    }

    fn preset_files(
        &self,
        source: Option<&PresetSource>,
        warn_tokens: u64,
        priced: Option<(&str, &ModelInfo)>,
        holder: Holder,
        fresh: bool,
    ) -> Result<Option<PresetFiles>, KitError> {
        let Some(source) = source else {
            return Ok(None);
        };
        match instructions::load_preset(
            &source.path,
            self.instructions.home.as_deref(),
            &source.preset,
            warn_tokens,
            priced,
            holder,
        ) {
            Ok(files) => Ok(Some(files)),
            Err(problem) if fresh => Err(KitError::Preset(format!(
                "preset {}'s instructions no longer load: {problem}",
                source.preset
            ))),
            Err(_) => Ok(None),
        }
    }

    fn child_start<'a>(
        &'a self,
        agent: Agent,
        start: Start<'a>,
        checklist: Vec<wisp_protocol::TodoItem>,
        path: &'a str,
        missing: Vec<String>,
    ) -> Pin<Box<impl Future<Output = ChildBuilt> + 'a>> {
        Box::pin(async move {
            match start {
                Start::Resume(resume) => self.resumed_child(agent, resume, missing).await,
                Start::Fresh => {
                    let (agent, _) = agent.with_start_sections().await;
                    self.fresh_child(agent, checklist, path, missing)
                }
            }
        })
    }

    fn resumed_child<'a>(
        &'a self,
        agent: Agent,
        resume: Resume<'a>,
        missing: Vec<String>,
    ) -> Pin<Box<impl Future<Output = ChildBuilt> + 'a>> {
        let rendering = agent
            .with_log(resume.log, self.log_context.clone())
            .resume_rendered(
                resume.recovered,
                resume.truncated_bytes,
                resume.logged,
                resume.rebuilt,
            );
        Box::pin(async move {
            let (agent, _, _) = rendering.await.map_err(KitError::Resume)?;
            Ok((agent, missing))
        })
    }

    fn fresh_child(
        &self,
        agent: Agent,
        checklist: Vec<wisp_protocol::TodoItem>,
        path: &str,
        missing: Vec<String>,
    ) -> ChildBuilt {
        let agent = agent.with_checklist(checklist);
        match self.logs.open(path) {
            Ok(Some(opened)) => Ok((
                agent.with_log(opened.log, self.log_context.clone()),
                missing,
            )),
            Ok(None) => Ok((agent, missing)),
            Err(e) => Err(KitError::Log(e)),
        }
    }

    fn workspace(
        &self,
        logged: Option<&LoggedSession>,
        no_tools: bool,
    ) -> Result<(Option<Workspace>, Option<WorkspaceNote>), KitError> {
        match &self.workspace {
            Ok(workspace) => match logged.map(|l| l.started.cwd.as_deref()) {
                Some(Some(cwd)) => {
                    let root = logged_root(cwd, workspace.home.as_deref());
                    if root.is_dir() {
                        Ok((
                            Some(Workspace {
                                root,
                                ..workspace.clone()
                            }),
                            None,
                        ))
                    } else {
                        Ok((
                            Some(workspace.clone()),
                            Some(WorkspaceNote::Gone(cwd.to_owned())),
                        ))
                    }
                }
                Some(None) => Ok((Some(workspace.clone()), Some(WorkspaceNote::NoDirectory))),
                None => Ok((Some(workspace.clone()), None)),
            },
            Err(e) if no_tools => Ok((None, Some(WorkspaceNote::Missing(e.clone())))),
            Err(e) => Err(KitError::Workspace(e.clone())),
        }
    }
}

#[cfg(test)]
mod tests;
