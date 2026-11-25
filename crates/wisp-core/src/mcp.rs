use std::collections::{BTreeMap, BTreeSet};
use std::future::Future;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError, Weak};
use std::time::Duration;

use base64::Engine;
use rmcp::model::{
    CallToolRequestParams, ClientCapabilities, ClientConfig, ContentBlock, GetPromptRequestParams,
    GetPromptResponse, Implementation, PaginatedRequestParams, ProtocolVersion,
    ReadResourceRequestParams, ReadResourceResponse, ResourceContents, SubscriptionFilter,
};
use rmcp::service::{NotificationContext, Peer, RoleClient, RunningService, ServiceError};
use rmcp::{ClientLifecycleMode, ClientServiceExt};
use tokio::process::{Child, Command};
use tokio::sync::Notify;
use tokio_util::sync::{CancellationToken, DropGuard};
use wisp_plugin_api::mcp::{Prompt, PromptMessage, Resource};
use wisp_protocol::ToolOutcome;
use wisp_tool_text::mcp as text;

use crate::config::McpServer;
use crate::session::Redactions;
use crate::tool::{Attachment, ToolRun, ToolSpec, canonical_specs};

mod elicit;
mod http;
mod keys;
mod saved;
mod sign_in;
pub(crate) use saved::HELD_CREDENTIAL;
pub use saved::{List, Prompt as SavedPrompt, Resource as SavedResource};
pub use sign_in::is_sign_in_link;

pub(crate) const PLUGIN: &str = "mcp";

pub(crate) fn link_note(
    name: &str,
    link: String,
    help: bool,
) -> Option<wisp_protocol::plugin::LinkNote> {
    let host = reqwest::Url::parse(&link).ok()?.host_str()?.to_owned();
    Some(wisp_protocol::plugin::LinkNote {
        plugin: PLUGIN.into(),
        message: wisp_tool_text::manage::sign_in_note(name, &host),
        link: wisp_protocol::Secret::new(link),
        opened: None,
        help: if help {
            text::sign_in_help(name).into()
        } else {
            Vec::new()
        },
    })
}

const START_LIMIT: Duration = Duration::from_millis(wisp_plugin_api::mcp::START_LIMIT_MS);
const OPTIONAL_LIMIT: Duration = Duration::from_secs(START_LIMIT.as_secs() / 12);
const SIGN_IN_REQUIRED: &str = "needs sign-in";
const TOOL_PREFIX: &str = "mcp__";
const NAME_OVERLAP: &str = "has overlapping mapped tool names";

#[derive(Clone)]
pub struct Service(Arc<Inner>, Option<Arc<DropGuard>>);

impl Default for Service {
    fn default() -> Self {
        Self::new(BTreeMap::new(), PathBuf::new(), None)
    }
}

#[derive(Default)]
struct Inner {
    closed: CancellationToken,
    servers: Mutex<BTreeMap<String, Arc<Server>>>,
    mutation: Arc<tokio::sync::Mutex<()>>,
    folder: Option<PathBuf>,
    config: Mutex<Option<(PathBuf, crate::endpoints::PathShown)>>,
    store: Mutex<Option<crate::secrets::StoreKind>>,
    cwd: PathBuf,
    revision: AtomicU64,
    changes: AtomicU64,
    known: Mutex<BTreeMap<String, KnownTool>>,
    retired_redactions: Mutex<BTreeMap<CacheOwner, Redactions>>,
    off: Mutex<BTreeSet<String>>,
    notes: Mutex<Vec<Note>>,
    ended: Mutex<Vec<SignInEnd>>,
    ended_signal: Arc<Notify>,
    awaited: Mutex<BTreeSet<String>>,
    forms: AtomicBool,
}

type CacheOwner = (String, String);

#[derive(Clone)]
struct KnownTool {
    owner: CacheOwner,
    original: String,
}

struct Server {
    config: McpServer,
    owner: Option<Arc<keys::RunKeyOwner>>,
    flights: elicit::Flights,
    keys: Mutex<BTreeMap<String, wisp_protocol::Secret>>,
    redactions: Mutex<Redactions>,
    name_problem: Option<&'static str>,
    saved: saved::Saved,
    snapshot: Mutex<Snapshot>,
    connection: tokio::sync::Mutex<Option<Connection>>,
    watch: Arc<Watch>,
    pending: tokio::sync::Mutex<Option<Arc<sign_in::Pending>>>,
    challenge: Arc<Mutex<Option<String>>>,
}

struct Snapshot {
    list: Option<List>,
    state: State,
    listed: Option<u64>,
}

struct Watch {
    name: String,
    notices: Mutex<Notices>,
}

#[derive(Default)]
struct Notices {
    owner: Weak<Inner>,
    seen: u64,
    relisting: bool,
}

impl Watch {
    fn notices(&self) -> MutexGuard<'_, Notices> {
        self.notices.lock().unwrap_or_else(PoisonError::into_inner)
    }

    fn bind(&self, owner: Weak<Inner>) -> u64 {
        let mut notices = self.notices();
        notices.owner = owner;
        notices.seen
    }

    fn notice(self: &Arc<Self>) {
        let mut notices = self.notices();
        notices.seen += 1;
        if let Some(owner) = notices
            .owner
            .upgrade()
            .filter(|owner| !owner.closed.is_cancelled())
        {
            owner.changes.fetch_add(1, Ordering::AcqRel);
            if !notices.relisting {
                notices.relisting = true;
                let watch = self.clone();
                tokio::spawn(async move {
                    let service = Service(owner, None);
                    tokio::select! {
                        biased;
                        () = service.0.closed.cancelled() => {},
                        () = service.relist(watch) => {},
                    }
                });
            }
        }
    }

    fn settle(&self, attempted: Option<u64>) -> bool {
        let mut notices = self.notices();
        if attempted.is_none() || attempted == Some(notices.seen) {
            notices.relisting = false;
        }
        !notices.relisting
    }
}

struct Connection {
    running: RunningService<RoleClient, Asking>,
    _group: Option<ProcessGroup>,
}

impl Connection {
    fn is_live(&self) -> bool {
        !self.running.is_closed() && !self.running.peer().is_transport_closed()
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum State {
    Stopped,
    Starting,
    Ready,
    NeedsSignIn,
    NeedsKey(Vec<String>),
    Failed(String),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct KeyResult {
    pub tools: u32,
    pub kept: bool,
}

enum Edit<'a> {
    Add(&'a McpServer),
    Remove,
    Keys(&'a [String]),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Row {
    pub off: bool,
    pub name: String,
    pub state: State,
    pub tools: usize,
    pub fresh: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Note {
    Changed {
        by: String,
        name: String,
        added: bool,
    },
    SignIn(String),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SignInEnd {
    pub line: String,
    pub awaited: bool,
}

impl Server {
    fn snapshot(&self) -> MutexGuard<'_, Snapshot> {
        self.snapshot.lock().unwrap_or_else(PoisonError::into_inner)
    }

    fn cache_owner(&self) -> CacheOwner {
        (self.watch.name.clone(), self.saved.key().to_owned())
    }

    fn redactions(&self) -> MutexGuard<'_, Redactions> {
        self.redactions
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
    }
}

struct Starting<'a>(&'a Server);

impl Drop for Starting<'_> {
    fn drop(&mut self) {
        let mut snapshot = self.0.snapshot();
        if snapshot.state == State::Starting {
            snapshot.state = State::Stopped;
        }
    }
}

impl std::fmt::Debug for Service {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_list().entries(self.servers().keys()).finish()
    }
}

fn saved_servers(config: &Path) -> Result<BTreeMap<String, McpServer>, &'static str> {
    crate::config_file::ConfigText::read(config, crate::endpoints::PathShown::Given)
        .and_then(|text| text.current())
        .map(|config| config.mcp_servers)
        .map_err(|_| "could not read config.toml")
}

fn pin_config(path: &Path) -> Option<PathBuf> {
    match std::fs::canonicalize(path) {
        Ok(path) => Some(path),
        Err(error)
            if error.kind() == std::io::ErrorKind::NotFound
                && path
                    .symlink_metadata()
                    .is_err_and(|error| error.kind() == std::io::ErrorKind::NotFound) =>
        {
            let parent = path
                .parent()
                .filter(|p| !p.as_os_str().is_empty())
                .unwrap_or(Path::new("."));
            Some(pin_config(parent)?.join(path.file_name()?))
        }
        Err(_) => None,
    }
}

fn check_pinned_config(config: &Path) -> Result<(), &'static str> {
    (pin_config(config).as_deref() == Some(config))
        .then_some(())
        .ok_or("config.toml has moved")
}

fn run_config_read_error() -> String {
    format!("could not read {}", crate::config::MCP_CONFIG_FILE)
}

fn check_pinned_run(run: &Path) -> Result<(), String> {
    if std::fs::canonicalize(run).is_ok_and(|canonical| canonical == run) {
        return Ok(());
    }
    Err(run_config_read_error())
}

fn run_definition(config: &Path, run: &Path, name: &str) -> Result<McpServer, String> {
    use crate::config::{MCP_CONFIG_FILE, mcp_servers_in};
    check_pinned_config(config)?;
    check_pinned_run(run)?;
    if saved_servers(config)?.contains_key(name) {
        return Err(text::NAME_EXISTS.into());
    }
    let text = std::fs::read_to_string(run).map_err(|_| run_config_read_error())?;
    mcp_servers_in(&text)?
        .remove(name)
        .ok_or_else(|| format!("the server is no longer in {MCP_CONFIG_FILE}"))
}

impl Service {
    pub fn new(
        servers: BTreeMap<String, McpServer>,
        cwd: PathBuf,
        folder: Option<PathBuf>,
    ) -> Self {
        Self::start_servers(servers, cwd, folder, |_, _| None)
    }

    pub fn new_with_run_config(
        servers: BTreeMap<String, McpServer>,
        cwd: PathBuf,
        folder: Option<PathBuf>,
        run_file: &Path,
        run_servers: &BTreeMap<String, McpServer>,
        config: Option<&Path>,
    ) -> Result<Self, String> {
        if run_servers.is_empty() {
            return Ok(Self::new(servers, cwd, folder));
        }
        check_pinned_run(run_file)?;
        let run_cwd = std::fs::canonicalize(&cwd)
            .map_err(|_| "could not find the server's working directory")?;
        let config = config
            .map(|path| pin_config(path).ok_or("could not resolve config.toml"))
            .transpose()?;
        let origin = |name: &str, server: &McpServer| {
            run_servers.contains_key(name).then(|| {
                Arc::new(keys::RunKeyOwner::new(
                    name,
                    server,
                    run_cwd.clone(),
                    run_file.to_owned(),
                    config.clone(),
                ))
            })
        };
        Ok(Self::start_servers(servers, cwd, folder, origin))
    }

    fn start_servers(
        servers: BTreeMap<String, McpServer>,
        cwd: PathBuf,
        folder: Option<PathBuf>,
        origin: impl Fn(&str, &McpServer) -> Option<Arc<keys::RunKeyOwner>>,
    ) -> Self {
        let closed = CancellationToken::new();
        let guard = Arc::new(closed.clone().drop_guard());
        let service = Self::build(servers, cwd, folder, closed, Some(guard), origin);
        for server in service.servers().values() {
            server.watch.bind(Arc::downgrade(&service.0));
        }
        service
    }

    fn build(
        servers: BTreeMap<String, McpServer>,
        cwd: PathBuf,
        folder: Option<PathBuf>,
        closed: CancellationToken,
        guard: Option<Arc<DropGuard>>,
        origin: impl Fn(&str, &McpServer) -> Option<Arc<keys::RunKeyOwner>>,
    ) -> Self {
        let prefixes: Vec<_> = servers.keys().map(|name| tool_name(name, "")).collect();
        let servers = servers
            .into_iter()
            .map(|(name, config)| {
                let owner = origin(&name, &config);
                let saved = saved::Saved::new(folder.as_deref(), &name, &config, owner.as_deref());
                let overlap = prefixes
                    .iter()
                    .filter(|prefix| **prefix == tool_name(&name, ""))
                    .count()
                    > 1;
                let (list, failure) = if overlap {
                    (
                        None,
                        Some("has a configured name that overlaps after mapping"),
                    )
                } else {
                    match saved.load_safe(&Redactions::default()) {
                        Ok(list) => (
                            list.filter(|list| check_names(&name, &list.tools).is_ok()),
                            None,
                        ),
                        Err(reason) => (None, Some(reason)),
                    }
                };
                let name_problem =
                    overlap.then_some("has a configured name that overlaps after mapping");
                let state = failure.map_or(State::Stopped, |reason| {
                    State::Failed(format!("{name} {reason}"))
                });
                let server = Server {
                    owner,
                    config,
                    flights: Arc::default(),
                    keys: Mutex::default(),
                    redactions: Mutex::default(),
                    name_problem,
                    saved,
                    snapshot: Mutex::new(Snapshot {
                        list,
                        state,
                        listed: None,
                    }),
                    connection: tokio::sync::Mutex::new(None),
                    watch: Arc::new(Watch {
                        name: name.clone(),
                        notices: Mutex::default(),
                    }),
                    pending: tokio::sync::Mutex::new(None),
                    challenge: Arc::default(),
                };
                (name, Arc::new(server))
            })
            .collect();
        let service = Self(
            Arc::new(Inner {
                closed,
                servers: Mutex::new(servers),
                mutation: Arc::default(),
                folder,
                config: Mutex::new(None),
                store: Mutex::default(),
                cwd,
                revision: AtomicU64::new(0),
                changes: AtomicU64::new(0),
                known: Mutex::default(),
                retired_redactions: Mutex::default(),
                off: Mutex::default(),
                notes: Mutex::default(),
                ended: Mutex::default(),
                ended_signal: Arc::default(),
                awaited: Mutex::default(),
                forms: AtomicBool::new(false),
            }),
            guard,
        );
        service.remember_names(|| {});
        service
    }

    fn remember_names(&self, publish: impl FnOnce()) {
        let mut known = self.0.known.lock().unwrap_or_else(PoisonError::into_inner);
        publish();
        let servers = self.servers();
        known.extend(
            self.catalog_of(&servers)
                .into_iter()
                .filter_map(|(name, tools)| {
                    let [(server, tool)] = tools.as_slice() else {
                        return None;
                    };
                    let owner = servers.get(server)?.cache_owner();
                    Some((
                        name,
                        KnownTool {
                            owner,
                            original: tool.name.clone(),
                        },
                    ))
                }),
        );
    }

    fn servers(&self) -> MutexGuard<'_, BTreeMap<String, Arc<Server>>> {
        self.0
            .servers
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
    }

    pub fn with_forms(self, forms: bool) -> Self {
        self.0.forms.store(forms, Ordering::Release);
        self
    }

    pub fn with_config(self, path: PathBuf, shown: crate::endpoints::PathShown) -> Self {
        *self.0.config.lock().unwrap_or_else(PoisonError::into_inner) = Some((path, shown));
        self
    }

    pub fn with_store(self, store: crate::secrets::StoreKind) -> Self {
        *self.0.store.lock().unwrap_or_else(PoisonError::into_inner) = Some(store);
        self
    }

    fn credentials(&self) -> (Option<PathBuf>, crate::secrets::StoreKind) {
        (
            self.0
                .config
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .as_ref()
                .map(|(path, _)| path.clone()),
            self.0
                .store
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .unwrap_or(crate::secrets::StoreKind::File),
        )
    }

    fn credentials_for(&self, server: &Server) -> (Option<PathBuf>, crate::secrets::StoreKind) {
        let (config, store) = self.credentials();
        (
            server
                .owner
                .as_ref()
                .map_or(config, |owner| owner.config.clone()),
            store,
        )
    }

    fn server(&self, name: &str) -> Result<Arc<Server>, String> {
        self.servers()
            .get(name)
            .cloned()
            .ok_or_else(|| text::removed(name))
    }

    fn watching(&self, watch: &Arc<Watch>) -> bool {
        self.servers()
            .get(&watch.name)
            .is_some_and(|server| Arc::ptr_eq(&server.watch, watch))
    }

    pub(crate) fn server_config(&self, name: &str) -> Result<McpServer, String> {
        Ok(self.server(name)?.config.clone())
    }

    pub fn definitions(&self) -> Vec<wisp_plugin_api::mcp::Tool> {
        self.available()
            .into_iter()
            .map(|(server, original, spec)| wisp_plugin_api::mcp::Tool {
                name: spec.name,
                server,
                original,
                description: spec.description,
                input_schema: spec.input_schema.to_string(),
            })
            .collect()
    }

    pub async fn add(&self, name: &str, config: McpServer) -> Result<u32, String> {
        self.add_by(name, config, None).await
    }

    pub(crate) async fn add_by(
        &self,
        name: &str,
        config: McpServer,
        by: Option<&str>,
    ) -> Result<u32, String> {
        self.add_keys_by(name, config, BTreeMap::new(), by, std::future::pending())
            .await
            .map(|result| result.tools)
    }

    pub async fn add_with_keys(
        &self,
        name: &str,
        config: McpServer,
        typed: BTreeMap<String, wisp_protocol::Secret>,
    ) -> Result<KeyResult, String> {
        self.add_keys_by(name, config, typed, None, std::future::pending())
            .await
    }

    fn staged(
        &self,
        name: &str,
        config: &McpServer,
        owner: Option<Arc<keys::RunKeyOwner>>,
        keys: BTreeMap<String, wisp_protocol::Secret>,
    ) -> Self {
        let staged = Self::build(
            BTreeMap::from([(name.into(), config.clone())]),
            self.0.cwd.clone(),
            None,
            self.0.closed.clone(),
            self.1.clone(),
            |_, _| owner.clone(),
        )
        .with_forms(self.0.forms.load(Ordering::Acquire));
        if let Ok(server) = staged.server(name) {
            *server.keys.lock().unwrap_or_else(PoisonError::into_inner) = keys;
        }
        staged
    }

    pub(crate) async fn add_keys_by(
        &self,
        name: &str,
        config: McpServer,
        typed: BTreeMap<String, wisp_protocol::Secret>,
        by: Option<&str>,
        cancelled: impl Future<Output = ()> + Send,
    ) -> Result<KeyResult, String> {
        check_add(name, &config.command, &config.args)?;
        if config
            .keys
            .iter()
            .chain(typed.keys())
            .any(|key| !wisp_protocol::plain_variable(key))
        {
            return Err(text::KEY_VARIABLE.into());
        }
        if !config.keys.is_empty() && is_address(&config.command) {
            return Err(text::ADDRESS_KEYS.into());
        }
        let mutation = self.0.mutation.clone().lock_owned().await;
        if self
            .servers()
            .keys()
            .any(|other| tool_name(other, "") == tool_name(name, ""))
        {
            return Err(text::NAME_EXISTS.into());
        }
        let mut found = self.keys_for(name, &config, None).await?;
        found.extend(typed.clone());
        let staged = self.staged(name, &config, None, found);
        let started = tokio::select! {
            biased;
            () = cancelled => return Err(wisp_tool_text::manage::NO_ANSWER.into()),
            result = staged.connect(name) => result,
        };
        if let Err(error) = started
            && !matches!(
                staged.rows().first().map(|row| &row.state),
                Some(State::NeedsSignIn)
            )
        {
            return Err(error);
        }
        let service = self.clone();
        let name = name.to_owned();
        let by = by.map(str::to_owned);
        let keyed = !config.keys.is_empty();
        let finish = async move {
            let _mutation = mutation;
            service
                .finish_server(&name, staged, typed, by, |config| {
                    service.persist(&name, Edit::Add(config), None)
                })
                .await
        };
        if keyed {
            tokio::spawn(finish)
                .await
                .map_err(|_| "the server could not be added".to_owned())?
        } else {
            finish.await
        }
    }

    async fn finish_server(
        &self,
        name: &str,
        staged: Self,
        mut typed: BTreeMap<String, wisp_protocol::Secret>,
        by: Option<String>,
        persist: impl FnOnce(&McpServer) -> Result<(), String> + Send,
    ) -> Result<KeyResult, String> {
        let server = staged
            .servers()
            .remove(name)
            .ok_or("the server could not be added")?;
        let mut server = Arc::try_unwrap(server).map_err(|_| "the server is still starting")?;
        server.saved = saved::Saved::new(
            self.0.folder.as_deref(),
            name,
            &server.config,
            server.owner.as_deref(),
        );
        let (list, listed) = {
            let snapshot = server.snapshot();
            (snapshot.list.clone(), snapshot.listed)
        };
        let watch = server.watch.clone();
        let old = self.server(name).ok();
        let old_connection = match &old {
            Some(old) => Some(old.connection.lock().await),
            None => None,
        };
        let mut redactions = self.live_redactions(&server);
        if let Some(old) = old
            .as_ref()
            .filter(|old| old.saved.key() == server.saved.key())
        {
            redactions = redactions.union([&*old.redactions()]);
        }
        if let Some(list) = &list {
            self.check_overlaps(name, list)?;
            list.check_secrets(&redactions)?;
        }
        let cache = match &list {
            Some(list) => Some((server.saved.edit().await?, list)),
            None => None,
        };
        let run_kept = if let Some(owner) = &server.owner {
            let cache = cache.map(|(edit, list)| (edit, list.clone()));
            Some(
                self.commit_run_keys(
                    name,
                    &server.config,
                    owner.clone(),
                    std::mem::take(&mut typed),
                    cache,
                    redactions,
                )
                .await?,
            )
        } else {
            let save = || persist(&server.config);
            match cache {
                Some((cache, list)) => cache.commit(list, &redactions, save)?,
                None => save()?,
            }
            None
        };
        let count =
            u32::try_from(list.as_ref().map_or(0, |list| list.tools.len())).unwrap_or(u32::MAX);
        let config = server.config.clone();
        if let Some(old) = &old {
            self.retire_redactions(old);
        }
        self.remember_names(|| {
            let mut servers = self.servers();
            servers.insert(name.into(), Arc::new(server));
            self.0.changes.fetch_add(1, Ordering::AcqRel);
        });
        if let Some(mut connection) = old_connection {
            *connection = None;
        }
        let seen = watch.bind(Arc::downgrade(&self.0));
        if seen != 0 && listed != Some(seen) {
            watch.notice();
        }
        self.0.revision.fetch_add(1, Ordering::AcqRel);
        if let Some(by) = &by {
            self.note(Note::Changed {
                by: by.clone(),
                name: name.into(),
                added: true,
            });
            if self
                .rows()
                .iter()
                .any(|row| row.name == name && row.state == State::NeedsSignIn)
            {
                self.note(Note::SignIn(name.into()));
            }
        }
        let kept = match run_kept {
            Some(kept) => kept,
            None => self.keep_keys(name.into(), config, typed).await,
        };
        Ok(KeyResult { tools: count, kept })
    }

    pub async fn remove(&self, name: &str) -> Result<(), String> {
        self.remove_by(name, None).await
    }

    pub(crate) async fn remove_by(&self, name: &str, by: Option<&str>) -> Result<(), String> {
        let _mutation = self.0.mutation.lock().await;
        let server = self.server(name)?;
        *server.pending.lock().await = None;
        self.0
            .awaited
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .remove(name);
        let mut connection = server.connection.lock().await;
        let run = server.owner.is_some();
        if run {
            self.unregister(name, &server, by);
            *connection = None;
        }
        self.erase_server(name, &server).await?;
        if !run {
            self.unregister(name, &server, by);
            *connection = None;
        }
        Ok(())
    }

    async fn erase_server(&self, name: &str, server: &Server) -> Result<(), String> {
        if let Some(owner) = &server.owner {
            owner.check_paths()?;
        }
        let cache = server.saved.edit().await?;
        let (config, _) = self.credentials_for(server);
        let Some(config) = config else {
            if let Some(owner) = &server.owner {
                owner.check_paths()?;
            }
            return cache.remove().map_err(str::to_owned);
        };
        let path = sign_in::profile(&config, name, server.owner.as_deref())
            .map_err(|_| sign_in::STORE_ERROR)?;
        let locks = {
            let (config, path) = (config.clone(), path.clone());
            tokio::task::spawn_blocking(move || {
                crate::oauth::lock_profile_with_config(&path, Some(&config))
            })
            .await
            .map_err(|_| sign_in::STORE_ERROR)?
            .map_err(|_| sign_in::STORE_ERROR)?
        };
        let http = is_address(&server.config.command);
        if let Some(owner) = &server.owner {
            owner.check_paths()?;
            cache.remove()?;
            if http {
                crate::oauth::clear_value_locked(&path, &locks)
                    .map_err(|_| sign_in::STORE_ERROR)?;
            } else {
                owner.clear(&config, &locks)?;
            }
            return Ok(());
        }
        cache.remove()?;
        keys::clear(&config, name, &locks)?;
        crate::oauth::clear_value_locked(&path, &locks).map_err(|_| sign_in::STORE_ERROR)?;
        if http {
            let legacy = crate::oauth::mcp_profile(&config, &server.config.command)
                .map_err(|_| sign_in::STORE_ERROR)?;
            crate::oauth::clear_value_locked(&legacy, &locks).map_err(|_| sign_in::STORE_ERROR)?;
        }
        self.persist(name, Edit::Remove, Some(&locks))
    }

    fn retire_redactions(&self, server: &Server) {
        let mut retired = self
            .0
            .retired_redactions
            .lock()
            .unwrap_or_else(PoisonError::into_inner);
        let redactions = server.redactions();
        if !redactions.is_empty() {
            let entry = retired.entry(server.cache_owner()).or_default();
            *entry = entry.union([&*redactions]);
        }
    }

    fn unregister(&self, name: &str, server: &Server, by: Option<&str>) {
        self.retire_redactions(server);
        self.servers().remove(name);
        self.0.revision.fetch_add(1, Ordering::AcqRel);
        if let Some(by) = by {
            self.note(Note::Changed {
                by: by.into(),
                name: name.into(),
                added: false,
            });
        }
    }

    fn persist(
        &self,
        name: &str,
        edit: Edit<'_>,
        locks: Option<&crate::secrets::Locks>,
    ) -> Result<(), String> {
        let config = self
            .0
            .config
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .clone();
        let Some((path, shown)) = config else {
            return Ok(());
        };
        let folder = crate::config_file::lock_folder(&path);
        let lock_error = "could not lock config.toml";
        let read_error = "could not read config.toml";
        let _lock = if let Some(locks) = locks {
            if !locks.covers(&folder) {
                return Err(lock_error.into());
            }
            None
        } else {
            Some(
                crate::secrets::lock_noting(
                    &[(folder.as_path(), crate::secrets::LockKind::Config)],
                    shown,
                    Duration::from_secs(5),
                )
                .map_err(|_| lock_error)?,
            )
        };
        let mut text =
            crate::config_file::ConfigText::read(&path, shown).map_err(|_| read_error)?;
        match edit {
            Edit::Add(server) => {
                if text
                    .current()
                    .map_err(|_| read_error)?
                    .mcp_servers
                    .contains_key(name)
                {
                    return Err(text::NAME_EXISTS.into());
                }
                if !text.set_mcp_server(name, server) {
                    return Err("could not add the server to config.toml".into());
                }
            }
            Edit::Remove => {
                text.remove_mcp_server(name);
            }
            Edit::Keys(keys) => {
                if !text.add_mcp_keys(name, keys) {
                    return Err("could not set the server's keys in config.toml".into());
                }
            }
        }
        text.write().map_err(|_| "could not write config.toml")?;
        Ok(())
    }

    pub(crate) fn note(&self, note: Note) {
        self.0
            .notes
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .push(note);
    }

    pub fn await_sign_in(&self, name: &str) {
        self.0
            .awaited
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .insert(name.into());
    }

    fn ended(&self, name: &str, line: String) {
        let awaited = self
            .0
            .awaited
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .remove(name);
        self.0
            .ended
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .push(SignInEnd { line, awaited });
        self.0.ended_signal.notify_one();
    }

    pub fn take_ended(&self) -> Vec<SignInEnd> {
        std::mem::take(&mut *self.0.ended.lock().unwrap_or_else(PoisonError::into_inner))
    }

    pub(crate) fn ended_signal(&self) -> Arc<Notify> {
        self.0.ended_signal.clone()
    }

    pub(crate) fn take_notes(&self) -> Vec<Note> {
        std::mem::take(&mut *self.0.notes.lock().unwrap_or_else(PoisonError::into_inner))
    }

    pub fn turned_off(&self, names: BTreeSet<String>) {
        let mut off = self.0.off.lock().unwrap_or_else(PoisonError::into_inner);
        if *off != names {
            *off = names;
            self.0.revision.fetch_add(1, Ordering::AcqRel);
            self.0.changes.fetch_add(1, Ordering::AcqRel);
        }
    }

    fn is_off(&self, name: &str) -> bool {
        self.0
            .off
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .contains(name)
    }

    pub(crate) fn tools_turned_off(&self, names: &BTreeSet<String>) -> BTreeSet<String> {
        self.tools_of(names).into_keys().collect()
    }

    fn tools_of(&self, servers: &BTreeSet<String>) -> BTreeMap<String, String> {
        self.0
            .known
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .iter()
            .filter(|(name, known)| {
                servers.contains(&known.owner.0) && self.known_name_visible(name, known)
            })
            .map(|(name, known)| (name.clone(), known.owner.0.clone()))
            .collect()
    }

    pub(crate) fn disabled_entries(&self) -> BTreeMap<String, String> {
        let off = self
            .0
            .off
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .clone();
        self.entries_of(&off)
    }

    pub(crate) fn entries_of(&self, servers: &BTreeSet<String>) -> BTreeMap<String, String> {
        let mut entries = self.tools_of(servers);
        for (server, state) in self
            .servers()
            .iter()
            .filter(|(server, _)| servers.contains(*server))
        {
            if let Some(list) = self.visible_list(state, &mut state.snapshot()) {
                entries.extend(
                    list.prompts
                        .iter()
                        .map(|prompt| (prompt_name(server, &prompt.name), server.clone())),
                );
                entries.extend(
                    list.resources
                        .iter()
                        .map(|resource| (resource_name(server, &resource.uri), server.clone())),
                );
            }
        }
        entries
    }

    pub(crate) fn owns_tool(&self, server: &str, name: &str) -> bool {
        self.0
            .known
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .get(name)
            .is_some_and(|known| known.owner.0 == server)
    }

    pub fn revision(&self) -> u64 {
        self.0.revision.load(Ordering::Acquire)
    }

    pub(crate) fn changes(&self) -> u64 {
        self.0.changes.load(Ordering::Acquire)
    }

    pub fn rows(&self) -> Vec<Row> {
        self.servers()
            .iter()
            .map(|(name, server)| {
                let mut snapshot = server.snapshot();
                let tools = self
                    .visible_list(server, &mut snapshot)
                    .map_or(0, |list| list.tools.len());
                Row {
                    name: name.clone(),
                    off: self.is_off(name),
                    state: if !matches!(snapshot.state, State::Failed(_))
                        && server
                            .challenge
                            .lock()
                            .unwrap_or_else(PoisonError::into_inner)
                            .is_some()
                    {
                        State::NeedsSignIn
                    } else {
                        snapshot.state.clone()
                    },
                    tools,
                    fresh: snapshot.listed == Some(server.watch.notices().seen),
                }
            })
            .collect()
    }

    pub fn saved(&self, name: &str) -> Option<List> {
        let server = self.server(name).ok()?;
        self.visible_list(&server, &mut server.snapshot()).cloned()
    }

    #[cfg(any(test, feature = "test-support"))]
    pub async fn seed_saved_tools(&self, name: &str, tools: Vec<ToolSpec>) -> Result<(), String> {
        let server = self.server(name)?;
        let redactions = server.redactions().clone();
        server
            .saved
            .write(&List::new(tools, Vec::new(), Vec::new(), None), &redactions)
            .await
            .map_err(str::to_owned)
    }

    fn catalog(&self) -> BTreeMap<String, Vec<(String, ToolSpec)>> {
        self.catalog_of(&self.servers())
    }

    fn catalog_of(
        &self,
        servers: &BTreeMap<String, Arc<Server>>,
    ) -> BTreeMap<String, Vec<(String, ToolSpec)>> {
        let mut catalog = BTreeMap::<String, Vec<(String, ToolSpec)>>::new();
        for (name, server) in servers {
            let mut snapshot = server.snapshot();
            let Some(list) = self.visible_list(server, &mut snapshot) else {
                continue;
            };
            for tool in &list.tools {
                catalog
                    .entry(tool_name(name, &tool.name))
                    .or_default()
                    .push((name.clone(), tool.clone()));
            }
        }
        catalog
    }

    pub fn tools(&self) -> Vec<ToolSpec> {
        self.available()
            .into_iter()
            .map(|(_, _, spec)| spec)
            .collect()
    }

    fn available(&self) -> Vec<(String, String, ToolSpec)> {
        self.catalog()
            .into_iter()
            .filter_map(|(name, tools)| match tools.as_slice() {
                [(server, tool)] => Some((
                    server.clone(),
                    tool.name.clone(),
                    ToolSpec {
                        name,
                        ..tool.clone()
                    },
                )),
                _ => {
                    for (server_name, _) in tools {
                        if let Some(server) = self.servers().get(&server_name) {
                            server.snapshot().state =
                                State::Failed(format!("{server_name} {NAME_OVERLAP}"));
                        }
                    }
                    None
                }
            })
            .collect()
    }

    pub async fn connect(&self, name: &str) -> Result<(), String> {
        self.ready(name).await.map(|_| ())
    }

    async fn ready(&self, name: &str) -> Result<Peer<RoleClient>, String> {
        loop {
            if self.is_off(name) {
                return Err(wisp_tool_text::manage::turned_off(name));
            }
            let server = self.server(name)?;
            let mut connection = server.connection.lock().await;
            if self.watching(&server.watch) {
                return self.ready_on(name, &server, &mut connection).await;
            }
        }
    }

    async fn ready_on(
        &self,
        name: &str,
        server: &Server,
        connection: &mut Option<Connection>,
    ) -> Result<Peer<RoleClient>, String> {
        if !server.config.keys.is_empty() && is_address(&server.config.command) {
            return Err(text::ADDRESS_KEYS.into());
        }
        if let Some(reason) = server.name_problem {
            return Err(format!("{name} {reason}"));
        }
        if server
            .challenge
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .is_some()
        {
            *connection = None;
            server.snapshot().state = State::NeedsSignIn;
        }

        if let Some(connected) = connection.as_ref().filter(|c| c.is_live()) {
            return Ok(connected.running.peer().clone());
        }
        *connection = None;
        if let Some(owner) = &server.owner {
            owner.check_paths()?;
        }
        let mut found = self
            .keys_for(name, &server.config, server.owner.clone())
            .await?;
        found.extend(
            server
                .keys
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .clone(),
        );
        let values: Vec<_> = found
            .values()
            .map(|value| value.expose().to_owned())
            .collect();
        crate::session::register_secrets(values.iter().cloned());
        *server.redactions() = Redactions::new(values);
        self.evict_catalog(server).await?;
        let missing = keys::missing(&server.config, &found);
        if !missing.is_empty() {
            server.snapshot().state = State::NeedsKey(missing.clone());
            return Err(text::needs_key(name, &missing));
        }
        server.snapshot().state = State::Starting;
        let _starting = Starting(server);
        let result = async {
            let started = tokio::time::Instant::now();
            let connected = start_within(START_LIMIT, async {
                if is_address(&server.config.command) {
                    self.start_http(name, server).await
                } else {
                    start(
                        &server.config,
                        server
                            .owner
                            .as_ref()
                            .map_or(self.0.cwd.as_path(), |owner| owner.cwd.as_path()),
                        &found,
                        self.0.forms.load(Ordering::Acquire),
                        server.flights.clone(),
                        server.watch.clone(),
                    )
                    .await
                }
            })
            .await?;
            let remaining = START_LIMIT.saturating_sub(started.elapsed());
            let peer = connected.running.peer();
            listen(peer, &server.watch, &self.0.closed).await;
            start_within(remaining, self.refresh(name, server, peer)).await?;
            Ok::<_, String>(connected)
        }
        .await;
        match result {
            Ok(connected) => {
                let peer = connected.running.peer().clone();
                *connection = Some(connected);
                server.snapshot().state = State::Ready;
                Ok(peer)
            }
            Err(reason) if reason == SIGN_IN_REQUIRED => {
                server.snapshot().state = State::NeedsSignIn;
                Err(format!("{name} {SIGN_IN_REQUIRED}"))
            }
            Err(reason) => {
                let message = format!("{name} couldn't start: {reason}");
                server.snapshot().state = State::Failed(message.clone());
                Err(message)
            }
        }
    }

    async fn relist(&self, watch: Arc<Watch>) {
        while let Ok(server) = self.server(&watch.name) {
            let connection = server.connection.lock().await;
            if !self.watching(&watch) {
                break;
            }
            let Some(connected) = connection.as_ref().filter(|c| c.is_live()) else {
                break;
            };
            let seen = watch.notices().seen;
            let _ = tokio::time::timeout(
                START_LIMIT,
                self.refresh_at(&watch.name, &server, connected.running.peer(), seen),
            )
            .await;
            if !connected.is_live() {
                break;
            }
            if watch.settle(Some(seen)) {
                return;
            }
        }
        watch.settle(None);
    }

    fn check_overlaps(&self, name: &str, list: &List) -> Result<(), String> {
        let catalog = self.catalog();
        if list.tools.iter().any(|tool| {
            catalog
                .get(&tool_name(name, &tool.name))
                .is_some_and(|entries| entries.iter().any(|(other, _)| other != name))
        }) {
            return Err(NAME_OVERLAP.into());
        }
        Ok(())
    }

    async fn refresh(
        &self,
        name: &str,
        server: &Server,
        peer: &Peer<RoleClient>,
    ) -> Result<List, String> {
        let seen = server.watch.notices().seen;
        self.refresh_at(name, server, peer, seen).await
    }

    async fn refresh_at(
        &self,
        name: &str,
        server: &Server,
        peer: &Peer<RoleClient>,
        seen: u64,
    ) -> Result<List, String> {
        peer.clear_response_cache().await;
        let (list, complete) = fetch_list(peer).await?;
        check_names(name, &list.tools)?;
        self.check_overlaps(name, &list)?;
        let redactions = self.live_redactions(server);
        list.check_secrets(&redactions)?;
        if complete {
            server.saved.write(&list, &redactions).await?;
        }
        list.check_secrets(&redactions)?;
        self.remember(server, list.clone(), seen);
        Ok(list)
    }

    async fn start_http(&self, name: &str, server: &Server) -> Result<Connection, String> {
        *server
            .challenge
            .lock()
            .unwrap_or_else(PoisonError::into_inner) = None;
        let result = start_http(
            &server.config.command,
            None,
            server.challenge.clone(),
            self.0.forms.load(Ordering::Acquire),
            server.flights.clone(),
            server.watch.clone(),
        )
        .await;
        let challenge = server
            .challenge
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .clone();
        if result.is_ok() || challenge.is_none() {
            return result;
        }
        let (config, store) = self.credentials_for(server);
        let manager = sign_in::restored(
            name,
            &server.config.command,
            config,
            server.owner.clone(),
            store,
            challenge.as_deref(),
        )
        .await
        .ok()
        .flatten();
        self.evict_catalog(server).await?;
        let manager = manager.ok_or(SIGN_IN_REQUIRED)?;
        *server
            .challenge
            .lock()
            .unwrap_or_else(PoisonError::into_inner) = None;
        let result = start_http(
            &server.config.command,
            Some(manager),
            server.challenge.clone(),
            self.0.forms.load(Ordering::Acquire),
            server.flights.clone(),
            server.watch.clone(),
        )
        .await;
        if server
            .challenge
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .is_some()
        {
            return Err(SIGN_IN_REQUIRED.into());
        }
        result
    }

    pub async fn sign_in_start(&self, name: &str) -> Result<Option<String>, String> {
        let server = self.server(name)?;
        if !is_address(&server.config.command) {
            return Err(format!("{name} doesn't use a sign-in"));
        }
        if !sign_in::permitted(&server.config.command) {
            return Err(sign_in::INSECURE.into());
        }
        let mut pending = server.pending.lock().await;
        if let Some(pending) = pending
            .as_ref()
            .filter(|pending| pending.return_to.check().is_ok())
        {
            return Ok(Some(pending.link.expose().to_owned()));
        }
        *pending = None;
        if self.connect(name).await.is_ok() {
            return Ok(None);
        }
        if server.snapshot().state != State::NeedsSignIn {
            return Err(format!("couldn't reach {name}"));
        }
        let (config, store) = self.credentials_for(&server);
        let challenge = server
            .challenge
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .clone();
        let started = tokio::time::timeout(
            START_LIMIT,
            sign_in::start(
                name,
                &server.config.command,
                config,
                server.owner.clone(),
                store,
                challenge.as_deref(),
            ),
        )
        .await
        .map_err(|_| sign_in::START_ERROR)??;
        if started.return_to.uses_loopback() {
            let service = Arc::downgrade(&self.0);
            let weak = Arc::downgrade(&started);
            let return_to = started.return_to.clone();
            let name = name.to_owned();
            let task = tokio::spawn(async move {
                let received = return_to.receive().await;
                if let (Some(service), Some(pending)) = (service.upgrade(), weak.upgrade()) {
                    let service = Service(service, None);
                    match received {
                        Ok(returned) => {
                            let result = service.complete_sign_in(&name, &pending, &returned).await;
                            let page = if result.is_ok() {
                                format!("Signed in to {name}. You can close this page.")
                            } else {
                                "Couldn't finish the sign-in. Return to Wisp.".into()
                            };
                            returned.respond(&page).await;
                        }
                        Err(error) => service.refused_sign_in(&name, &pending, &error).await,
                    }
                }
            });
            *started.task.lock().unwrap_or_else(PoisonError::into_inner) =
                Some(task.abort_handle());
        }
        let link = started.link.expose().to_owned();
        *pending = Some(started);
        Ok(Some(link))
    }

    pub async fn sign_in_finish(&self, name: &str, address: &str) -> Result<u32, String> {
        let server = self.server(name)?;
        let pending = server
            .pending
            .lock()
            .await
            .clone()
            .ok_or_else(|| sign_in::not_started(name))?;
        let returned = match pending.return_to.pasted(address) {
            Ok(returned) => returned,
            Err(error) => {
                self.refused_sign_in(name, &pending, &error).await;
                return Err(sign_in::message(error));
            }
        };
        self.complete_sign_in(name, &pending, &returned).await
    }

    async fn refused_sign_in(
        &self,
        name: &str,
        pending: &Arc<sign_in::Pending>,
        error: &crate::oauth::AuthError,
    ) {
        use crate::oauth::AuthError;

        if !matches!(
            error,
            AuthError::Declined | AuthError::Expired | AuthError::Callback
        ) {
            return;
        }
        let _mutation = self.0.mutation.lock().await;
        if let Ok(server) = self.server(name) {
            let mut current = server.pending.lock().await;
            if current
                .as_ref()
                .is_some_and(|current| Arc::ptr_eq(current, pending))
            {
                *current = None;
                if matches!(error, AuthError::Declined) {
                    self.ended(
                        name,
                        text::not_signed_in(name, &sign_in::message(AuthError::Declined)),
                    );
                }
            }
        }
    }

    async fn complete_sign_in(
        &self,
        name: &str,
        pending: &Arc<sign_in::Pending>,
        returned: &crate::oauth::Returned,
    ) -> Result<u32, String> {
        let _mutation = self.0.mutation.lock().await;
        let server = self.server(name)?;
        if !server
            .pending
            .lock()
            .await
            .as_ref()
            .is_some_and(|current| Arc::ptr_eq(current, pending))
        {
            return Err(sign_in::not_started(name));
        }
        let result = pending.finish(returned).await;
        *server.pending.lock().await = None;
        result.inspect_err(|reason| self.ended(name, text::not_signed_in(name, reason)))?;
        self.connect(name)
            .await
            .inspect_err(|line| self.ended(name, line.clone()))?;
        let count = self
            .visible_list(&server, &mut server.snapshot())
            .map_or(0, |list| list.tools.len());
        *server.connection.lock().await = None;
        server.snapshot().state = State::Stopped;
        let count = u32::try_from(count).unwrap_or(u32::MAX);
        self.ended(name, text::signed_in(name, count));
        Ok(count)
    }

    fn remember(&self, server: &Server, list: List, seen: u64) {
        self.remember_names(|| {
            let mut snapshot = server.snapshot();
            if snapshot.list.as_ref().is_none_or(|old| list.differs(old)) {
                self.0.revision.fetch_add(1, Ordering::AcqRel);
            }
            snapshot.list = Some(list);
            snapshot.listed = Some(seen);
            self.0.changes.fetch_add(1, Ordering::AcqRel);
        });
    }

    pub fn prompts(&self) -> Vec<Prompt> {
        self.servers()
            .iter()
            .filter(|(server, _)| !self.is_off(server))
            .flat_map(|(server, state)| {
                self.visible_list(state, &mut state.snapshot())
                    .into_iter()
                    .flat_map(|list| &list.prompts)
                    .map(|prompt| Prompt {
                        name: prompt_name(server, &prompt.name),
                        server: server.clone(),
                        original: prompt.name.clone(),
                        description: prompt.description.clone(),
                        arguments: prompt.arguments.clone(),
                    })
                    .collect::<Vec<_>>()
            })
            .collect()
    }

    pub fn resources(&self) -> Vec<Resource> {
        self.servers()
            .iter()
            .filter(|(server, _)| !self.is_off(server))
            .flat_map(|(server, state)| {
                self.visible_list(state, &mut state.snapshot())
                    .into_iter()
                    .flat_map(|list| &list.resources)
                    .map(|resource| Resource {
                        name: resource_name(server, &resource.uri),
                        server: server.clone(),
                        uri: resource.uri.clone(),
                        title: resource.title.clone(),
                        description: resource.description.clone(),
                    })
                    .collect::<Vec<_>>()
            })
            .collect()
    }

    pub async fn get_prompt(&self, name: &str) -> Result<Vec<PromptMessage>, String> {
        let prompt = self
            .prompts()
            .into_iter()
            .find(|prompt| prompt.name == name)
            .ok_or_else(|| text::unavailable(name))?;
        let peer = self.ready(&prompt.server).await?;
        let server = self.server(&prompt.server)?;
        if !self
            .prompts()
            .iter()
            .any(|current| current.name == name && current.original == prompt.original)
        {
            return Err(self.gone(&server, &prompt.original));
        }
        match peer
            .get_prompt_once(GetPromptRequestParams::new(prompt.original))
            .await
        {
            Ok(GetPromptResponse::Complete(result)) => Ok(result
                .messages
                .into_iter()
                .map(|message| PromptMessage {
                    role: match message.role {
                        rmcp::model::Role::User => "user",
                        rmcp::model::Role::Assistant => "assistant",
                    }
                    .into(),
                    text: block_text(&message.content).map(|text| server.redactions().scrub(text)),
                })
                .collect()),
            Ok(GetPromptResponse::InputRequired(_)) => Err(text::INPUT_REQUIRED.into()),
            _ => Err(text::read_failed(&prompt.server, "prompt")),
        }
    }

    pub async fn read_resource(&self, name: &str) -> Result<Vec<Option<String>>, String> {
        let resource = self
            .resources()
            .into_iter()
            .find(|resource| resource.name == name)
            .ok_or_else(|| text::unavailable(name))?;
        let peer = self.ready(&resource.server).await?;
        let server = self.server(&resource.server)?;
        if !self
            .resources()
            .iter()
            .any(|current| current.name == name && current.uri == resource.uri)
        {
            return Err(self.gone(&server, &resource.uri));
        }
        match peer
            .read_resource_once(ReadResourceRequestParams::new(resource.uri))
            .await
        {
            Ok(ReadResourceResponse::Complete(result)) => Ok(result
                .contents
                .iter()
                .map(|part| resource_text(part).map(|text| server.redactions().scrub(text)))
                .collect()),
            Ok(ReadResourceResponse::InputRequired(_)) => Err(text::INPUT_REQUIRED.into()),
            _ => Err(text::read_failed(&resource.server, "resource")),
        }
    }

    pub async fn list(&self, name: &str) -> Result<List, String> {
        self.connect(name).await?;
        self.saved(name)
            .ok_or_else(|| format!("{name} has no saved tool list"))
    }

    pub async fn call(
        &self,
        name: &str,
        input: serde_json::Value,
        limit: Duration,
        cancel: CancellationToken,
    ) -> Result<ToolRun, String> {
        self.call_asking(
            name,
            input,
            limit,
            cancel,
            crate::call::user::UserChannel::none(),
        )
        .await
    }

    pub async fn call_asking(
        &self,
        name: &str,
        input: serde_json::Value,
        limit: Duration,
        cancel: CancellationToken,
        user: crate::call::user::UserChannel,
    ) -> Result<ToolRun, String> {
        let (server_name, original) = self.resolve(name)?;
        let peer = tokio::select! {
            result = self.ready(&server_name) => result?,
            _ = cancel.cancelled() => return Err(text::CALL_CANCELLED.into()),
        };
        let server = self.server(&server_name)?;
        if self.catalog().get(name).is_none_or(|entries| !matches!(entries.as_slice(), [(server, tool)] if server == &server_name && tool.name == original)) {
            return Err(self.gone(&server, &original));
        }
        let Some(arguments) = input.as_object() else {
            return Err("MCP tool arguments must be an object".into());
        };
        let redactions = server.redactions().clone();
        let registered = elicit::Flight::register(
            server.flights.clone(),
            server_name.clone(),
            user,
            cancel.clone(),
            redactions,
        );
        let flight = &registered.0;
        let request = async {
            let mut responses = None;
            let mut request_state = None;
            let mut backoff = Duration::from_millis(50);
            let mut asked = false;
            for _ in 0..rmcp::model::DEFAULT_MRTR_MAX_ROUNDS {
                if cancel.is_cancelled() {
                    return Err(text::CALL_CANCELLED.into());
                }
                let mut params =
                    CallToolRequestParams::new(original.clone()).with_arguments(arguments.clone());
                params.input_responses = responses.take();
                params.request_state = request_state.take();
                let result = peer.call_tool_once(params).await;
                if let Some(failure) = flight
                    .failure
                    .lock()
                    .unwrap_or_else(PoisonError::into_inner)
                    .take()
                {
                    return Err(failure);
                }
                match result {
                    Ok(rmcp::model::CallToolResponse::Complete(result)) => {
                        let secret = flight.received_secret.load(Ordering::Acquire);
                        return content(
                            result,
                            secret.then_some(server_name.as_str()),
                            &flight
                                .redactions
                                .lock()
                                .unwrap_or_else(PoisonError::into_inner),
                        );
                    }
                    Ok(rmcp::model::CallToolResponse::InputRequired(result)) => {
                        asked = true;
                        let mut answers = BTreeMap::new();
                        for (id, request) in result.input_requests.unwrap_or_default() {
                            let rmcp::model::InputRequest::Elicitation(request) = request else {
                                return Err(text::INPUT_REQUIRED.into());
                            };
                            let answer = elicit::answer(flight, request.params).await?;
                            if answer.action == rmcp::model::ElicitationAction::Cancel {
                                return Err(text::CALL_CANCELLED.into());
                            }
                            answers.insert(
                                id,
                                serde_json::to_value(answer)
                                    .map_err(|_| "the MCP answer could not be sent")?,
                            );
                        }
                        if answers.is_empty() {
                            tokio::time::sleep(backoff).await;
                            backoff = (backoff * 2).min(Duration::from_millis(250));
                        } else {
                            responses = Some(answers);
                        }
                        request_state = result.request_state;
                    }
                    Err(ServiceError::McpError(error))
                        if error.code.0 == -32602
                            && !asked
                            && flight.rounds.load(Ordering::Acquire) == 0 =>
                    {
                        let server = self.server(&server_name)?;
                        let schema = self
                            .refresh(&server_name, &server, &peer)
                            .await?
                            .tools
                            .into_iter()
                            .find(|t| t.name == original)
                            .map(|t| t.input_schema)
                            .unwrap_or_default();
                        return Err(format!(
                            "{server_name} refused those arguments; current parameters: {schema}"
                        ));
                    }
                    _ => break,
                }
            }
            Err(text::call_incomplete(&server_name))
        };
        let result = tokio::select! {
            biased;
            _ = cancel.cancelled() => Err(text::CALL_CANCELLED.into()),
            () = flight.timed_out(limit) => Err("the MCP call timed out".into()),
            result = request => result,
        }
        .map(|mut run| {
            if let Some(result) = &mut run.result {
                for note in flight
                    .notes
                    .lock()
                    .unwrap_or_else(PoisonError::into_inner)
                    .iter()
                {
                    if !result.is_empty() {
                        result.push('\n');
                    }
                    result.push_str(note);
                }
                *result = flight.scrub(result);
            }
            run
        })
        .map_err(|error| flight.scrub(&error));
        let server = self.server(&server_name)?;
        if server
            .challenge
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .is_some()
        {
            *server.connection.lock().await = None;
            server.snapshot().state = State::NeedsSignIn;
        }
        result
    }

    pub(crate) fn entry_name_hidden(&self, name: &str) -> bool {
        name.starts_with(TOOL_PREFIX) && self.index_has_secret(name, "", "", None)
    }

    fn metadata_redactions(&self, owner: &CacheOwner, server: Option<&Server>) -> Redactions {
        let retired = self
            .0
            .retired_redactions
            .lock()
            .unwrap_or_else(PoisonError::into_inner);
        match (retired.get(owner), server) {
            (Some(retired), Some(server)) => retired.union([&*server.redactions()]),
            (None, Some(server)) => server.redactions().clone(),
            (Some(retired), None) => retired.clone(),
            (None, None) => Redactions::default(),
        }
    }

    fn live_redactions(&self, server: &Server) -> Redactions {
        self.metadata_redactions(&server.cache_owner(), Some(server))
    }

    fn visible_list<'a>(&self, server: &Server, snapshot: &'a mut Snapshot) -> Option<&'a List> {
        let redactions = self.live_redactions(server);
        if snapshot.list.as_ref()?.check_secrets(&redactions).is_ok() {
            return snapshot.list.as_ref();
        }
        if let Err(reason) = server.saved.try_evict_if_contaminated(&redactions) {
            self.cache_failed(server, snapshot, reason);
        }
        None
    }

    fn cache_failed(&self, server: &Server, snapshot: &mut Snapshot, reason: &str) -> String {
        let message = format!("{} {reason}", server.watch.name);
        let state = State::Failed(message.clone());
        let changed =
            snapshot.list.is_some() || snapshot.listed.is_some() || snapshot.state != state;
        snapshot.list = None;
        snapshot.listed = None;
        snapshot.state = state;
        if changed {
            self.0.revision.fetch_add(1, Ordering::AcqRel);
            self.0.changes.fetch_add(1, Ordering::AcqRel);
        }
        message
    }

    async fn evict_catalog(&self, server: &Server) -> Result<(), String> {
        server
            .saved
            .evict_if_contaminated(&self.live_redactions(server))
            .await
            .map_err(|reason| self.cache_failed(server, &mut server.snapshot(), reason))
    }

    fn known_live_server(&self, known: &KnownTool) -> Option<Arc<Server>> {
        self.server(&known.owner.0)
            .ok()
            .filter(|server| server.saved.key() == known.owner.1)
    }

    pub(crate) fn index_has_secret(
        &self,
        name: &str,
        summary: &str,
        text: &str,
        schema: Option<&str>,
    ) -> bool {
        let known = self
            .0
            .known
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .get(name)
            .cloned();
        let has_entry_prefix = |server: &str| {
            name.starts_with(&prompt_name(server, ""))
                || name.starts_with(&resource_name(server, ""))
        };
        let server = match &known {
            Some(known) => self.known_live_server(known),
            None => self
                .servers()
                .iter()
                .find(|(server, _)| has_entry_prefix(server))
                .map(|(_, server)| server.clone()),
        };
        let redactions = match (&known, &server) {
            (Some(known), server) => self.metadata_redactions(&known.owner, server.as_deref()),
            (None, Some(server)) => self.live_redactions(server),
            (None, None) => {
                let retired = self
                    .0
                    .retired_redactions
                    .lock()
                    .unwrap_or_else(PoisonError::into_inner);
                Redactions::default().union(retired.iter().filter_map(
                    |((server, _), redactions)| has_entry_prefix(server).then_some(redactions),
                ))
            }
        };
        let held = crate::session::held();
        if held.is_empty() && redactions.is_empty() {
            return false;
        }
        let contains = |text: &str| saved::has_secret(text, &held, &redactions);
        let contaminated = [name, summary, text].into_iter().any(contains)
            || known.is_some_and(|known| contains(&known.original))
            || schema
                .and_then(|schema| serde_json::from_str(schema).ok())
                .is_some_and(|schema| saved::json_has_secret(&schema, &contains));
        if contaminated
            && let Some(server) = server
            && let Err(reason) = server.saved.try_evict_if_contaminated(&redactions)
        {
            self.cache_failed(&server, &mut server.snapshot(), reason);
        }
        contaminated
    }

    fn known_name_visible(&self, name: &str, known: &KnownTool) -> bool {
        let held = crate::session::held();
        let redactions =
            self.metadata_redactions(&known.owner, self.known_live_server(known).as_deref());
        ![name, &known.original]
            .iter()
            .any(|text| saved::has_secret(text, &held, &redactions))
    }

    fn gone(&self, server: &Server, original: &str) -> String {
        if saved::has_secret(
            original,
            &crate::session::held(),
            &self.live_redactions(server),
        ) {
            HELD_CREDENTIAL.into()
        } else {
            text::gone(&server.watch.name, original)
        }
    }

    fn resolve(&self, name: &str) -> Result<(String, String), String> {
        if self.entry_name_hidden(name) {
            return Err(HELD_CREDENTIAL.into());
        }
        let mut catalog = self.catalog();
        match catalog.remove(name).as_deref() {
            Some([(server, tool)]) => Ok((server.clone(), tool.name.clone())),
            None => self
                .0
                .known
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .get(name)
                .map_or_else(
                    || Err(text::unavailable(name)),
                    |known| {
                        if self.known_name_visible(name, known) {
                            Ok((known.owner.0.clone(), known.original.clone()))
                        } else {
                            Err(saved::HELD_CREDENTIAL.into())
                        }
                    },
                ),
            _ => Err("MCP tool names overlap after unsupported characters are replaced".into()),
        }
    }
}

async fn start(
    config: &McpServer,
    cwd: &Path,
    keys: &BTreeMap<String, wisp_protocol::Secret>,
    forms: bool,
    flights: elicit::Flights,
    watch: Arc<Watch>,
) -> Result<Connection, String> {
    let mut command = Command::new(&config.command);
    crate::jobs::apply_job_env(&mut command);
    command
        .args(&config.args)
        .current_dir(cwd)
        .kill_on_drop(true);
    for name in &config.keys {
        if let Some(value) = keys.get(name) {
            command.env(name, value.expose());
        }
    }
    command.env(wisp_protocol::WISP_JOB, "1");
    #[cfg(unix)]
    command.process_group(0);
    let mut group = ProcessGroup(
        command
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .map_err(|error| {
                if error.kind() == std::io::ErrorKind::NotFound {
                    let command = Path::new(&config.command)
                        .file_name()
                        .and_then(|name| name.to_str())
                        .unwrap_or("the command");
                    text::not_installed(command)
                } else {
                    "could not start; check its command".into()
                }
            })?,
    );
    let transport = group
        .0
        .stdout
        .take()
        .zip(group.0.stdin.take())
        .ok_or("could not open its MCP pipes")?;
    let running = serve(transport, forms, flights, watch).await?;
    Ok(Connection {
        running,
        _group: Some(group),
    })
}

async fn start_http(
    address: &str,
    manager: Option<rmcp::transport::auth::AuthorizationManager>,
    challenge: Arc<Mutex<Option<String>>>,
    forms: bool,
    flights: elicit::Flights,
    watch: Arc<Watch>,
) -> Result<Connection, String> {
    use rmcp::transport::streamable_http_client::{
        StreamableHttpClientTransport, StreamableHttpClientTransportConfig,
    };
    let transport = StreamableHttpClientTransport::with_client(
        http::Http::new(address, manager, challenge)?,
        StreamableHttpClientTransportConfig::with_uri(address),
    );
    let running = serve(transport, forms, flights, watch).await?;
    Ok(Connection {
        running,
        _group: None,
    })
}

struct Asking {
    info: ClientConfig,
    flights: elicit::Flights,
    watch: Arc<Watch>,
}

impl Asking {
    fn legacy_notice(&self, context: NotificationContext<RoleClient>) {
        if context
            .peer
            .peer_info()
            .is_some_and(|info| info.protocol_version < ProtocolVersion::V_2026_07_28)
        {
            self.watch.notice();
        }
    }
}

impl rmcp::ClientHandler for Asking {
    fn get_info(&self) -> ClientConfig {
        self.info.clone()
    }

    async fn on_tool_list_changed(&self, context: NotificationContext<RoleClient>) {
        self.legacy_notice(context);
    }

    async fn on_prompt_list_changed(&self, context: NotificationContext<RoleClient>) {
        self.legacy_notice(context);
    }

    async fn on_resource_list_changed(&self, context: NotificationContext<RoleClient>) {
        self.legacy_notice(context);
    }

    async fn create_elicitation(
        &self,
        request: rmcp::model::ElicitRequestParams,
        _context: rmcp::service::RequestContext<RoleClient>,
    ) -> Result<rmcp::model::ElicitResult, rmcp::ErrorData> {
        use rmcp::model::{ElicitRequestParams, ElicitResult, ElicitationAction};
        if !matches!(request, ElicitRequestParams::FormElicitationParams { .. }) {
            return Ok(ElicitResult::new(ElicitationAction::Decline));
        }
        let flight = {
            let flights = self.flights.lock().unwrap_or_else(PoisonError::into_inner);
            match flights.as_slice() {
                [flight] => Some(flight.clone()),
                _ => None,
            }
        };
        let Some(flight) = flight else {
            return Ok(ElicitResult::new(ElicitationAction::Cancel));
        };
        if flight.rounds.fetch_add(1, Ordering::AcqRel) >= rmcp::model::DEFAULT_MRTR_MAX_ROUNDS {
            return Ok(ElicitResult::new(ElicitationAction::Cancel));
        }
        Ok(match elicit::answer(&flight, request).await {
            Ok(result) => result,
            Err(failure) => {
                *flight
                    .failure
                    .lock()
                    .unwrap_or_else(PoisonError::into_inner) = Some(failure);
                ElicitResult::new(ElicitationAction::Cancel)
            }
        })
    }
}

async fn serve<T, E, A>(
    transport: T,
    forms: bool,
    flights: elicit::Flights,
    watch: Arc<Watch>,
) -> Result<RunningService<RoleClient, Asking>, String>
where
    T: rmcp::transport::IntoTransport<RoleClient, E, A>,
    E: std::error::Error + Send + Sync + 'static,
{
    let mut capabilities = ClientCapabilities::default();
    if forms {
        capabilities.elicitation = Some(
            rmcp::model::ElicitationCapability::new()
                .with_form(rmcp::model::FormElicitationCapability::new()),
        );
    }
    let client = Asking {
        info: ClientConfig::new(
            capabilities,
            Implementation::new("Wisp", env!("CARGO_PKG_VERSION")),
        ),
        flights,
        watch,
    };
    client
        .serve_with_lifecycle(
            transport,
            ClientLifecycleMode::Auto {
                preferred_versions: vec![ProtocolVersion::V_2026_07_28],
                legacy_version: Some(ProtocolVersion::V_2025_11_25),
            },
        )
        .await
        .map_err(|_| "did not answer the MCP startup request".into())
}

async fn start_within<T>(
    limit: Duration,
    phase: impl Future<Output = Result<T, String>>,
) -> Result<T, String> {
    tokio::time::timeout(limit, phase)
        .await
        .map_err(|_| format!("did not start within {} seconds", START_LIMIT.as_secs()))?
}

async fn listen(peer: &Peer<RoleClient>, watch: &Arc<Watch>, closed: &CancellationToken) {
    let Some(info) = peer
        .peer_info()
        .filter(|info| info.protocol_version >= ProtocolVersion::V_2026_07_28)
    else {
        return;
    };
    let filter = SubscriptionFilter::builder()
        .tools_list_changed()
        .prompts_list_changed()
        .resources_list_changed()
        .build()
        .supported_by(&info.capabilities);
    if filter == SubscriptionFilter::default() {
        return;
    }
    if let Ok(Ok(mut subscription)) =
        tokio::time::timeout(OPTIONAL_LIMIT, peer.listen(filter)).await
    {
        let watch = watch.clone();
        let closed = closed.clone();
        tokio::spawn(async move {
            loop {
                tokio::select! {
                    biased;
                    () = closed.cancelled() => break,
                    next = subscription.next() => match next {
                        Ok(Some(_)) => watch.notice(),
                        _ => break,
                    },
                }
            }
        });
    }
}

pub(crate) fn is_address(command: &str) -> bool {
    command.starts_with("http://") || command.starts_with("https://")
}

struct ProcessGroup(Child);

impl Drop for ProcessGroup {
    fn drop(&mut self) {
        #[cfg(unix)]
        if let Some(pid) = self
            .0
            .id()
            .and_then(|id| i32::try_from(id).ok())
            .and_then(rustix::process::Pid::from_raw)
        {
            let _ = rustix::process::kill_process_group(pid, rustix::process::Signal::KILL);
        }
        let _ = self.0.try_wait();
    }
}

async fn pages<T, F>(
    family: &str,
    mut fetch: impl FnMut(Option<String>) -> F + Send,
) -> Result<(Vec<T>, Option<u64>), String>
where
    F: Future<Output = Result<(Vec<T>, Option<String>, Option<u64>), ServiceError>> + Send,
{
    let mut items = Vec::new();
    let mut cursor = None;
    let mut seen = BTreeSet::new();
    let mut ttl = None;
    loop {
        let (page, next, page_ttl) = fetch(cursor)
            .await
            .map_err(|_| format!("could not list its {family}"))?;
        ttl = merge_ttl(ttl, page_ttl);
        items.extend(page);
        cursor = next;
        let Some(next) = &cursor else {
            break;
        };
        if !seen.insert(next.clone()) {
            return Err(format!("repeated a {family} page"));
        }
    }
    Ok((items, ttl))
}

fn merge_ttl(previous: Option<u64>, next: Option<u64>) -> Option<u64> {
    match (previous, next) {
        (Some(a), Some(b)) => Some(u64::min(a, b)),
        (None, value) => value,
        (_, None) => Some(0),
    }
}

async fn fetch_list(peer: &Peer<RoleClient>) -> Result<(List, bool), String> {
    let capabilities = peer
        .peer_info()
        .map(|info| info.capabilities.clone())
        .unwrap_or_default();
    let tools = pages("tools", |cursor| async move {
        let page = peer
            .list_tools(Some(PaginatedRequestParams::default().with_cursor(cursor)))
            .await?;
        Ok((page.tools, page.next_cursor, page.ttl_ms))
    })
    .await;
    let (tools, mut ttl) = match tools {
        Ok(tools) => tools,
        Err(_) if capabilities.tools.is_none() => (Vec::new(), None),
        Err(error) => return Err(error),
    };
    let mut tools: Vec<_> = tools
        .into_iter()
        .map(|tool| ToolSpec {
            name: tool.name.into_owned(),
            description: tool.description.map(|s| s.into_owned()).unwrap_or_default(),
            input_schema: serde_json::Value::Object((*tool.input_schema).clone()),
        })
        .collect();
    tools.sort_by(|a, b| a.name.cmp(&b.name));
    let tools = serde_json::from_str(&canonical_specs(&tools))
        .map_err(|_| "returned invalid tool definitions")?;
    let prompts = if capabilities.prompts.is_some() {
        tokio::time::timeout(
            OPTIONAL_LIMIT,
            pages("prompts", |cursor| async move {
                let page = peer
                    .list_prompts(Some(PaginatedRequestParams::default().with_cursor(cursor)))
                    .await?;
                Ok((page.prompts, page.next_cursor, page.ttl_ms))
            }),
        )
        .await
        .unwrap_or_else(|_| Err("optional listing timed out".into()))
        .map(|(items, page_ttl)| {
            ttl = merge_ttl(ttl, page_ttl);
            items
                .into_iter()
                .map(saved::Prompt::from)
                .collect::<Vec<_>>()
        })
    } else {
        Ok(Vec::new())
    };
    let resources = if capabilities.resources.is_some() {
        tokio::time::timeout(
            OPTIONAL_LIMIT,
            pages("resources", |cursor| async move {
                let page = peer
                    .list_resources(Some(PaginatedRequestParams::default().with_cursor(cursor)))
                    .await?;
                Ok((page.resources, page.next_cursor, page.ttl_ms))
            }),
        )
        .await
        .unwrap_or_else(|_| Err("optional listing timed out".into()))
        .map(|(items, page_ttl)| {
            ttl = merge_ttl(ttl, page_ttl);
            items
                .into_iter()
                .map(saved::Resource::from)
                .collect::<Vec<_>>()
        })
    } else {
        Ok(Vec::new())
    };
    let complete = prompts.is_ok() && resources.is_ok();
    let mut prompts = prompts.unwrap_or_default();
    let mut resources = resources.unwrap_or_default();
    prompts.sort_by(|a, b| a.name.cmp(&b.name));
    resources.sort_by(|a, b| a.uri.cmp(&b.uri));
    Ok((List::new(tools, prompts, resources, ttl), complete))
}

fn prompt_name(server: &str, original: &str) -> String {
    format!("{}prompt:{original}", tool_name(server, ""))
}

fn resource_name(server: &str, uri: &str) -> String {
    format!("{}resource:{uri}", tool_name(server, ""))
}

pub fn tool_name(server: &str, tool: &str) -> String {
    let safe = |text: &str| {
        text.chars()
            .map(|ch| {
                if ch.is_ascii_alphanumeric() || ch == '_' || ch == '-' {
                    ch
                } else {
                    '_'
                }
            })
            .collect::<String>()
    };
    format!("{TOOL_PREFIX}{}__{}", safe(server), safe(tool))
}

pub(crate) fn check_add(name: &str, command: &str, args: &[String]) -> Result<(), String> {
    if wisp_protocol::looks_like_key(name) {
        return Err(wisp_tool_text::manage::KEY_NAME.to_owned());
    }
    if name.is_empty()
        || name.chars().any(char::is_control)
        || !wisp_plugin_api::manifest::is_tool_name(&tool_name(name, ""))
    {
        return Err("the server name is not valid".into());
    }
    if command.trim().is_empty()
        || command.contains('\0')
        || args.iter().any(|arg| arg.contains('\0'))
    {
        return Err("the command is not valid".into());
    }
    if is_address(command) {
        let address = reqwest::Url::parse(command).map_err(|_| "the address isn't valid")?;
        if address.host_str().is_none()
            || !address.username().is_empty()
            || address.password().is_some()
            || address.query().is_some()
            || address.fragment().is_some()
        {
            return Err("a key can't be saved here; use the server's sign-in".into());
        }
        if !args.is_empty() {
            return Err("use an address without command arguments".into());
        }
    }
    let words: Vec<_> = std::iter::once(command)
        .chain(args.iter().map(String::as_str))
        .collect();
    if words.iter().enumerate().any(|(index, word)| {
        wisp_protocol::command_secret(word, index.checked_sub(1).map(|previous| words[previous]))
            .is_some()
    }) {
        return Err("a key can't be saved here; when an agent adds the server with the key's name in keys, Wisp asks the user for it".into());
    }
    Ok(())
}

fn check_names(server: &str, tools: &[ToolSpec]) -> Result<(), &'static str> {
    let mut names = BTreeSet::new();
    for tool in tools {
        let name = tool_name(server, &tool.name);
        if !wisp_plugin_api::manifest::is_tool_name(&name) || !names.insert(name) {
            return Err("has tool names that cannot be mapped uniquely for a provider");
        }
    }
    Ok(())
}

fn content(
    result: rmcp::model::CallToolResult,
    withhold: Option<&str>,
    redactions: &Redactions,
) -> Result<ToolRun, String> {
    let mut text = Vec::new();
    let mut run = ToolRun::new(ToolOutcome::Completed {
        is_error: result.is_error.unwrap_or(false),
        exit: None,
    });
    let mut withheld = false;
    let mut attach = |data: String, mime: String| -> Result<(), String> {
        if withhold.is_some() {
            withheld = true;
            return Ok(());
        }
        run.attachments.push(Attachment {
            media_type: mime
                .parse()
                .map_err(|_| "the MCP result has an invalid media type")?,
            bytes: base64::engine::general_purpose::STANDARD
                .decode(data)
                .map_err(|_| "the MCP result has invalid attachment data")?,
            slice: None,
        });
        Ok(())
    };
    for block in result.content {
        if let ContentBlock::Text(block) = &block
            && let Some(structured) = &result.structured_content
            && serde_json::from_str::<serde_json::Value>(&block.text)
                .is_ok_and(|value| value == *structured)
        {
            continue;
        }
        if let Some(body) = block_text(&block) {
            text.push(redactions.scrub(body));
            continue;
        }
        match block {
            ContentBlock::Image(block) => attach(block.data, block.mime_type)?,
            ContentBlock::Audio(block) => attach(block.data, block.mime_type)?,
            ContentBlock::Resource(block) => match block.resource {
                ResourceContents::BlobResourceContents {
                    blob, mime_type, ..
                } => attach(
                    blob,
                    mime_type.unwrap_or_else(|| "application/octet-stream".into()),
                )?,
                resource => text.push(scrubbed_json(resource, redactions)?),
            },
            block => text.push(scrubbed_json(block, redactions)?),
        }
    }
    if let Some(structured) = result.structured_content {
        text.push(scrubbed_json(structured, redactions)?);
    }
    if let Some(server) = withhold.filter(|_| withheld) {
        text.push(wisp_tool_text::mcp::withheld_attachments(server));
    }
    run.result = Some(text.join("\n"));
    Ok(run)
}

fn scrubbed_json(value: impl serde::Serialize, redactions: &Redactions) -> Result<String, String> {
    let mut value = serde_json::to_value(value).map_err(|_| "the MCP result could not be read")?;
    redactions.scrub_value(&mut value);
    Ok(value.to_string())
}

fn block_text(block: &ContentBlock) -> Option<&str> {
    match block {
        ContentBlock::Text(block) => Some(&block.text),
        ContentBlock::Resource(block) => resource_text(&block.resource),
        ContentBlock::ResourceLink(link) => Some(&link.uri),
        _ => None,
    }
}

fn resource_text(resource: &ResourceContents) -> Option<&str> {
    match resource {
        ResourceContents::TextResourceContents { text, .. } => Some(text),
        _ => None,
    }
}
