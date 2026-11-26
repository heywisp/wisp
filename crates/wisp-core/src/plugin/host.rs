use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError, atomic::Ordering};
use std::time::Duration;

use futures::FutureExt;
use futures::future::BoxFuture;
use tokio_util::sync::CancellationToken;
use wisp_plugin_api::chat::Chat;
use wisp_plugin_api::config::{AccountList, Accounts, Config, SettingRow};
use wisp_plugin_api::hooks::{Answer as PointAnswer, Points};
use wisp_plugin_api::host::{
    AgentRename, AgentRow, Agents, CallContext, Captured, Clock, Context, Facts, FileKind, Fill,
    Host, Instant, Job, JobError, JobInfo, Jobs, Log, Media, OutputSummary, Progress, Relation,
    Renamed, ScreenReport, ScreenReports, SecretAsk, SecretEnd, SecretRefusal, Secrets, Sent,
    Session, Settings, Size, SpawnInput, SpawnSpec, StartedAgent, Stopped, Todo, Todos, TokenCount,
    WarmMark, Workspace,
};
use wisp_plugin_api::http::{Http, HttpError, Request, Response};
use wisp_plugin_api::instructions::{Change, Diagnostic, InstructionsReport, LoadReport};
use wisp_plugin_api::manifest::{Capability, Origin};
use wisp_plugin_api::secret_check::{SecretCheck, Verdict};
use wisp_plugin_api::usage::{AccountUsage, Usage};

pub use accounts::UsageAccount;
use wisp_plugin_api::pricing::{Pricing, SpendTotals, Usage as ApiUsage};
use wisp_plugin_api::prompt::{EnvironmentFacts, JobNews, News, WorkspaceFacts};
use wisp_plugin_api::tree::{Agent, AgentNode, AgentStates, AgentTree, Identity, ShownState};
use wisp_plugin_api::types::{AskReply, FileSlice, Level, Question, SliceKind};
use wisp_plugin_api::{Kernel, Scope};
use wisp_protocol::filter::Query;
use wisp_protocol::settings::{Form, ResolvedSetting};
use wisp_protocol::{CacheTtl, ContentBlock};
use wisp_tool_text::chat as text;
use wisp_tool_text::files::{NO_DATA_FOLDER, carries, slice_supported};

use super::caller::Caller;
use super::convert;
use super::imports::{Import, Inside};
use crate::agents::{AgentsHandle, Reach, endpoints, still_working};
use crate::call::OutputSink;
use crate::call::user::UserChannel;
use crate::clock::{Clock as KernelClock, SystemClock};
use crate::jobs::Processes;
use crate::policy::{AllowAll, Gate};
use crate::pricing::{self, ModelInfo};
use crate::tool::MediaCount;
use crate::workspace::{Workspace as KernelWorkspace, shorten_path};

#[cfg(unix)]
use crate::jobs::exec::{HostSpawner, Spawner};

pub const AGENTS_PLUGIN: &str = "agents";

pub const INSTRUCTIONS_PLUGIN: &str = "instructions";

pub const MENUS_PLUGIN: &str = "menus";

const KEPT_CHANGES: usize = 4;

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Instructed {
    pub report: Option<LoadReport>,
    pub warnings: Vec<Diagnostic>,
}

pub const IN_RENDER: &str = "a sidebar section can't do this while it renders";

pub const IN_START_RENDER: &str = "a section can't do this while it renders at start";

fn in_render() -> String {
    match super::caller::Caller::current().render {
        Some(super::caller::Render::Start) => IN_START_RENDER,
        _ => IN_RENDER,
    }
    .to_owned()
}

#[derive(Clone)]
pub struct Snapshot {
    pub facts: Facts,
    pub media_types: Vec<String>,
    pub media_slices: Vec<SliceKind>,
    pub image_edge: Option<u32>,
    pub count: MediaCount,
    pub clock: Arc<dyn KernelClock>,
    pub settings: Vec<(String, String)>,
    pub fill: Fill,
    pub prices: Prices,
    pub gate: Arc<dyn Gate>,
    pub environment: Option<EnvironmentFacts>,
    pub terminal: Option<Size>,
    pub usage_accounts: Vec<UsageAccount>,
    pub usage_cache: crate::plan_usage::PlanUsageCache,
    pub no_rename: Option<&'static str>,
}

#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct Prices {
    pub model: Option<ModelInfo>,
    pub spend: SpendTotals,
}

impl Default for Snapshot {
    fn default() -> Self {
        Self {
            facts: Facts {
                model: String::new(),
                endpoint_kind: String::new(),
                effort: String::new(),
                platform: super::PLATFORM.to_owned(),
                capabilities: Vec::new(),
            },
            media_types: Vec::new(),
            media_slices: Vec::new(),
            image_edge: None,
            count: MediaCount::default(),
            clock: Arc::new(SystemClock),
            settings: Vec::new(),
            fill: Fill {
                estimate: 0,
                measured: None,
                window: None,
                point: None,
                ceiling: None,
                auto_compact: false,
                epoch: 0,
                parts: Vec::new(),
                cached: None,
            },
            prices: Prices::default(),
            gate: Arc::new(AllowAll),
            environment: None,
            terminal: None,
            usage_accounts: Vec::new(),
            usage_cache: crate::plan_usage::PlanUsageCache::default(),
            no_rename: Some(text::NOT_SAVED),
        }
    }
}

#[derive(Default)]
struct Bound {
    workspace: Option<KernelWorkspace>,
    #[cfg(unix)]
    jobs: Option<crate::jobs::Jobs>,
}

pub struct KernelHost {
    index: Mutex<index::State>,
    started: tokio::time::Instant,
    #[cfg(unix)]
    spawner: Mutex<Option<Arc<dyn Spawner>>>,
    bound: Mutex<Bound>,
    snapshot: Mutex<Snapshot>,
    calls: Mutex<BTreeMap<String, Arc<KernelCall>>>,
    owned: Mutex<BTreeSet<String>>,
    http: Mutex<Option<reqwest::Client>>,
    engine: Mutex<std::sync::Weak<super::hooks::Engine>>,
    tree: Mutex<AgentsHandle>,
    config_dir: Mutex<Option<String>>,
    data_dir: Mutex<Option<std::path::PathBuf>>,
    mcp: Mutex<crate::mcp::Service>,
    instructed: Mutex<Instructed>,
    changes: Mutex<Vec<(String, Change)>>,
    renames: Mutex<Vec<String>>,
    news: Mutex<Option<NewsSlot>>,
    #[cfg(test)]
    captured: Mutex<Option<Captured>>,
    #[cfg(any(test, feature = "test-support"))]
    fallback_home: Mutex<Option<PathBuf>>,
}

struct NewsSlot {
    plugin: String,
    news: News,
    read: bool,
}

impl std::fmt::Debug for KernelHost {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("KernelHost").finish_non_exhaustive()
    }
}

impl Default for KernelHost {
    fn default() -> Self {
        Self::new()
    }
}

fn locked<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(PoisonError::into_inner)
}

fn millis(duration: Duration) -> u64 {
    u64::try_from(duration.as_millis()).unwrap_or(u64::MAX)
}

impl KernelHost {
    pub fn new() -> Self {
        Self {
            index: Mutex::default(),
            started: tokio::time::Instant::now(),
            #[cfg(unix)]
            spawner: Mutex::new(None),
            bound: Mutex::default(),
            snapshot: Mutex::default(),
            calls: Mutex::default(),
            owned: Mutex::default(),
            http: Mutex::new(None),
            engine: Mutex::new(std::sync::Weak::new()),
            tree: Mutex::new(AgentsHandle::none()),
            config_dir: Mutex::new(None),
            data_dir: Mutex::new(None),
            mcp: Mutex::default(),
            instructed: Mutex::default(),
            changes: Mutex::default(),
            renames: Mutex::default(),
            news: Mutex::new(None),
            #[cfg(test)]
            captured: Mutex::default(),
            #[cfg(any(test, feature = "test-support"))]
            fallback_home: Mutex::new(None),
        }
    }

    pub fn set_config_dir(&self, dir: Option<String>) {
        *locked(&self.config_dir) = dir;
    }

    pub fn set_data_dir(&self, dir: Option<std::path::PathBuf>) {
        *locked(&self.data_dir) = dir;
    }

    pub(crate) fn take_renames(&self) -> Vec<String> {
        std::mem::take(&mut *locked(&self.renames))
    }

    pub fn take_instructed(&self) -> Instructed {
        std::mem::take(&mut *locked(&self.instructed))
    }

    pub fn take_change(&self, text: &str) -> Option<Change> {
        let changes = std::mem::take(&mut *locked(&self.changes));
        changes
            .into_iter()
            .rev()
            .find(|(sent, _)| sent == text)
            .map(|(_, change)| change)
    }

    pub(crate) fn set_tree(&self, handle: AgentsHandle) {
        *locked(&self.tree) = handle;
    }

    #[cfg(test)]
    pub(crate) fn tree(&self) -> AgentsHandle {
        locked(&self.tree).clone()
    }

    pub(crate) fn attach_engine(&self, engine: &Arc<super::hooks::Engine>) {
        *locked(&self.engine) = Arc::downgrade(engine);
    }

    fn engine(&self) -> Option<Arc<super::hooks::Engine>> {
        locked(&self.engine).upgrade()
    }

    pub(crate) fn offer_news(&self, plugin: &str, news: News) {
        *locked(&self.news) = Some(NewsSlot {
            plugin: plugin.to_owned(),
            news,
            read: false,
        });
    }

    pub(crate) fn withdraw_news(&self) -> bool {
        locked(&self.news).take().is_some_and(|slot| slot.read)
    }

    pub(crate) fn set_estimate(&self, estimate: u64) {
        locked(&self.snapshot).fill.estimate = estimate;
    }

    #[cfg(test)]
    #[must_use]
    pub(crate) fn with_capture(self, captured: Captured) -> Self {
        *locked(&self.captured) = Some(captured);
        self
    }

    pub fn owned_settings(&self) -> Vec<String> {
        locked(&self.owned).iter().cloned().collect()
    }

    #[cfg(unix)]
    #[must_use]
    pub fn with_spawner(self, spawner: Arc<dyn Spawner>) -> Self {
        *locked(&self.spawner) = Some(spawner);
        self
    }

    pub fn bind(&self, processes: &Processes, workspace: &KernelWorkspace) {
        locked(&self.bound).workspace = Some(workspace.clone());
        self.bind_jobs(processes);
    }

    pub fn bind_jobs(&self, processes: &Processes) {
        #[cfg(unix)]
        {
            let spawner = locked(&self.spawner)
                .get_or_insert_with(|| Arc::new(HostSpawner::detect()))
                .clone();
            locked(&self.bound).jobs = Some(crate::jobs::Jobs::new(processes.clone(), spawner));
        }
        #[cfg(not(unix))]
        let _ = processes;
    }

    pub fn publish(&self, snapshot: Snapshot) {
        *locked(&self.snapshot) = snapshot;
    }

    pub fn snapshot(&self) -> Snapshot {
        locked(&self.snapshot).clone()
    }

    pub fn elapsed_ms(&self) -> u64 {
        millis(self.started.elapsed())
    }

    pub(crate) fn open_call(&self, call: Arc<KernelCall>) {
        locked(&self.calls).insert(call.call_id.clone(), call);
    }

    pub(crate) fn close_call(&self, call_id: &str) -> Option<Arc<KernelCall>> {
        locked(&self.calls).remove(call_id)
    }

    pub(crate) fn call(&self, call_id: &str) -> Option<Arc<KernelCall>> {
        locked(&self.calls).get(call_id).cloned()
    }

    fn workspace(&self) -> Option<KernelWorkspace> {
        locked(&self.bound).workspace.clone()
    }

    #[cfg(any(test, feature = "test-support"))]
    pub fn set_fallback_home(&self, home: Option<PathBuf>) {
        *locked(&self.fallback_home) = home;
    }

    pub(super) fn secret_check_home(&self) -> Option<PathBuf> {
        if let Some(home) = self.workspace().and_then(|workspace| workspace.home) {
            return Some(home);
        }
        #[cfg(any(test, feature = "test-support"))]
        let fallback = locked(&self.fallback_home).clone();
        #[cfg(not(any(test, feature = "test-support")))]
        let fallback = crate::secrets::scan::os_detection_home();
        fallback
    }

    #[cfg(unix)]
    fn jobs(&self) -> Option<crate::jobs::Jobs> {
        locked(&self.bound).jobs.clone()
    }

    pub fn set_mcp(&self, service: crate::mcp::Service) {
        *locked(&self.mcp) = service;
    }

    pub fn mcp(&self) -> crate::mcp::Service {
        locked(&self.mcp).clone()
    }

    fn http_client(&self) -> Result<reqwest::Client, String> {
        let mut client = locked(&self.http);
        if let Some(client) = client.as_ref() {
            return Ok(client.clone());
        }
        let built = super::http::client()?;
        *client = Some(built.clone());
        Ok(built)
    }

    fn deadline_ms(&self, deadline: Option<tokio::time::Instant>) -> Option<u64> {
        deadline.map(|at| millis(at.saturating_duration_since(self.started)))
    }
}

pub(crate) fn kind_of(path: &Path) -> Option<FileKind> {
    let kind = std::fs::metadata(path).ok()?.file_type();
    if kind.is_file() {
        return Some(FileKind::Regular);
    }
    if kind.is_dir() {
        return Some(FileKind::Directory);
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::FileTypeExt;
        if kind.is_fifo() {
            return Some(FileKind::Fifo);
        }
        if kind.is_socket() {
            return Some(FileKind::Socket);
        }
        if kind.is_block_device() || kind.is_char_device() {
            return Some(FileKind::Device);
        }
    }
    Some(FileKind::Other)
}

pub struct KernelCall {
    pub(crate) call_id: String,
    pub(crate) cancel: CancellationToken,
    pub(crate) steered: CancellationToken,
    pub(crate) moved: CancellationToken,
    pub(crate) deadline: Option<tokio::time::Instant>,
    pub(crate) user: UserChannel,
    pub(crate) cwd: PathBuf,
    pub(crate) live_tail: bool,
    pub(crate) sink: Mutex<OutputSink>,
    pub(crate) host: Arc<KernelHost>,
    pub(crate) todos: Mutex<Option<Vec<Todo>>>,
    pub(crate) plugin: String,
    pub(crate) agents: AgentsHandle,
    pub(crate) view: Option<Arc<CallView>>,
    pub(crate) asker: Option<crate::agent::ask::Asker>,
    pub(crate) model_asks: Mutex<Vec<crate::agent::ask::ModelAsk>>,
}

pub struct CallView {
    pub(crate) settings: Vec<ResolvedSetting>,
    pub(crate) accounts: accounts::AccountsView,
}

impl KernelCall {
    pub(crate) fn take_model_asks(&self) -> Vec<crate::agent::ask::ModelAsk> {
        std::mem::take(&mut *locked(&self.model_asks))
    }

    pub(crate) fn take_todos(&self) -> Option<Vec<Todo>> {
        locked(&self.todos).take()
    }

    pub(crate) fn take_sink(&self) -> OutputSink {
        locked(&self.sink).take()
    }

    #[cfg(unix)]
    fn give_sink(&self, sink: OutputSink) {
        *locked(&self.sink) = sink;
    }

    pub(crate) fn finish_sink(&self, sink: &mut OutputSink) {
        *sink = self.take_sink();
    }

    #[cfg(unix)]
    fn scope(&self) -> crate::jobs::Scope {
        crate::jobs::Scope {
            call_id: self.call_id.clone(),
            cancel: self.cancel.clone(),
            steered: self.steered.clone(),
            moved: self.moved.clone(),
            deadline: self.deadline,
            user: self.user.clone(),
        }
    }
}

impl CallContext for KernelCall {
    fn id(&self) -> String {
        self.call_id.clone()
    }

    fn cwd(&self) -> String {
        self.cwd.display().to_string()
    }

    fn deadline_ms(&self) -> Option<u64> {
        self.host.deadline_ms(self.deadline)
    }

    fn cancelled(&self) -> bool {
        self.cancel.is_cancelled()
    }

    fn wait_cancelled(&self) -> BoxFuture<'_, ()> {
        self.cancel.cancelled().boxed()
    }

    fn moved(&self) -> bool {
        self.moved.is_cancelled()
    }

    fn steered(&self) -> bool {
        self.steered.is_cancelled()
    }

    fn live_tail(&self) -> bool {
        self.live_tail
    }

    fn write_output(&self, bytes: &[u8]) {
        locked(&self.sink).write_now(bytes);
    }

    fn output_summary(&self) -> OutputSummary {
        let sink = locked(&self.sink);
        let captured = sink.snapshot();
        OutputSummary {
            text: captured.tail.clone(),
            bytes: captured.bytes,
            lines: captured.lines,
            file: sink.file_path().map(|p| p.display().to_string()),
        }
    }

    fn private_file(&self, suffix: &str) -> Result<String, String> {
        let sink = locked(&self.sink);
        let Some(out) = sink.file_path() else {
            return Err(sink
                .file_error()
                .unwrap_or("there is no output file to put it beside")
                .to_owned());
        };
        let path = out.with_extension(suffix);
        let Some(text) = path.to_str().map(str::to_owned) else {
            return Err(format!("{} is not valid UTF-8", path.display()));
        };
        crate::call::create_private(&path).map_err(|e| e.to_string())?;
        Ok(text)
    }

    fn ask(&self, items: Vec<Question>) -> BoxFuture<'_, AskReply> {
        let questions = items.into_iter().map(convert::question).collect();
        async move { self.user.ask(questions).await.into() }.boxed()
    }
}

pub struct PluginHost {
    id: String,
    origin: Origin,
    capabilities: Vec<Capability>,
    world: String,
    native_families: Vec<String>,
    owned: Vec<String>,
    kernel: Arc<KernelHost>,
}

impl PluginHost {
    pub fn new(id: &str, world: &str, owned: &[String], kernel: Arc<KernelHost>) -> Self {
        locked(&kernel.owned).extend(owned.iter().cloned());
        Self {
            id: id.to_owned(),
            origin: Origin::External,
            capabilities: Vec::new(),
            world: world.to_owned(),
            native_families: Vec::new(),
            owned: owned.to_vec(),
            kernel,
        }
    }

    pub fn origin(mut self, origin: Origin) -> Self {
        self.origin = origin;
        self
    }

    pub(crate) fn native_families(mut self, families: Vec<String>) -> Self {
        self.native_families = families;
        self
    }

    pub fn capabilities(mut self, requested: &[Capability]) -> Self {
        self.capabilities = requested.to_vec();
        self
    }

    pub(crate) fn reads_usage(origin: Origin, requested: &[Capability]) -> bool {
        origin == Origin::Bundled && requested.contains(&Capability::Usage)
    }

    pub fn id(&self) -> &str {
        &self.id
    }

    fn private_data_root(&self) -> Option<PathBuf> {
        locked(&self.kernel.data_dir)
            .as_ref()
            .map(|base| base.join(&self.id))
    }

    fn refuses(&self, import: Import) -> bool {
        import.refused() || !import.spoken_in(&self.world)
    }
}

impl Session for PluginHost {
    fn current(&self) -> Facts {
        if Import::SessionCurrent.refused() {
            return Snapshot::default().facts;
        }
        self.kernel.snapshot().facts
    }
}

impl Workspace for PluginHost {
    fn root(&self) -> String {
        self.kernel
            .workspace()
            .map(|w| w.root.display().to_string())
            .unwrap_or_default()
    }

    fn home(&self) -> Option<String> {
        self.kernel
            .workspace()
            .and_then(|w| w.home)
            .map(|h| h.display().to_string())
    }

    fn shorten(&self, path: &str) -> String {
        let home = self.kernel.workspace().and_then(|w| w.home);
        shorten_path(Path::new(path), home.as_deref())
    }

    fn kind_of(&self, path: &str) -> Option<FileKind> {
        kind_of(Path::new(path))
    }
}

impl Clock for PluginHost {
    fn now(&self) -> Instant {
        if Import::ClockNow.refused() {
            return Instant {
                seconds: 0,
                nanos: 0,
            };
        }
        let now = self.kernel.snapshot().clock.now();
        Instant {
            seconds: now.as_second(),
            nanos: u32::try_from(now.subsec_nanosecond()).unwrap_or(0),
        }
    }

    fn local_offset(&self, at: Instant) -> Option<i32> {
        if Import::ClockLocalOffset.refused() {
            return None;
        }
        let clock = self.kernel.snapshot().clock;
        let zone = clock.local_zone()?;
        let at = jiff::Timestamp::new(at.seconds, i32::try_from(at.nanos).unwrap_or(0)).ok()?;
        Some(zone.to_offset(at).seconds())
    }

    fn elapsed_ms(&self) -> u64 {
        if Import::ClockElapsedMs.refused() {
            return 0;
        }
        self.kernel.elapsed_ms()
    }

    fn sleep(&self, ms: u64) -> BoxFuture<'_, ()> {
        if Import::ClockSleep.refused() {
            return futures::future::ready(()).boxed();
        }
        tokio::time::sleep(Duration::from_millis(ms)).boxed()
    }
}

impl Settings for PluginHost {
    fn get(&self, key: &str) -> Option<String> {
        if !self.owned.iter().any(|k| k == key) {
            return None;
        }
        self.kernel
            .snapshot()
            .settings
            .into_iter()
            .find(|(k, _)| k == key)
            .map(|(_, v)| v)
    }
}

impl Context for PluginHost {
    fn current_fill(&self) -> Fill {
        if Import::ContextCurrentFill.refused() {
            return Snapshot::default().fill;
        }
        self.kernel.snapshot().fill
    }

    fn estimate_text(&self, text: &str) -> u64 {
        crate::instructions::estimate_tokens(text.len() as u64)
    }
}

impl Media for PluginHost {
    fn media_types(&self) -> Vec<String> {
        if Import::MediaTypes.refused() {
            return Vec::new();
        }
        self.kernel.snapshot().media_types
    }

    fn media_slices(&self) -> Vec<SliceKind> {
        if self.refuses(Import::MediaSlices) {
            return Vec::new();
        }
        self.kernel.snapshot().media_slices
    }

    fn image_edge(&self) -> Option<u32> {
        if Import::MediaImageEdge.refused() {
            return None;
        }
        self.kernel.snapshot().image_edge
    }

    fn count_tokens(
        &self,
        media_type: String,
        bytes: Vec<u8>,
        slice: Option<FileSlice>,
    ) -> BoxFuture<'_, TokenCount> {
        if self.refuses(Import::MediaCountTokens)
            || (slice.is_some() && !Import::MediaSlices.spoken_in(&self.world))
        {
            return ready(TokenCount::Unsupported);
        }
        let Ok(slice) = slice.map(convert::file_slice).transpose() else {
            return ready(TokenCount::Unsupported);
        };
        let snapshot = self.kernel.snapshot();
        let carried: Vec<_> = snapshot.media_types.iter().map(String::as_str).collect();
        let supported = match slice {
            Some(slice) => slice_supported(
                &media_type,
                convert::slice_kind(&slice),
                &carried,
                &snapshot.media_slices,
            ),
            None => carries(&carried, &media_type),
        };
        let Ok(mime) = media_type.parse() else {
            return ready(TokenCount::Unsupported);
        };
        if !supported {
            return ready(TokenCount::Unsupported);
        }
        let block = ContentBlock::media_sliced(mime, wisp_media::encode_base64(&bytes), slice);
        let count = snapshot.count;
        async move { convert::token_count(count.count(block).await) }.boxed()
    }
}

#[cfg(unix)]
struct HostJob(crate::jobs::JobRef);

fn ready<'a, T: Send + 'a>(value: T) -> BoxFuture<'a, T> {
    futures::future::ready(value).boxed()
}

#[cfg(unix)]
mod idle {
    use wisp_plugin_api::host::{
        Found, JobInfo, JobOutput, Mode, OutputFile, Read, Rows, Settled, Waited, Written,
    };
    use wisp_plugin_api::types::Typed;

    pub(super) fn written() -> Written {
        Written {
            total: 0,
            left: 0,
            typed: Typed {
                echoed: false,
                unsent: false,
                signalled: false,
            },
            stopped: false,
            failed: None,
            after_kill: None,
        }
    }

    fn file() -> OutputFile {
        OutputFile {
            path: None,
            problem: None,
        }
    }

    pub(super) fn read() -> Read {
        Read {
            section: None,
            complete: 0,
            held: None,
            file: file(),
        }
    }

    pub(super) fn output() -> JobOutput {
        JobOutput {
            head: String::new(),
            tail: String::new(),
            bytes: 0,
            lines: 0,
            file: file(),
        }
    }

    pub(super) fn rows() -> Rows {
        Rows {
            last: 0,
            complete: 0,
            rows: Vec::new(),
            cut: false,
            held: None,
        }
    }

    pub(super) fn found() -> Found {
        Found {
            last: 0,
            rows: Vec::new(),
            shown: 0,
            remaining: 0,
            last_shown: 0,
            gaps: Vec::new(),
        }
    }

    pub(super) const CANCELLED: Waited = Waited::Cancelled;

    pub(super) const SETTLED: Settled = Settled::Cancelled;

    pub(super) fn info() -> JobInfo {
        JobInfo {
            id: None,
            call_id: None,
            label: String::new(),
            end: None,
            mode: Mode::Pipe,
            lines: 0,
            bytes: 0,
            restored: false,
            echo_off: false,
            full_screen: false,
            unread: 0,
            limit_ms: None,
            left_ms: None,
            size: None,
            keep: false,
            log: None,
        }
    }
}

#[cfg(unix)]
impl Job for HostJob {
    fn info(&self) -> BoxFuture<'_, JobInfo> {
        match Import::JobInfo.current() {
            Inside::Allowed => self.0.info().boxed(),
            Inside::Quiet => self.0.info_quiet().boxed(),
            Inside::Unavailable | Inside::NeedsCall => ready(idle::info()),
        }
    }

    fn write(
        &self,
        input: wisp_plugin_api::host::Typing,
        within_ms: u64,
    ) -> BoxFuture<'_, wisp_plugin_api::host::Written> {
        if Import::JobWrite.refused() {
            return ready(idle::written());
        }
        self.0
            .write(input, Duration::from_millis(within_ms))
            .boxed()
    }

    fn signal(
        &self,
        kind: wisp_plugin_api::types::SignalKind,
        within_ms: u64,
    ) -> BoxFuture<'_, wisp_plugin_api::host::Written> {
        if Import::JobSignal.refused() {
            return ready(idle::written());
        }
        self.0
            .signal(kind, Duration::from_millis(within_ms))
            .boxed()
    }

    fn resize(
        &self,
        to: wisp_plugin_api::host::Size,
    ) -> Result<(), wisp_plugin_api::host::JobError> {
        if Import::JobResize.refused() {
            return Ok(());
        }
        self.0.resize(to)
    }

    fn set_limit(&self, ms: Option<u64>) {
        if !Import::JobSetLimit.refused() {
            self.0.set_limit(ms);
        }
    }

    fn wait(
        &self,
        how: wisp_plugin_api::host::WaitFor,
    ) -> BoxFuture<'_, Result<wisp_plugin_api::host::Waited, String>> {
        if Import::JobWait.refused() {
            return ready(Ok(idle::CANCELLED));
        }
        self.0.wait(how).boxed()
    }

    fn hold(
        &self,
        spec: wisp_plugin_api::host::HoldSpec,
    ) -> BoxFuture<'_, wisp_plugin_api::host::Waited> {
        if Import::JobHold.refused() {
            return ready(idle::CANCELLED);
        }
        self.0.hold(spec).boxed()
    }

    fn settle(&self, quiet_ms: u64, max_ms: u64) -> BoxFuture<'_, wisp_plugin_api::host::Settled> {
        if Import::JobSettle.refused() {
            return ready(idle::SETTLED);
        }
        self.0
            .settle(
                Duration::from_millis(quiet_ms),
                Duration::from_millis(max_ms),
            )
            .boxed()
    }

    fn output(&self) -> BoxFuture<'_, wisp_plugin_api::host::JobOutput> {
        if Import::JobOutput.refused() {
            return ready(idle::output());
        }
        self.0.output().boxed()
    }

    fn saved(&self) -> BoxFuture<'_, Option<wisp_plugin_api::host::SavedFile>> {
        if Import::JobSaved.refused() {
            return ready(None);
        }
        self.0.saved().boxed()
    }

    fn screen(&self) -> BoxFuture<'_, Option<wisp_plugin_api::host::Screen>> {
        if Import::JobScreen.refused() {
            return ready(None);
        }
        self.0.screen().boxed()
    }

    fn new_output(&self) -> BoxFuture<'_, wisp_plugin_api::host::Read> {
        if Import::JobNewOutput.refused() {
            return ready(idle::read());
        }
        self.0.new_output().boxed()
    }

    fn tail(&self, lines: u32) -> BoxFuture<'_, wisp_plugin_api::host::Read> {
        if Import::JobTail.refused() {
            return ready(idle::read());
        }
        self.0.tail(lines).boxed()
    }

    fn lines(&self, first: u64, count: u32) -> BoxFuture<'_, wisp_plugin_api::host::Rows> {
        if Import::JobLines.refused() {
            return ready(idle::rows());
        }
        self.0.lines(first, count).boxed()
    }

    fn search(
        &self,
        pattern: String,
        from: u64,
        context: u32,
        max: u32,
    ) -> BoxFuture<'_, Result<wisp_plugin_api::host::Found, String>> {
        if Import::JobSearch.refused() {
            return ready(Ok(idle::found()));
        }
        async move { self.0.search(&pattern, from, context, max).await }.boxed()
    }

    fn background(&self) -> BoxFuture<'_, Option<u32>> {
        if Import::JobBackground.refused() {
            return ready(None);
        }
        self.0.background().boxed()
    }

    fn ask_attach(&self, reason: String) -> BoxFuture<'_, AskReply> {
        if Import::JobAskAttach.refused() {
            return ready(AskReply::Unavailable);
        }
        self.0.ask_attach(reason).boxed()
    }
}

pub const NO_RUNNING_CALL: &str = "no running call has this id";

pub const NO_JOBS: &str = "jobs aren't available on this platform";

impl Jobs for PluginHost {
    fn spawn<'a>(
        &'a self,
        spec: SpawnSpec,
        ctx: &'a dyn CallContext,
    ) -> BoxFuture<'a, Result<Box<dyn Job>, JobError>> {
        if Import::JobsSpawn.refused() {
            return ready(Err(JobError::Invalid(NO_RUNNING_CALL.to_owned())));
        }
        #[cfg(unix)]
        {
            let id = ctx.id();
            async move {
                let call = self
                    .kernel
                    .call(&id)
                    .ok_or_else(|| JobError::Invalid(NO_RUNNING_CALL.to_owned()))?;
                let jobs = self
                    .kernel
                    .jobs()
                    .ok_or_else(|| JobError::Invalid(NO_JOBS.to_owned()))?;
                match jobs.spawn(spec, &call.scope(), call.take_sink()).await {
                    Ok(job) => Ok(Box::new(HostJob(job)) as Box<dyn Job>),
                    Err((error, sink)) => {
                        call.give_sink(*sink);
                        Err(error)
                    }
                }
            }
            .boxed()
        }
        #[cfg(not(unix))]
        {
            let _ = (spec, ctx);
            futures::future::ready(Err(JobError::Invalid(NO_JOBS.to_owned()))).boxed()
        }
    }

    fn adopt(&self, id: u32, ctx: &dyn CallContext) -> Option<Box<dyn Job>> {
        if Import::JobsAdopt.refused() {
            return None;
        }
        #[cfg(unix)]
        {
            let call = self.kernel.call(&ctx.id())?;
            let job = self.kernel.jobs()?.adopt(id, &call.scope())?;
            Some(Box::new(HostJob(job)))
        }
        #[cfg(not(unix))]
        {
            let _ = (id, ctx);
            None
        }
    }

    fn all(&self) -> BoxFuture<'_, Vec<JobInfo>> {
        #[cfg(unix)]
        {
            async move {
                match (self.kernel.jobs(), Import::JobsAll.current()) {
                    (Some(jobs), Inside::Allowed) => jobs.all().await,
                    (Some(jobs), Inside::Quiet) => jobs.all_quiet().await,
                    _ => Vec::new(),
                }
            }
            .boxed()
        }
        #[cfg(not(unix))]
        {
            futures::future::ready(Vec::new()).boxed()
        }
    }

    fn mark_reported(&self, id: u32, ctx: &dyn CallContext) {
        if Import::JobsMarkReported.refused() {
            return;
        }
        #[cfg(unix)]
        if let (Some(call), Some(jobs)) = (self.kernel.call(&ctx.id()), self.kernel.jobs()) {
            jobs.mark_reported(id, &call.scope());
        }
        #[cfg(not(unix))]
        let _ = (id, ctx);
    }

    fn which(&self, program: &str) -> Option<String> {
        if Import::JobsWhich.refused() {
            return None;
        }
        #[cfg(unix)]
        {
            crate::jobs::exec::find_program(program).map(|path| path.display().to_string())
        }
        #[cfg(not(unix))]
        {
            let _ = program;
            None
        }
    }

    fn capture(
        &self,
        program: String,
        args: Vec<String>,
        timeout_ms: u64,
    ) -> BoxFuture<'_, Captured> {
        if Import::JobsCapture.refused() {
            return ready(Captured::Failed(in_render()));
        }
        #[cfg(test)]
        if let Some(captured) = locked(&self.kernel.captured).clone() {
            return futures::future::ready(captured).boxed();
        }
        #[cfg(unix)]
        {
            async move {
                let spawner = locked(&self.kernel.spawner)
                    .get_or_insert_with(|| Arc::new(HostSpawner::detect()))
                    .clone();
                crate::jobs::Jobs::new(Processes::default(), spawner)
                    .capture(&program, &args, timeout_ms)
                    .await
            }
            .boxed()
        }
        #[cfg(not(unix))]
        {
            let _ = (program, args, timeout_ms);
            futures::future::ready(Captured::NotFound).boxed()
        }
    }
}

impl Todos for PluginHost {
    fn replace(&self, items: Vec<Todo>, ctx: &dyn CallContext) -> Result<(), String> {
        if Import::TodosReplace.refused() {
            return Err(in_render());
        }
        let id = ctx.id();
        let call = self.kernel.call(&id).ok_or_else(|| {
            format!("no tool call {id} is running, so the checklist wasn't changed")
        })?;
        wisp_tool_text::todos::check(&items)?;
        *locked(&call.todos) = Some(wisp_tool_text::todos::normal(items));
        Ok(())
    }
}

impl PluginHost {
    fn owned_call(
        &self,
        import: Import,
        ctx: &dyn CallContext,
        owner: &str,
    ) -> Result<Arc<KernelCall>, String> {
        if import.refused() {
            return Err(in_render());
        }
        if self.id != owner {
            let interface = import.wit().split('.').next().unwrap_or_default();
            return Err(format!(
                "the {interface} interface belongs to the bundled {owner} plugin, not {}",
                self.id
            ));
        }
        let id = ctx.id();
        match self.kernel.call(&id) {
            Some(call) if call.plugin == self.id => Ok(call),
            _ => Err(format!(
                "no {owner} tool call {id} is running, so nothing was done"
            )),
        }
    }
}

fn spawn_request(input: SpawnInput) -> Result<wisp_protocol::SpawnRequest, String> {
    let effort = match input.effort {
        None => None,
        Some(name) => Some(name.parse::<wisp_protocol::Effort>().map_err(|_| {
            format!(
                "effort must be one of {}",
                wisp_protocol::Effort::WORDS.join(", ")
            )
        })?),
    };
    Ok(wisp_protocol::SpawnRequest {
        name: input.name,
        task: input.task,
        todos: input.todos,
        cwd: input.cwd,
        preset: input.preset,
        model: input.model,
        effort,
        tools_off: input.tools_off,
        spawn: input.spawn,
        keep_warm: input.keep_warm.unwrap_or(false),
        endpoint: input.endpoint,
    })
}

fn state_words(entry: &wisp_protocol::AgentEntry) -> (Option<String>, Option<String>) {
    use wisp_protocol::{Activity, AgentState};
    let detail = match (entry.state, &entry.activity) {
        (AgentState::Unknown, _) => return (None, None),
        (AgentState::Queued { reason }, _) => Some(reason.as_str().to_owned()),
        (AgentState::Running, Activity::Tool { name }) => Some(format!("using {name}")),
        (AgentState::Stopping, Activity::Tool { name }) => Some(name.clone()),
        (AgentState::Idle { outcome }, activity) => activity
            .waiting_text()
            .or_else(|| outcome.map(|o| o.as_str().to_owned())),
        (AgentState::Held { reason }, _) => Some(reason.as_str().to_owned()),
        _ => None,
    };
    (Some(entry.state.as_str().to_owned()), detail)
}

fn shown_kind(kind: Option<&'static str>, entry: &wisp_protocol::AgentEntry) -> Option<String> {
    let provider = entry.provider.as_deref()?;
    Some(
        kind.or_else(|| {
            crate::endpoints::ProviderKind::parse(provider)
                .map(|p| endpoints::account_kind(p, entry.billing, false))
        })
        .unwrap_or(provider)
        .to_owned(),
    )
}

fn shown_effort(agents: &AgentsHandle, entry: &wisp_protocol::AgentEntry) -> String {
    agents
        .display_effort(entry)
        .map_or("none", wisp_protocol::Effort::as_str)
        .to_owned()
}

fn row(reach: Reach, entry: wisp_protocol::AgentEntry, agents: &AgentsHandle) -> AgentRow {
    let root = entry.path.is_root();
    let (state, detail) = if root {
        (None, None)
    } else {
        state_words(&entry)
    };
    AgentRow {
        path: wisp_protocol::agents::shown_agent(&entry.path).to_owned(),
        session: entry.session_id.clone(),
        relation: match reach {
            Reach::Caller => Relation::Caller,
            Reach::Parent => Relation::Parent,
            Reach::Sibling => Relation::Sibling,
            Reach::Child => Relation::Child,
            Reach::Below => Relation::Descendant,
        },
        preset: entry.preset.clone(),
        model: wisp_protocol::shown_model(&entry.model).to_owned(),
        effort: shown_effort(agents, &entry),
        endpoint: entry
            .endpoint
            .as_deref()
            .map(|e| endpoints::plain(e).to_owned()),
        provider: shown_kind(agents.account_kind(&entry.path), &entry),
        state,
        detail,
        plan_micros: entry
            .plan_usd
            .filter(|_| entry.billing == wisp_protocol::Billing::Mixed)
            .map(pricing::micros),
        billing_note: (!entry.billing.is_metered()).then(|| {
            if entry.billing == wisp_protocol::Billing::Mixed {
                wisp_tool_text::agents::MIXED_BILLING_NOTE.to_owned()
            } else {
                wisp_tool_text::agents::SUBSCRIPTION_NOTE.to_owned()
            }
        }),
        spent_micros: Some(pricing::micros(entry.spent_usd)).filter(|m| !root && *m > 0),
        lower_bound: !root && entry.lower_bound,
        priced: root || entry.priced,
        checklist: entry.checklist.map(|p| Progress {
            completed: u32::try_from(p.completed).unwrap_or(u32::MAX),
            total: u32::try_from(p.total).unwrap_or(u32::MAX),
        }),
        last_report: entry
            .last_report
            .clone()
            .filter(|_| !still_working(entry.state)),
        cwd: entry.cwd.clone(),
        running_ms: entry.running_ms,
        context_used: entry.context.map(|c| c.used),
        context_window: entry.context.and_then(|c| c.window),
        context_limit: entry.context.and_then(|c| c.limit),
        context_estimated: entry.context.is_some_and(|c| c.estimated),
        keep_warm: entry.keep_warm.map(convert::warm_row),
    }
}

impl Agents for PluginHost {
    fn spawn_agent<'a>(
        &'a self,
        input: SpawnInput,
        ctx: &'a dyn CallContext,
    ) -> BoxFuture<'a, Result<StartedAgent, String>> {
        async move {
            let call = self.owned_call(Import::AgentsSpawnAgent, ctx, AGENTS_PLUGIN)?;
            let asked = input.keep_warm == Some(true);
            let request = spawn_request(input).map_err(|reason| {
                call.agents
                    .refuse(wisp_protocol::AgentAction::Spawn, reason)
            })?;
            let spawned = call.agents.spawn(request, None).await?;
            let keep_warm = if spawned.config.keep_warm {
                Some(WarmMark::Limit(
                    spawned.entry.keep_warm.map_or(0, |w| w.limit),
                ))
            } else {
                asked.then_some(WarmMark::Unsupported)
            };
            let entry = spawned.entry;
            let provider = shown_kind(call.agents.account_kind(&entry.path), &entry);
            Ok(StartedAgent {
                path: entry.path.to_string(),
                model: wisp_protocol::shown_model(&entry.model).to_owned(),
                effort: shown_effort(&call.agents, &entry),
                endpoint: endpoints::plain(entry.endpoint.as_deref().unwrap_or_default())
                    .to_owned(),
                provider: provider.unwrap_or_default(),
                cwd: entry.cwd.unwrap_or_default(),
                keep_warm,
                fell_back: spawned.fell_back,
            })
        }
        .boxed()
    }

    fn list_agents(&self, ctx: &dyn CallContext) -> Result<Vec<AgentRow>, String> {
        let call = self.owned_call(Import::AgentsListAgents, ctx, AGENTS_PLUGIN)?;
        Ok(call
            .agents
            .reachable()
            .into_iter()
            .map(|(reach, entry)| row(reach, entry, &call.agents))
            .collect())
    }

    fn report(&self, input: String, ctx: &dyn CallContext) -> Result<(), String> {
        let call = self.owned_call(Import::AgentsReport, ctx, AGENTS_PLUGIN)?;
        let value: serde_json::Value = serde_json::from_str(&input).map_err(|e| {
            call.agents.refuse(
                wisp_protocol::AgentAction::Report,
                format!("report's input is not JSON: {e}"),
            )
        })?;
        call.agents.report(&value).map(|_| ())
    }

    fn send_message(
        &self,
        to: String,
        text: String,
        wake: bool,
        ctx: &dyn CallContext,
    ) -> Result<Sent, String> {
        let call = self.owned_call(Import::AgentsSendMessage, ctx, AGENTS_PLUGIN)?;
        let (path, delivery) = call.agents.send(&to, text, wake)?;
        Ok(Sent {
            to: wisp_protocol::agents::shown_agent(&path).to_owned(),
            delivery,
        })
    }

    fn stop_agent(
        &self,
        name: String,
        end: bool,
        ctx: &dyn CallContext,
    ) -> Result<Stopped, String> {
        let call = self.owned_call(Import::AgentsStopAgent, ctx, AGENTS_PLUGIN)?;
        let (path, below) = call.agents.stop(&name, end)?;
        Ok(Stopped {
            path: path.to_string(),
            below,
            ended: end,
        })
    }
}

impl AgentRename for PluginHost {
    fn rename_agent(
        &self,
        name: String,
        new_name: String,
        ctx: &dyn CallContext,
    ) -> Result<Renamed, String> {
        let call = self.owned_call(Import::AgentRenameRenameAgent, ctx, AGENTS_PLUGIN)?;
        let (from, to) = call.agents.rename(&name, &new_name)?;
        Ok(Renamed {
            from: from.to_string(),
            to: to.to_string(),
        })
    }
}

impl Chat for PluginHost {
    fn rename(&self, name: String) -> Result<String, String> {
        let caller = super::caller::Caller::current();
        if Import::ChatRename.refused() || caller.cancel.is_none() || caller.depth > 0 {
            return Err(text::OUTSIDE.to_owned());
        }
        if let Some(reason) = self.kernel.snapshot().no_rename {
            return Err(reason.to_owned());
        }
        let name = crate::agent::naming::cleaned(&name).ok_or(text::EMPTY)?;
        locked(&self.kernel.renames).push(name.clone());
        Ok(name)
    }
}

impl wisp_plugin_api::project::Project for PluginHost {
    fn key(&self) -> Option<String> {
        if self.refuses(Import::ProjectKey) {
            return None;
        }
        let data = locked(&self.kernel.data_dir).clone()?;
        let root = self.kernel.workspace()?.root;
        crate::workspace::project_key(&root, &data)
    }
}

impl SecretCheck for PluginHost {
    fn check(&self, text: &str) -> Verdict {
        if self.refuses(Import::SecretCheckCheck) {
            return Verdict::Refused;
        }
        if let Some(checked) = Caller::current().gather_checked {
            checked.store(true, Ordering::Release);
        }
        let home = self.kernel.secret_check_home();
        if crate::secrets::scan::contains_secret_or_identity(text, home.as_deref()) {
            Verdict::Refused
        } else {
            Verdict::Safe
        }
    }
}

impl Log for PluginHost {
    fn write(&self, _level: Level, _line: &str) {}
}

impl Points for PluginHost {
    fn fire(&self, point: String, payload: String) -> BoxFuture<'_, Vec<PointAnswer>> {
        let rendering = Import::PointsFire.refused();
        match self.kernel.engine().filter(|_| !rendering) {
            Some(engine) => engine.fire_custom(&self.id, point, payload),
            None => futures::future::ready(Vec::new()).boxed(),
        }
    }
}

impl WorkspaceFacts for PluginHost {
    fn environment(&self) -> Option<EnvironmentFacts> {
        if self.refuses(Import::WorkspaceFactsEnvironment) {
            return None;
        }
        self.kernel.snapshot().environment
    }
    fn terminal(&self) -> Option<Size> {
        if self.refuses(Import::WorkspaceFactsTerminal) {
            return None;
        }
        self.kernel.snapshot().terminal
    }
    fn config_dir(&self) -> Option<String> {
        if self.refuses(Import::WorkspaceFactsConfigDir) {
            return None;
        }
        locked(&self.kernel.config_dir).clone()
    }
    fn data_dir(&self) -> Result<String, String> {
        if Import::WorkspaceFactsDataDir.refused() {
            return Err(in_render());
        }
        let dir = self
            .private_data_root()
            .filter(|_| Import::WorkspaceFactsDataDir.spoken_in(&self.world))
            .ok_or_else(|| NO_DATA_FOLDER.to_owned())?;
        crate::session::create_private_dir(&dir).map_err(|e| e.kind().to_string())?;
        Ok(dir.display().to_string())
    }
}

impl PluginHost {
    fn reports_instructions(&self, import: Import) -> bool {
        !self.refuses(import) && self.id == INSTRUCTIONS_PLUGIN
    }
}

impl InstructionsReport for PluginHost {
    fn loaded(&self, report: LoadReport) {
        if self.reports_instructions(Import::InstructionsReportLoaded) {
            *locked(&self.kernel.instructed) = Instructed {
                report: Some(report),
                warnings: Vec::new(),
            };
        }
    }

    fn warning(&self, diagnostic: Diagnostic) {
        if self.reports_instructions(Import::InstructionsReportWarning) {
            locked(&self.kernel.instructed).warnings.push(diagnostic);
        }
    }

    fn changed(&self, change: Change, text: String) {
        if self.reports_instructions(Import::InstructionsReportChanged) {
            let mut changes = locked(&self.kernel.changes);
            changes.retain(|(sent, _)| *sent != text);
            if changes.len() >= KEPT_CHANGES {
                changes.remove(0);
            }
            changes.push((text, change));
        }
    }
}

impl JobNews for PluginHost {
    fn opening(&self) -> Option<News> {
        if self.refuses(Import::JobNewsOpening) {
            return None;
        }
        let mut slot = locked(&self.kernel.news);
        let slot = slot.as_mut().filter(|slot| slot.plugin == self.id)?;
        slot.read = true;
        Some(slot.news.clone())
    }
}

impl Pricing for PluginHost {
    fn estimate(&self, usage: ApiUsage) -> Option<u64> {
        if self.refuses(Import::PricingEstimate) {
            return None;
        }
        let prices = locked(&self.kernel.snapshot).prices;
        let split = convert::usage(usage);
        let usd = pricing::estimate_usd(&prices.model?, &split, CacheTtl::OneHour);
        Some(pricing::micros(usd))
    }

    fn spend(&self) -> SpendTotals {
        if self.refuses(Import::PricingSpend) {
            return SpendTotals::default();
        }
        locked(&self.kernel.snapshot).prices.spend
    }
}

impl Http for PluginHost {
    fn fetch(&self, req: Request) -> BoxFuture<'_, Result<Response, HttpError>> {
        if !Import::HttpFetch.spoken_in(&self.world) {
            return ready(Err(HttpError::NotAllowed(
                super::http::OLD_WORLD.to_owned(),
            )));
        }
        async move {
            let gate = locked(&self.kernel.snapshot).gate.clone();
            super::http::fetch(|| self.kernel.http_client(), gate.as_ref(), &self.id, req).await
        }
        .boxed()
    }
}

impl PluginHost {
    fn tree(&self, import: Import) -> Option<AgentsHandle> {
        (import.spoken_in(&self.world) && super::caller::Caller::current().sees_agents())
            .then(|| locked(&self.kernel.tree).clone())
    }
}

impl Agent for PluginHost {
    fn current_agent(&self) -> Option<Identity> {
        let tree = self.tree(Import::AgentCurrent)?;
        let root = tree.root_session_id()?;
        tree.identity().map(|entry| convert::identity(&entry, root))
    }
}

impl AgentTree for PluginHost {
    fn snapshot(&self) -> Vec<AgentNode> {
        self.tree(Import::AgentTreeSnapshot)
            .map(|tree| tree.nodes().into_iter().map(convert::node).collect())
            .unwrap_or_default()
    }
}

impl ScreenReports for PluginHost {
    fn wait(&self, call: String, within_ms: u64) -> BoxFuture<'_, Option<ScreenReport>> {
        let owned = self
            .kernel
            .call(&call)
            .filter(|call| call.plugin == self.id);
        let Some(call) = owned.filter(|_| !self.refuses(Import::ScreenReportWait)) else {
            return ready(None);
        };
        async move {
            let report = call.user.report();
            let report = tokio::time::timeout(Duration::from_millis(within_ms), report).await;
            report.ok().flatten().map(convert::screen_report)
        }
        .boxed()
    }
}

impl Secrets for PluginHost {
    fn type_into<'a>(
        &'a self,
        ask: SecretAsk,
        ctx: &'a dyn CallContext,
    ) -> BoxFuture<'a, SecretEnd> {
        if self.refuses(Import::SecretsTypeInto) || self.origin != Origin::Bundled {
            return ready(SecretEnd::Unavailable);
        }
        #[cfg(unix)]
        {
            let id = ctx.id();
            async move {
                let (Some(call), Some(jobs)) = (self.kernel.call(&id), self.kernel.jobs()) else {
                    return SecretEnd::Unavailable;
                };
                match jobs.adopt(ask.process, &call.scope()) {
                    Some(job) => job.type_secret(&ask).await,
                    None => SecretEnd::Refused(SecretRefusal::NoProcess),
                }
            }
            .boxed()
        }
        #[cfg(not(unix))]
        {
            let _ = (ask, ctx);
            ready(SecretEnd::Unavailable)
        }
    }
}

impl AgentStates for PluginHost {
    fn states(&self) -> Vec<ShownState> {
        self.tree(Import::AgentStatesStates)
            .map(|tree| tree.nodes().iter().map(convert::shown_state).collect())
            .unwrap_or_default()
    }
}

fn setting_row(setting: &ResolvedSetting) -> SettingRow {
    let spec = setting.name.spec();
    let value = match &setting.value {
        Some(value) => {
            endpoints::plain_setting(spec.kind, value).unwrap_or_else(|| spec.shown(value))
        }
        None => spec.default_words(),
    };
    SettingRow {
        name: spec.name.to_owned(),
        value,
        layer: setting.layer.word().to_owned(),
        help: spec.help.to_owned(),
        rule: spec.describe(Form::Rule),
    }
}

impl PluginHost {
    fn viewed(
        &self,
        import: Import,
        ctx: &dyn CallContext,
    ) -> Result<(Arc<KernelCall>, Arc<CallView>), String> {
        let call = self.owned_call(import, ctx, MENUS_PLUGIN)?;
        let view = call.view.clone().ok_or_else(|| {
            format!(
                "no {MENUS_PLUGIN} tool call {} is running, so nothing was done",
                ctx.id()
            )
        })?;
        Ok((call, view))
    }
}

impl Config for PluginHost {
    fn settings(
        &self,
        filter: Option<String>,
        ctx: &dyn CallContext,
    ) -> Result<Vec<SettingRow>, String> {
        let (_, view) = self.viewed(Import::ConfigSettings, ctx)?;
        let query = Query::new(filter.as_deref().unwrap_or_default());
        Ok(view
            .settings
            .iter()
            .filter(|setting| setting.name.spec().matches(&query))
            .map(setting_row)
            .collect())
    }
}

impl Accounts for PluginHost {
    fn list<'a>(&'a self, ctx: &'a dyn CallContext) -> BoxFuture<'a, Result<AccountList, String>> {
        async move {
            let (call, view) = self.viewed(Import::AccountsList, ctx)?;
            Ok(view.accounts.list(&call.cancel).await)
        }
        .boxed()
    }
}

impl Usage for PluginHost {
    fn accounts(&self) -> Vec<AccountUsage> {
        if !Self::reads_usage(self.origin, &self.capabilities)
            || self.refuses(Import::UsageAccounts)
            || !super::caller::Caller::current().sees_agents()
        {
            return Vec::new();
        }
        self.kernel.usage_accounts()
    }
}

impl KernelHost {
    pub(crate) fn usage_accounts(&self) -> Vec<AccountUsage> {
        let snapshot = self.snapshot();
        accounts::usage(
            &snapshot.usage_accounts,
            &snapshot.usage_cache,
            snapshot.clock.now().as_second(),
        )
    }
}

impl wisp_plugin_api::model::Model for PluginHost {
    fn ask<'a>(
        &'a self,
        req: wisp_plugin_api::model::Prompt,
        ctx: &'a dyn CallContext,
    ) -> BoxFuture<'a, Result<wisp_plugin_api::model::Reply, wisp_plugin_api::model::AskError>>
    {
        use wisp_plugin_api::model::AskError;
        async move {
            if self.origin != Origin::Bundled || !self.capabilities.contains(&Capability::Model) || self.refuses(Import::ModelAsk) {
                return Err(AskError::NotAllowed("model requests need a bundled plugin requesting model in world 0.4.0 or later, inside a tool call".to_owned()));
            }
            let caller = super::caller::Caller::current();
            let call = self.kernel.call(&ctx.id()).filter(|call| call.plugin == self.id && caller.cancel.is_some() && caller.at.call_id.as_deref() == Some(call.call_id.as_str()))
                .ok_or_else(|| AskError::NotAllowed("no tool call of this plugin is running".to_owned()))?;
            if let Some(family) = &req.native && !self.native_families.contains(family) {
                return Err(AskError::NotAllowed(format!("this plugin does not request {family}")));
            }
            let asker = call.asker.as_ref().ok_or_else(|| AskError::NotAllowed("model requests are unavailable".to_owned()))?;
            asker.ask(req, &call.cancel, &call.model_asks).await
        }.boxed()
    }
}

impl wisp_plugin_api::questions::Questions for PluginHost {
    fn post<'a>(
        &'a self,
        items: Vec<Question>,
        ctx: &'a dyn CallContext,
    ) -> BoxFuture<'a, wisp_plugin_api::questions::Posted> {
        use wisp_plugin_api::questions::Posted;
        async move {
            if self.refuses(Import::QuestionsPost) {
                return Posted::Unavailable;
            }
            let Some(call) = self.kernel.call(&ctx.id()) else {
                return Posted::Unavailable;
            };
            let questions = items.into_iter().map(convert::question).collect();
            call.user.post(questions).await
        }
        .boxed()
    }

    fn withdraw<'a>(&'a self, ctx: &'a dyn CallContext) -> BoxFuture<'a, bool> {
        async move {
            if self.refuses(Import::QuestionsWithdraw) {
                return false;
            }
            let Some(call) = self.kernel.call(&ctx.id()) else {
                return false;
            };
            call.user.withdraw().await
        }
        .boxed()
    }
}

impl Host for PluginHost {
    fn chat(&self) -> &dyn Chat {
        if Import::ChatRename.spoken_in(&self.world) {
            self
        } else {
            &()
        }
    }

    fn questions(&self) -> &dyn wisp_plugin_api::questions::Questions {
        if Import::QuestionsPost.spoken_in(&self.world) {
            self
        } else {
            &()
        }
    }

    fn project(&self) -> &dyn wisp_plugin_api::project::Project {
        self
    }

    fn model(&self) -> &dyn wisp_plugin_api::model::Model {
        self
    }

    fn manage(&self) -> &dyn wisp_plugin_api::manage::Manage {
        self
    }

    fn usage(&self) -> &dyn Usage {
        self
    }

    fn mcp(&self) -> &dyn wisp_plugin_api::mcp::Mcp {
        self
    }

    fn private_files(&self) -> &dyn wisp_plugin_api::private_files::PrivateFiles {
        self
    }

    fn secret_check(&self) -> &dyn SecretCheck {
        self
    }

    fn index(&self) -> &dyn wisp_plugin_api::index::Index {
        if Import::IndexEntries.spoken_in(&self.world) {
            self
        } else {
            &()
        }
    }
    fn kernel(&self) -> Option<&dyn Kernel> {
        Some(self)
    }
}

impl Kernel for PluginHost {
    fn caller(&self) -> Arc<dyn Scope> {
        Arc::new(Caller::current().read_only_private_files())
    }

    fn restarted(&self) -> BoxFuture<'_, ()> {
        match self.kernel.engine() {
            Some(engine) => engine.restarted(&self.id),
            None => futures::future::ready(()).boxed(),
        }
    }
}

pub(crate) mod accounts;
mod index;
pub(crate) mod manage;
mod mcp;

pub(crate) mod private_files;

#[cfg(test)]
mod tests;
