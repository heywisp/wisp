use std::future::Future;
use std::pin::Pin;
use std::sync::{Arc, Mutex, PoisonError};
use std::task::{Context as TaskContext, Poll, Waker};

use futures::future::BoxFuture;
use wisp_plugin_api::Kernel;
use wisp_plugin_api::config::{AccountList, Accounts, Config, SettingRow};
use wisp_plugin_api::hooks::{Answer as PointAnswer, Points};
use wisp_plugin_api::host::{
    AgentRename, AgentRow, Agents, CallContext, Captured, Clock, Context, Delivery, Facts,
    FileKind, Fill, Host, Instant, Job, JobError, JobInfo, Jobs, Log, Media, OutputSummary,
    Renamed, ScreenReport, ScreenReports, SecretAsk, SecretEnd, Secrets, Sent, Session, Settings,
    Size, SpawnInput, SpawnSpec, StartedAgent, Stopped, Todo, Todos, TokenCount, Workspace,
};
use wisp_plugin_api::http::{Http, HttpError, Request, Response};
use wisp_plugin_api::instructions::{Change, Diagnostic, InstructionsReport, LoadReport};
use wisp_plugin_api::pricing::{Pricing, SpendTotals, Usage};
use wisp_plugin_api::prompt::{EnvironmentFacts, JobNews, News, WorkspaceFacts};
use wisp_plugin_api::tree::{
    Activity, Agent, AgentNode, AgentState, AgentStates, AgentTree, Identity, ShownState,
};
use wisp_plugin_api::types::{AskReply, Level, Question};
use wisp_tool_text::files::NO_DATA_FOLDER;
use wisp_tool_text::paths::shorten_path;

pub type ModelReplies = Arc<
    Mutex<
        std::collections::VecDeque<
            Result<wisp_plugin_api::model::Reply, wisp_plugin_api::model::AskError>,
        >,
    >,
>;

pub type CountFn = dyn Fn(String, Vec<u8>) -> BoxFuture<'static, TokenCount> + Send + Sync;
pub type SleepFn = dyn Fn(u64) -> BoxFuture<'static, ()> + Send + Sync;
pub type FireFn = dyn Fn(String, String) -> BoxFuture<'static, Vec<PointAnswer>> + Send + Sync;
pub type KindFn = dyn Fn(&str) -> Option<FileKind> + Send + Sync;
pub type ElapsedFn = dyn Fn() -> u64 + Send + Sync;
pub type NowFn = dyn Fn() -> Instant + Send + Sync;
pub type OffsetFn = dyn Fn(Instant) -> Option<i32> + Send + Sync;
pub type AskFn = dyn Fn(Vec<Question>) -> BoxFuture<'static, AskReply> + Send + Sync;
pub type EstimateFn = dyn Fn(Usage) -> Option<u64> + Send + Sync;
pub type Replaced = Arc<Mutex<Vec<(String, Vec<Todo>)>>>;
pub type FetchFn = dyn Fn(Request) -> BoxFuture<'static, Result<Response, HttpError>> + Send + Sync;
pub type Spawned = Arc<Mutex<Vec<(String, SpawnInput)>>>;
pub type Reported = Arc<Mutex<Vec<(String, String)>>>;
pub type Messaged = Arc<Mutex<Vec<(String, String, String, bool)>>>;
pub type Halted = Arc<Mutex<Vec<(String, String, bool)>>>;
pub type Renames = Arc<Mutex<Vec<(String, String, String)>>>;
pub type Instructed = Arc<Mutex<Vec<Report>>>;
pub type SecretCalls = Arc<Mutex<Vec<(String, SecretAsk)>>>;
pub type PostedCalls = Arc<Mutex<Vec<(String, Vec<Question>)>>>;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Report {
    Loaded(LoadReport),
    Warning(Diagnostic),
    Changed(Box<Change>, String),
}

#[derive(Clone, Default)]
pub struct FakeHost {
    pub root: String,
    pub home: Option<String>,
    pub capabilities: Vec<String>,
    pub media_types: Vec<String>,
    pub image_edge: Option<u32>,
    pub count: Option<Arc<CountFn>>,
    pub sleep: Option<Arc<SleepFn>>,
    pub fire: Option<Arc<FireFn>>,
    pub kind_of: Option<Arc<KindFn>>,
    pub elapsed: Option<Arc<ElapsedFn>>,
    pub now: Option<Arc<NowFn>>,
    pub offset: Option<Arc<OffsetFn>>,
    pub fill: Option<Fill>,
    pub settings: Vec<(String, String)>,
    pub estimate: Option<Arc<EstimateFn>>,
    pub spend: SpendTotals,
    pub todos: Replaced,
    pub refuse_todos: Option<String>,
    pub fetch: Option<Arc<FetchFn>>,
    pub captured: Option<Captured>,
    pub programs: Vec<(String, String)>,
    pub spawned: Spawned,
    pub reported: Reported,
    pub messaged: Messaged,
    pub halted: Halted,
    pub renamed: Renames,
    pub agents: Vec<AgentRow>,
    pub refuse_agents: Option<String>,
    pub warm_refreshes: u32,
    pub identity: Option<Identity>,
    pub tree: Vec<AgentNode>,
    pub usage: Vec<wisp_plugin_api::usage::AccountUsage>,
    pub states: Vec<ShownState>,
    pub screen_report: Option<ScreenReport>,
    pub secret_end: Option<SecretEnd>,
    pub secret_asked: SecretCalls,
    pub environment: Option<EnvironmentFacts>,
    pub config_dir: Option<String>,
    pub data_dir: Option<String>,
    pub terminal: Option<Size>,
    pub instructed: Instructed,
    pub news: Option<News>,
    pub jobs: Vec<JobInfo>,
    pub config: Vec<SettingRow>,
    pub refuse_config: Option<String>,
    pub accounts: AccountList,
    pub kernel: Option<Arc<dyn Kernel>>,
    pub model_prompts: Arc<Mutex<Vec<wisp_plugin_api::model::Prompt>>>,
    pub model_replies: ModelReplies,
    pub entries: Vec<wisp_plugin_api::index::Entry>,
    pub index_changed: Option<Arc<dyn Fn() + Send + Sync>>,
    pub mcp: Option<Arc<dyn wisp_plugin_api::mcp::Mcp>>,
    pub opened: Vec<(String, Result<String, String>)>,
    pub managed: Arc<Mutex<Vec<wisp_plugin_api::manage::Change>>>,
    pub manage_reply: Option<Result<String, String>>,
    pub chat_renamed: Arc<Mutex<Vec<String>>>,
    pub rename_reply: Option<Result<String, String>>,
    pub posted_questions: PostedCalls,
    pub post_reply: Option<wisp_plugin_api::questions::Posted>,
    pub withdraw_calls: Arc<Mutex<Vec<String>>>,
    pub withdraw_reply: bool,
}

pub fn identity(path: &str) -> Identity {
    Identity {
        path: path.to_owned(),
        name: path.trim_start_matches('/').to_owned(),
        parent: (path != "/").then(|| "/".to_owned()),
        preset: None,
        depth: u32::from(path != "/"),
        session_id: "s".to_owned(),
        root_session_id: "s".to_owned(),
    }
}

pub fn agent_node(path: &str, state: AgentState) -> AgentNode {
    let (parent, name) = path.rsplit_once('/').unwrap_or_default();
    AgentNode {
        path: path.to_owned(),
        name: name.to_owned(),
        parent: Some(if parent.is_empty() { "/" } else { parent }.to_owned()),
        session_id: "s1".to_owned(),
        preset: None,
        model: "claude-test".to_owned(),
        effort: "low".to_owned(),
        endpoint: None,
        provider: None,
        depth: u32::try_from(path.matches('/').count()).unwrap_or(u32::MAX),
        state,
        activity: Activity::Idle,
        since_ms: 0,
        spent_micros: 0,
        lower_bound: false,
        priced: true,
        checklist: None,
        cwd: None,
        running_ms: None,
        context: None,
    }
}

fn kind_from_metadata(path: &str) -> Option<FileKind> {
    let meta = std::fs::metadata(path).ok()?;
    Some(if meta.is_file() {
        FileKind::Regular
    } else if meta.is_dir() {
        FileKind::Directory
    } else {
        FileKind::Other
    })
}

impl Session for FakeHost {
    fn current(&self) -> Facts {
        Facts {
            model: String::new(),
            endpoint_kind: String::new(),
            effort: String::new(),
            platform: "unix".to_owned(),
            capabilities: self.capabilities.clone(),
        }
    }
}

impl Workspace for FakeHost {
    fn root(&self) -> String {
        self.root.clone()
    }

    fn home(&self) -> Option<String> {
        self.home.clone()
    }

    fn shorten(&self, path: &str) -> String {
        shorten_path(
            std::path::Path::new(path),
            self.home.as_deref().map(std::path::Path::new),
        )
    }

    fn kind_of(&self, path: &str) -> Option<FileKind> {
        match &self.kind_of {
            Some(kind_of) => kind_of(path),
            None => kind_from_metadata(path),
        }
    }
}

impl Clock for FakeHost {
    fn now(&self) -> Instant {
        self.now.as_ref().map_or(
            Instant {
                seconds: 0,
                nanos: 0,
            },
            |now| now(),
        )
    }

    fn local_offset(&self, at: Instant) -> Option<i32> {
        match &self.offset {
            Some(offset) => offset(at),
            None => Some(0),
        }
    }

    fn elapsed_ms(&self) -> u64 {
        self.elapsed.as_ref().map_or(0, |elapsed| elapsed())
    }

    fn sleep(&self, ms: u64) -> BoxFuture<'_, ()> {
        match &self.sleep {
            Some(sleep) => sleep(ms),
            None => Box::pin(std::future::ready(())),
        }
    }
}

impl Settings for FakeHost {
    fn get(&self, key: &str) -> Option<String> {
        self.settings
            .iter()
            .find(|(k, _)| k == key)
            .map(|(_, v)| v.clone())
    }
}

impl Context for FakeHost {
    fn current_fill(&self) -> Fill {
        self.fill.clone().unwrap_or(Fill {
            estimate: 0,
            measured: None,
            window: None,
            point: None,
            ceiling: None,
            auto_compact: false,
            epoch: 0,
            parts: Vec::new(),
            cached: None,
        })
    }

    fn estimate_text(&self, text: &str) -> u64 {
        (text.len() as u64).div_ceil(4)
    }
}

impl Media for FakeHost {
    fn media_types(&self) -> Vec<String> {
        self.media_types.clone()
    }

    fn image_edge(&self) -> Option<u32> {
        self.image_edge
    }

    fn count_tokens(&self, media_type: String, bytes: Vec<u8>) -> BoxFuture<'_, TokenCount> {
        match &self.count {
            Some(count) => count(media_type, bytes),
            None => Box::pin(std::future::ready(TokenCount::Unsupported)),
        }
    }
}

impl Jobs for FakeHost {
    fn spawn<'a>(
        &'a self,
        _spec: SpawnSpec,
        _ctx: &'a dyn CallContext,
    ) -> BoxFuture<'a, Result<Box<dyn Job>, JobError>> {
        Box::pin(std::future::ready(Err(JobError::Invalid(
            "the fake host runs no jobs".to_owned(),
        ))))
    }

    fn adopt(&self, _id: u32, _ctx: &dyn CallContext) -> Option<Box<dyn Job>> {
        None
    }

    fn all(&self) -> BoxFuture<'_, Vec<JobInfo>> {
        Box::pin(std::future::ready(self.jobs.clone()))
    }

    fn mark_reported(&self, _id: u32, _ctx: &dyn CallContext) {}

    fn which(&self, program: &str) -> Option<String> {
        self.programs
            .iter()
            .find(|(name, _)| name == program)
            .map(|(_, path)| path.clone())
    }

    fn capture(
        &self,
        _program: String,
        _args: Vec<String>,
        _timeout_ms: u64,
    ) -> BoxFuture<'_, Captured> {
        let captured = self.captured.clone().unwrap_or(Captured::NotFound);
        Box::pin(std::future::ready(captured))
    }
}

impl Todos for FakeHost {
    fn replace(&self, items: Vec<Todo>, ctx: &dyn CallContext) -> Result<(), String> {
        if let Some(why) = &self.refuse_todos {
            return Err(why.clone());
        }
        self.todos
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .push((ctx.id(), items));
        Ok(())
    }
}

impl Agents for FakeHost {
    fn spawn_agent<'a>(
        &'a self,
        input: SpawnInput,
        ctx: &'a dyn CallContext,
    ) -> BoxFuture<'a, Result<StartedAgent, String>> {
        let result = match &self.refuse_agents {
            Some(why) => Err(why.clone()),
            None => {
                let started = StartedAgent {
                    path: format!("/{}", input.name),
                    model: input
                        .model
                        .clone()
                        .unwrap_or_else(|| "claude-test".to_owned()),
                    effort: input.effort.clone().unwrap_or_else(|| "high".to_owned()),
                    endpoint: input.endpoint.clone().unwrap_or_else(|| "env".to_owned()),
                    provider: "anthropic".to_owned(),
                    cwd: input.cwd.clone().unwrap_or_else(|| self.root.clone()),
                    keep_warm: input
                        .keep_warm
                        .filter(|marked| *marked)
                        .map(|_| wisp_plugin_api::host::WarmMark::Limit(self.warm_refreshes)),
                    fell_back: None,
                };
                self.spawned
                    .lock()
                    .unwrap_or_else(PoisonError::into_inner)
                    .push((ctx.id(), input));
                Ok(started)
            }
        };
        Box::pin(std::future::ready(result))
    }

    fn list_agents(&self, _ctx: &dyn CallContext) -> Result<Vec<AgentRow>, String> {
        match &self.refuse_agents {
            Some(why) => Err(why.clone()),
            None => Ok(self.agents.clone()),
        }
    }

    fn report(&self, input: String, ctx: &dyn CallContext) -> Result<(), String> {
        if let Some(why) = &self.refuse_agents {
            return Err(why.clone());
        }
        self.reported
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .push((ctx.id(), input));
        Ok(())
    }

    fn send_message(
        &self,
        to: String,
        text: String,
        wake: bool,
        ctx: &dyn CallContext,
    ) -> Result<Sent, String> {
        if let Some(why) = &self.refuse_agents {
            return Err(why.clone());
        }
        self.messaged
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .push((ctx.id(), to.clone(), text, wake));
        let delivery = if wake {
            Delivery::Woken
        } else {
            Delivery::Idle
        };
        Ok(Sent { to, delivery })
    }

    fn stop_agent(
        &self,
        name: String,
        end: bool,
        ctx: &dyn CallContext,
    ) -> Result<Stopped, String> {
        if let Some(why) = &self.refuse_agents {
            return Err(why.clone());
        }
        self.halted
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .push((ctx.id(), name.clone(), end));
        let path = if name.starts_with('/') {
            name
        } else {
            format!("/{name}")
        };
        Ok(Stopped {
            path,
            below: 0,
            ended: end,
        })
    }
}

impl AgentRename for FakeHost {
    fn rename_agent(
        &self,
        name: String,
        new_name: String,
        ctx: &dyn CallContext,
    ) -> Result<Renamed, String> {
        if let Some(why) = &self.refuse_agents {
            return Err(why.clone());
        }
        self.renamed
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .push((ctx.id(), name.clone(), new_name.clone()));
        let from = if name.starts_with('/') {
            name
        } else {
            format!("/{name}")
        };
        let to = match from.rsplit_once('/') {
            Some((parent, _)) => format!("{parent}/{new_name}"),
            None => new_name,
        };
        Ok(Renamed { from, to })
    }
}

impl Log for FakeHost {
    fn write(&self, _level: Level, _line: &str) {}
}

impl Points for FakeHost {
    fn fire(&self, point: String, payload: String) -> BoxFuture<'_, Vec<PointAnswer>> {
        match &self.fire {
            Some(fire) => fire(point, payload),
            None => Box::pin(std::future::ready(Vec::new())),
        }
    }
}

impl WorkspaceFacts for FakeHost {
    fn environment(&self) -> Option<EnvironmentFacts> {
        self.environment.clone()
    }
    fn config_dir(&self) -> Option<String> {
        self.config_dir.clone()
    }
    fn data_dir(&self) -> Result<String, String> {
        self.data_dir
            .clone()
            .ok_or_else(|| NO_DATA_FOLDER.to_owned())
    }
    fn terminal(&self) -> Option<Size> {
        self.terminal
    }
}

impl InstructionsReport for FakeHost {
    fn loaded(&self, report: LoadReport) {
        self.instructed
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .push(Report::Loaded(report));
    }

    fn warning(&self, diagnostic: Diagnostic) {
        self.instructed
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .push(Report::Warning(diagnostic));
    }

    fn changed(&self, change: Change, text: String) {
        self.instructed
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .push(Report::Changed(Box::new(change), text));
    }
}

impl JobNews for FakeHost {
    fn opening(&self) -> Option<News> {
        self.news.clone()
    }
}

impl Pricing for FakeHost {
    fn estimate(&self, usage: Usage) -> Option<u64> {
        self.estimate.as_ref().and_then(|estimate| estimate(usage))
    }
    fn spend(&self) -> SpendTotals {
        self.spend
    }
}

pub const NO_NETWORK: &str = "the fake host has no network";

impl Http for FakeHost {
    fn fetch(&self, req: Request) -> BoxFuture<'_, Result<Response, HttpError>> {
        match &self.fetch {
            Some(fetch) => fetch(req),
            None => Box::pin(std::future::ready(Err(HttpError::NotAllowed(
                NO_NETWORK.to_owned(),
            )))),
        }
    }
}

impl Agent for FakeHost {
    fn current_agent(&self) -> Option<Identity> {
        self.identity.clone()
    }
}

impl AgentTree for FakeHost {
    fn snapshot(&self) -> Vec<AgentNode> {
        self.tree.clone()
    }
}

impl AgentStates for FakeHost {
    fn states(&self) -> Vec<ShownState> {
        self.states.clone()
    }
}

impl ScreenReports for FakeHost {
    fn wait(&self, _call: String, _within_ms: u64) -> BoxFuture<'_, Option<ScreenReport>> {
        Box::pin(std::future::ready(self.screen_report.clone()))
    }
}

impl Secrets for FakeHost {
    fn type_into<'a>(
        &'a self,
        ask: SecretAsk,
        ctx: &'a dyn CallContext,
    ) -> BoxFuture<'a, SecretEnd> {
        self.secret_asked
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .push((ctx.id(), ask));
        Box::pin(std::future::ready(
            self.secret_end.unwrap_or(SecretEnd::Unavailable),
        ))
    }
}

impl Config for FakeHost {
    fn settings(
        &self,
        _filter: Option<String>,
        _ctx: &dyn CallContext,
    ) -> Result<Vec<SettingRow>, String> {
        match &self.refuse_config {
            Some(why) => Err(why.clone()),
            None => Ok(self.config.clone()),
        }
    }
}

impl Accounts for FakeHost {
    fn list<'a>(&'a self, _ctx: &'a dyn CallContext) -> BoxFuture<'a, Result<AccountList, String>> {
        let listed = match &self.refuse_config {
            Some(why) => Err(why.clone()),
            None => Ok(self.accounts.clone()),
        };
        Box::pin(std::future::ready(listed))
    }
}

impl wisp_plugin_api::index::Index for FakeHost {
    fn changed(&self) {
        if let Some(changed) = &self.index_changed {
            changed();
        }
    }

    fn entries(&self) -> Vec<wisp_plugin_api::index::Entry> {
        self.entries.clone()
    }
    fn open<'a>(&'a self, name: &'a str) -> BoxFuture<'a, Result<String, String>> {
        Box::pin(async move {
            self.opened
                .iter()
                .find(|(key, _)| key == name)
                .map(|(_, value)| value.clone())
                .unwrap_or_else(|| Err(format!("nothing named `{name}` was found")))
        })
    }
}

impl wisp_plugin_api::usage::Usage for FakeHost {
    fn accounts(&self) -> Vec<wisp_plugin_api::usage::AccountUsage> {
        self.usage.clone()
    }
}

impl wisp_plugin_api::manage::Manage for FakeHost {
    fn manage<'a>(
        &'a self,
        change: wisp_plugin_api::manage::Change,
        _ctx: &'a dyn CallContext,
    ) -> BoxFuture<'a, Result<String, String>> {
        self.managed
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .push(change);
        let reply = self
            .manage_reply
            .clone()
            .unwrap_or_else(|| Err(wisp_plugin_api::manage::UNAVAILABLE.to_owned()));
        Box::pin(std::future::ready(reply))
    }
}

impl wisp_plugin_api::model::Model for FakeHost {
    fn ask<'a>(
        &'a self,
        req: wisp_plugin_api::model::Prompt,
        _ctx: &'a dyn CallContext,
    ) -> BoxFuture<'a, Result<wisp_plugin_api::model::Reply, wisp_plugin_api::model::AskError>>
    {
        self.model_prompts
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .push(req);
        let reply = self
            .model_replies
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .pop_front()
            .unwrap_or_else(|| {
                Err(wisp_plugin_api::model::AskError::Failed(
                    "no scripted reply".to_owned(),
                ))
            });
        Box::pin(std::future::ready(reply))
    }
}

impl wisp_plugin_api::chat::Chat for FakeHost {
    fn rename(&self, name: String) -> Result<String, String> {
        self.chat_renamed
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .push(name.clone());
        self.rename_reply.clone().unwrap_or(Ok(name))
    }
}

impl wisp_plugin_api::questions::Questions for FakeHost {
    fn post<'a>(
        &'a self,
        items: Vec<Question>,
        ctx: &'a dyn CallContext,
    ) -> BoxFuture<'a, wisp_plugin_api::questions::Posted> {
        self.posted_questions
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .push((ctx.id(), items));
        let reply = self
            .post_reply
            .unwrap_or(wisp_plugin_api::questions::Posted::Unavailable);
        Box::pin(std::future::ready(reply))
    }

    fn withdraw<'a>(&'a self, ctx: &'a dyn CallContext) -> BoxFuture<'a, bool> {
        self.withdraw_calls
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .push(ctx.id());
        Box::pin(std::future::ready(self.withdraw_reply))
    }
}

impl Host for FakeHost {
    fn chat(&self) -> &dyn wisp_plugin_api::chat::Chat {
        self
    }

    fn questions(&self) -> &dyn wisp_plugin_api::questions::Questions {
        self
    }

    fn mcp(&self) -> &dyn wisp_plugin_api::mcp::Mcp {
        self.mcp.as_deref().unwrap_or(&())
    }

    fn usage(&self) -> &dyn wisp_plugin_api::usage::Usage {
        self
    }

    fn model(&self) -> &dyn wisp_plugin_api::model::Model {
        self
    }

    fn manage(&self) -> &dyn wisp_plugin_api::manage::Manage {
        self
    }

    fn index(&self) -> &dyn wisp_plugin_api::index::Index {
        self
    }
    fn kernel(&self) -> Option<&dyn Kernel> {
        self.kernel.as_deref()
    }
}

#[derive(Default)]
struct FlagState {
    set: bool,
    waiting: Vec<Waker>,
}

#[derive(Clone, Default)]
pub struct Flag(Arc<Mutex<FlagState>>);

impl Flag {
    pub fn set(&self) {
        let waiting = {
            let mut state = self.0.lock().unwrap_or_else(PoisonError::into_inner);
            state.set = true;
            std::mem::take(&mut state.waiting)
        };
        for waker in waiting {
            waker.wake();
        }
    }

    pub fn is_set(&self) -> bool {
        self.0.lock().unwrap_or_else(PoisonError::into_inner).set
    }

    pub fn wait(&self) -> FlagWait {
        FlagWait(self.clone())
    }
}

pub struct FlagWait(Flag);

impl Future for FlagWait {
    type Output = ();

    fn poll(self: Pin<&mut Self>, cx: &mut TaskContext<'_>) -> Poll<()> {
        let mut state = (self.0).0.lock().unwrap_or_else(PoisonError::into_inner);
        if state.set {
            return Poll::Ready(());
        }
        state.waiting.push(cx.waker().clone());
        Poll::Pending
    }
}

#[derive(Clone, Default)]
pub struct FakeCall {
    pub id: String,
    pub cwd: String,
    pub cancel: Flag,
    pub moved: Flag,
    pub steered: Flag,
    pub live_tail: bool,
    pub output: Arc<Mutex<Vec<u8>>>,
    pub ask: Option<Arc<AskFn>>,
    pub private_dir: Option<std::path::PathBuf>,
}

impl FakeCall {
    pub fn written(&self) -> Vec<u8> {
        self.output
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .clone()
    }
}

impl CallContext for FakeCall {
    fn id(&self) -> String {
        self.id.clone()
    }

    fn cwd(&self) -> String {
        self.cwd.clone()
    }

    fn deadline_ms(&self) -> Option<u64> {
        None
    }

    fn cancelled(&self) -> bool {
        self.cancel.is_set()
    }

    fn wait_cancelled(&self) -> BoxFuture<'_, ()> {
        Box::pin(self.cancel.wait())
    }

    fn moved(&self) -> bool {
        self.moved.is_set()
    }

    fn steered(&self) -> bool {
        self.steered.is_set()
    }

    fn live_tail(&self) -> bool {
        self.live_tail
    }

    fn write_output(&self, bytes: &[u8]) {
        self.output
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .extend_from_slice(bytes);
    }

    fn output_summary(&self) -> OutputSummary {
        let written = self.written();
        OutputSummary {
            text: String::from_utf8_lossy(&written).into_owned(),
            bytes: written.len() as u64,
            lines: written.iter().filter(|b| **b == b'\n').count() as u64,
            file: None,
        }
    }

    fn private_file(&self, suffix: &str) -> Result<String, String> {
        let Some(dir) = &self.private_dir else {
            return Err("the fake call has no output file".to_owned());
        };
        let path = dir.join(format!("call.{suffix}"));
        std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&path)
            .map_err(|e| e.to_string())?;
        Ok(path.display().to_string())
    }

    fn ask(&self, items: Vec<Question>) -> BoxFuture<'_, AskReply> {
        match &self.ask {
            Some(ask) => ask(items),
            None => Box::pin(std::future::ready(AskReply::Unavailable)),
        }
    }
}
