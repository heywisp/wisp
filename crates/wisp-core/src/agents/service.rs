use std::collections::{BTreeMap, BTreeSet, VecDeque};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::time::Duration;

use serde_json::Value;
use tokio::sync::{Notify, mpsc, watch};
use tokio::task::JoinHandle;
use tokio::time::Instant;
use wisp_plugin_api::host::Delivery;
use wisp_protocol::agents::{
    Choice, RENAME_AGENT, REPORT, ROOT_NAME, SPAWN_AGENT, SPAWN_TOOLS, WRITE_TODOS, shown_agent,
};
use wisp_protocol::{
    Activity, AgentAction, AgentEntry, AgentEvent, AgentPath, AgentState, CONFIG_FILE, CacheTtl,
    ClientCommand, ContextFill, Effort, HoldReason, IdleOutcome, KeepWarm, MailId, MailKind,
    MessageTime, ParentLink, PresetChange, QueueReason, RejectKind, RequestId, SpawnChoices,
    SpawnRequest, TodoItem, TodoProgress, TodoStatus, shorten_path,
};
use wisp_tool_text::agents::EndRefusal;

use super::brief;
use super::endpoints::{self, EndpointView, Fixed, Gone, Ready};
use super::guarantee::StopCause;
use super::names::check_name;
use super::news::{News, StateNews, StopNews, TreeNews};
use super::report;
use super::state::{self, Change, Watch};
use super::{CheckedPresets, Limits, PresetProblem, tool_off};
use crate::agent::{self, Agent, AgentTask, Mail};
use crate::kit::{ChildSpec, InstructionsChoice, Kit, KitError, PresetSource};

pub const NOT_FOR_AGENTS: &str = "not_for_agents";
pub use wisp_protocol::agents::NO_SUCH_AGENT;
pub const REFUSED: &str = "agent_refused";
const CLOSING: &str = "the session is closing, so no agent can start";
pub(crate) const ROOT_EVENTS: usize = 1024;
pub(crate) const ROOT_COMMANDS: usize = 16;
const CHILD_COMMANDS: usize = 64;
const CHILD_EVENTS: usize = 256;
pub const MAX_PEER_MESSAGES: u32 = 16;

pub struct TreeSetup {
    pub presets: CheckedPresets,
    pub tools_off: Vec<String>,
    pub followers: Vec<(String, String)>,
    pub limits: Limits,
    pub instructions: InstructionsChoice,
    pub loaded: LoadedTree,
    pub keep_warm_main: bool,
    pub restarted: bool,
    pub start: Option<String>,
    pub unusable: Vec<wisp_protocol::subscription_auth::Unusable>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Caller {
    pub session_id: String,
    pub model: String,
    pub effort: Effort,
    pub tools: Vec<String>,
    pub questions: Vec<String>,
    pub cwd: Option<PathBuf>,
    pub home: Option<PathBuf>,
    pub cache_ttl: CacheTtl,
    pub message_time: MessageTime,
    pub context_note: bool,
    pub endpoint: String,
    pub plugins: crate::plugin::PluginSwitches,
}

#[derive(Debug, Clone, Default, PartialEq)]
pub struct TurnSignals {
    pub reported: Option<String>,
    pub invalid: u32,
    pub last_invalid: Option<(Value, String)>,
    pub asked: bool,
    pub to_parent: bool,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Spawned {
    pub entry: AgentEntry,
    pub session_id: String,
    pub config: SpawnChoices,
    pub fell_back: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Reach {
    Caller,
    Parent,
    Sibling,
    Child,
    Below,
}

pub(crate) struct OnAccount {
    pub(crate) agent: String,
    pub(crate) account: String,
    pub(crate) provider: Option<String>,
    pub(crate) working: bool,
    pub(crate) leave: Option<crate::agent::LeaveSender>,
}

struct Entry {
    path: watch::Sender<AgentPath>,
    session_id: String,
    depth: u32,
    model: String,
    effort: Effort,
    cwd: Option<String>,
    config: SpawnChoices,
    state: AgentState,
    activity: Activity,
    watch: Watch,
    commands: Option<mpsc::Sender<ClientCommand>>,
    processes: crate::jobs::Processes,
    leave: Option<crate::agent::LeaveSender>,
    mail: agent::MailSender,
    spent_usd: f64,
    plan_usd: Option<f64>,
    billing: wisp_protocol::Billing,
    lower_bound: bool,
    accounts: BTreeMap<String, wisp_protocol::AccountSpend>,
    last_report: Option<String>,
    checklist: Option<TodoProgress>,
    watched: bool,
    owes: bool,
    turn: TurnSignals,
    notes: Vec<AgentEvent>,
    stop: Option<StopCause>,
    permit: bool,
    worker: bool,
    granted: Arc<Notify>,
    announced: (f64, bool),
    peers: BTreeMap<AgentPath, u32>,
    questions: BTreeSet<AgentPath>,
    asking: BTreeSet<AgentPath>,
    ran: Option<Duration>,
    since: Option<tokio::time::Instant>,
    context: Option<ContextFill>,
    warm: Option<KeepWarm>,
    announced_state: AgentState,
    idle_at: Option<Instant>,
    state_since_ms: u64,
    place: Place,
    kind: Option<&'static str>,
    preset_change: Option<PresetChange>,
    waking: bool,
    request_seq: u64,
    left_active: Option<(u64, EndRefusal)>,
    halted_reply: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct Place {
    endpoint: Option<String>,
    provider: Option<String>,
    priced: bool,
}

impl Place {
    fn of(ready: &Ready, priced: bool) -> Self {
        Self {
            endpoint: Some(ready.name.clone()),
            provider: Some(ready.kind().as_str().to_owned()),
            priced,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Settled {
    pub(crate) model: String,
    pub(crate) effort: Effort,
    pub(crate) endpoint: String,
    pub(crate) provider: &'static str,
    pub(crate) kind: &'static str,
    pub(crate) priced: bool,
}

impl Entry {
    #[allow(
        clippy::too_many_arguments,
        reason = "the fields every entry must be given; the rest take their defaults here"
    )]
    fn new(
        path: watch::Sender<AgentPath>,
        session_id: String,
        depth: u32,
        model: String,
        effort: Effort,
        state: AgentState,
        mail: agent::MailSender,
        place: Place,
        state_since_ms: u64,
    ) -> Self {
        Self {
            path,
            session_id,
            depth,
            model,
            effort,
            cwd: None,
            config: SpawnChoices::default(),
            state,
            activity: Activity::Idle,
            watch: Watch::default(),
            commands: None,
            processes: crate::jobs::Processes::default(),
            leave: None,
            mail,
            spent_usd: 0.0,
            plan_usd: None,
            billing: Default::default(),
            lower_bound: false,
            accounts: BTreeMap::new(),
            last_report: None,
            checklist: None,
            watched: false,
            owes: false,
            turn: TurnSignals::default(),
            notes: Vec::new(),
            stop: None,
            permit: false,
            worker: true,
            granted: Arc::default(),
            announced: (0.0, false),
            peers: BTreeMap::new(),
            questions: BTreeSet::new(),
            asking: BTreeSet::new(),
            ran: None,
            since: None,
            context: None,
            warm: None,
            announced_state: state,
            idle_at: None,
            state_since_ms,
            place,
            kind: None,
            preset_change: None,
            waking: false,
            request_seq: 0,
            left_active: None,
            halted_reply: false,
        }
    }

    fn path(&self) -> AgentPath {
        self.path.borrow().clone()
    }

    fn track_running(&mut self) {
        match (self.state.is_running(), self.since) {
            (true, None) => self.since = Some(tokio::time::Instant::now()),
            (false, Some(at)) => {
                self.ran = Some(self.ran.unwrap_or_default() + at.elapsed());
                self.since = None;
            }
            _ => {}
        }
    }

    fn waits_on(&self, asked: &AgentPath) -> bool {
        let asking = matches!(
            self.state,
            AgentState::Running
                | AgentState::Stopping
                | AgentState::Idle {
                    outcome: Some(IdleOutcome::Asked)
                }
        );
        asking && self.asking.contains(asked)
    }

    fn running_ms(&self) -> Option<u64> {
        if self.path().is_root() || (self.ran.is_none() && self.since.is_none()) {
            return None;
        }
        let total =
            self.ran.unwrap_or_default() + self.since.map(|at| at.elapsed()).unwrap_or_default();
        Some(u64::try_from(total.as_millis()).unwrap_or(u64::MAX))
    }

    fn shown(&self) -> AgentEntry {
        AgentEntry {
            last_report: self.last_report.clone(),
            ..self.bare()
        }
    }

    fn priced(&self) -> bool {
        self.place.priced
    }

    fn bare(&self) -> AgentEntry {
        AgentEntry {
            plan_usd: self.plan_usd,
            billing: self.billing,
            path: self.path(),
            name: if self.path().is_root() {
                wisp_protocol::agents::ROOT_NAME.to_owned()
            } else {
                self.path().name().to_owned()
            },
            session_id: self.session_id.clone(),
            parent: self.path().parent(),
            preset: self.config.preset.clone(),
            model: self.model.clone(),
            effort: self.effort,
            depth: self.depth,
            state: self.state,
            activity: self.activity.clone(),
            spent_usd: self.spent_usd,
            lower_bound: self.lower_bound,
            accounts: self.accounts.clone(),
            checklist: self.checklist,
            last_report: None,
            cwd: self.cwd.clone(),
            running_ms: self.running_ms(),
            since_ms: Some(self.state_since_ms),
            context: self.context,
            keep_warm: self.warm,
            endpoint: self.place.endpoint.clone(),
            provider: self.place.provider.clone(),
            priced: self.priced(),
            preset_change: self.preset_change.filter(|_| self.state.is_live()),
        }
    }

    fn announce(&mut self, now_ms: u64, news: &News) -> AgentEvent {
        self.announced = (self.spent_usd, self.lower_bound);
        if self.announced_state.node_state() != self.state.node_state() {
            self.state_since_ms = now_ms;
        }
        if self.announced_state.is_busy() && !self.state.is_busy() {
            self.idle_at = Some(Instant::now());
        }
        self.announced_state = self.state;
        if !self.path().is_root() {
            news.push(TreeNews::State(StateNews {
                agent: self.path(),
                state: self.state,
                activity: self.activity.clone(),
                spent_usd: self.spent_usd,
                lower_bound: self.lower_bound,
            }));
        }
        self.changed()
    }

    fn unannounced(&self) -> bool {
        self.announced != (self.spent_usd, self.lower_bound)
    }

    fn warm_changed(&self) -> Option<AgentEvent> {
        self.warm.map(|keep_warm| AgentEvent::KeepWarmChanged {
            agent: self.path(),
            keep_warm,
        })
    }

    fn changed(&self) -> AgentEvent {
        AgentEvent::AgentStateChanged {
            plan_usd: self.plan_usd,
            billing: self.billing,
            agent: self.path(),
            state: self.state,
            activity: self.activity.clone(),
            spent_usd: self.spent_usd,
            lower_bound: self.lower_bound,
            accounts: self.accounts.clone(),
            since_ms: Some(self.state_since_ms),
        }
    }
}

#[derive(Default)]
struct Tree {
    events: Option<mpsc::Sender<AgentEvent>>,
    entries: BTreeMap<AgentPath, Entry>,
    starting: BTreeSet<AgentPath>,
    handles: Vec<JoinHandle<()>>,
    bounds: Vec<watch::Receiver<Duration>>,
    closed: bool,
    restarting: bool,
    restart_pending: bool,
    resumed: Arc<Notify>,
    restart_rows: Option<Vec<(AgentPath, state::Work)>>,
    waiters: VecDeque<AgentPath>,
    before_queue: BTreeMap<AgentPath, AgentState>,
    gone: Gone,
    renames: Vec<(AgentPath, AgentPath)>,
    renaming: Option<AgentPath>,
    seq: u64,
}

impl Tree {
    fn next_seq(&mut self) -> u64 {
        self.seq += 1;
        self.seq
    }

    pub(super) fn track_active<T>(
        &mut self,
        path: &AgentPath,
        f: impl FnOnce(&mut Entry) -> T,
    ) -> Option<T> {
        let entry = self.get_mut(path)?;
        let before = active_work(entry);
        let out = f(entry);
        let after = active_work(entry);
        if let (Some(reason), None) = (before, after) {
            let seq = self.next_seq();
            if let Some(entry) = self.get_mut(path) {
                entry.left_active = Some((seq, reason));
            }
        }
        Some(out)
    }

    fn resolve(&self, path: &AgentPath) -> AgentPath {
        path.followed(&self.renames)
    }

    fn get(&self, path: &AgentPath) -> Option<&Entry> {
        self.entries.get(&self.resolve(path))
    }

    fn get_mut(&mut self, path: &AgentPath) -> Option<&mut Entry> {
        let path = self.resolve(path);
        self.entries.get_mut(&path)
    }

    fn taken(&self, parent: &AgentPath, name: &str, own: Option<&AgentPath>) -> Result<(), String> {
        let path = parent.join(name);
        let leads = self.resolve(&path);
        let unused = leads == path && !self.entries.contains_key(&path);
        if (unused || Some(&leads) == own)
            && !self.starting.contains(&path)
            && self.renaming.as_ref() != Some(&path)
        {
            return Ok(());
        }
        Err(format!(
            "{} already has an agent named {name}; names are never reused, so choose another",
            shown_agent(parent)
        ))
    }

    fn rename(&mut self, from: &AgentPath, to: &AgentPath) {
        let follow = |path: AgentPath| path.renamed(from, to);
        let entries = std::mem::take(&mut self.entries);
        for (path, mut entry) in entries {
            let moved = follow(path.clone());
            if moved != path {
                entry.path.send_replace(moved.clone());
            }
            entry.peers = std::mem::take(&mut entry.peers)
                .into_iter()
                .map(|(p, n)| (follow(p), n))
                .collect();
            entry.questions = std::mem::take(&mut entry.questions)
                .into_iter()
                .map(follow)
                .collect();
            entry.asking = std::mem::take(&mut entry.asking)
                .into_iter()
                .map(follow)
                .collect();
            self.entries.insert(moved, entry);
        }
        self.starting = std::mem::take(&mut self.starting)
            .into_iter()
            .map(follow)
            .collect();
        for waiter in &mut self.waiters {
            *waiter = follow(std::mem::take(waiter));
        }
        self.before_queue = std::mem::take(&mut self.before_queue)
            .into_iter()
            .map(|(p, state)| (follow(p), state))
            .collect();
        self.renames.push((from.clone(), to.clone()));
        self.renaming = None;
    }
}

struct Shared {
    started: Instant,
    kit: Arc<Kit>,
    opening_replay: bool,
    presets: CheckedPresets,
    tools_off: Vec<String>,
    followers: Vec<(String, String)>,
    limits: Limits,
    instructions: InstructionsChoice,
    keep_warm_main: bool,
    restarted: bool,
    recount: Notify,
    tree: Mutex<Tree>,
    mail_ids: AtomicU64,
    worst: watch::Sender<Duration>,
    news: Arc<News>,
}

#[derive(Clone)]
pub struct AgentsService {
    shared: Arc<Shared>,
}

#[derive(Clone)]
pub struct AgentsHandle {
    service: Option<AgentsService>,
    path: watch::Receiver<AgentPath>,
    caller: Option<Arc<Caller>>,
}

impl Default for AgentsHandle {
    fn default() -> Self {
        Self::fixed(AgentPath::root())
    }
}

impl std::fmt::Debug for AgentsHandle {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("AgentsHandle")
            .field("path", &self.path())
            .field("tree", &self.service.is_some())
            .finish_non_exhaustive()
    }
}

fn refused(reason: impl Into<String>) -> (RejectKind, String) {
    (RejectKind::Other(NO_SUCH_AGENT.to_owned()), reason.into())
}

fn delivery_for(target: &AgentPath, state: AgentState, wake: bool, above: bool) -> Delivery {
    match state {
        _ if target.is_root() => Delivery::Running,
        AgentState::Running | AgentState::Stopping => Delivery::Running,
        AgentState::Queued {
            reason: QueueReason::Concurrency,
        } => Delivery::Queued,
        AgentState::Queued { .. } => Delivery::Running,
        AgentState::Held { reason } if wake && above && reason != HoldReason::Concurrency => {
            Delivery::Woken
        }
        AgentState::Held { .. } => Delivery::Held,
        AgentState::Idle {
            outcome: Some(IdleOutcome::Stopped | IdleOutcome::Failed),
        } if wake && !above => Delivery::Held,
        _ if wake => Delivery::Woken,
        _ => Delivery::Idle,
    }
}

fn active_work(entry: &Entry) -> Option<EndRefusal> {
    if entry.waking {
        return Some(EndRefusal::Running);
    }
    if entry.stop.is_some()
        || !limits::stoppable(entry.state)
        || entry.state == AgentState::Stopping
    {
        return None;
    }
    if entry.state.is_running() {
        Some(EndRefusal::Running)
    } else {
        Some(EndRefusal::Queued)
    }
}

fn end_refusal(
    tree: &Tree,
    caller: &AgentPath,
    target: &AgentPath,
) -> Option<(AgentPath, EndRefusal)> {
    let scope = |path: &AgentPath| path == target || target.is_ancestor_of(path);
    let live: Vec<&Entry> = tree
        .get(target)
        .into_iter()
        .chain(
            tree.entries
                .values()
                .filter(|e| target.is_ancestor_of(&e.path()) && e.state.is_live()),
        )
        .collect();
    if let Some((path, reason)) = live
        .iter()
        .find_map(|e| active_work(e).map(|reason| (e.path(), reason)))
    {
        return Some((path, reason));
    }
    let caller_entry = tree.get(caller)?;
    if let Some(sender) = caller_entry
        .mail
        .unread_report(|sender| scope(&tree.resolve(sender)))
    {
        return Some((tree.resolve(&sender), EndRefusal::UnreadReport));
    }
    if caller_entry.request_seq > 0
        && let Some((path, reason)) = live.iter().find_map(|e| {
            e.left_active
                .filter(|(seq, _)| *seq > caller_entry.request_seq)
                .map(|(_, reason)| (e.path(), reason))
        })
    {
        return Some((path, reason));
    }
    None
}

pub(crate) fn still_working(state: AgentState) -> bool {
    matches!(
        state,
        AgentState::Running
            | AgentState::Stopping
            | AgentState::Queued { .. }
            | AgentState::Held {
                reason: HoldReason::Concurrency
            }
            | AgentState::Idle {
                outcome: Some(IdleOutcome::Waiting)
            }
    )
}

fn build_failure(error: &KitError) -> &str {
    match error {
        KitError::Connect(_) => "its account couldn't be connected",
        KitError::Workspace(_) => "its working directory couldn't be used",
        KitError::Log(_) => "its session log couldn't be opened",
        KitError::Channel(_) | KitError::Resume(_) => "it couldn't be built",
        KitError::Preset(text) => text,
    }
}

fn agent_tool_refusal(tool: &str, tools: &[String]) -> String {
    super::agent_tool_text(tool, "spawn: false", |name| tools.iter().any(|t| t == name))
}

fn problem_text(problem: &PresetProblem) -> String {
    let text = problem.to_string();
    text.replacen("spawn = false", "spawn: false", 1)
}

fn inherited_tools(
    caller: &Caller,
    tools_off: &[String],
    spawn: bool,
    followers: &[(String, String)],
) -> Vec<String> {
    let off = |tool: &String| {
        tools_off.contains(tool)
            || followers
                .iter()
                .any(|(leader, follower)| follower == tool && tools_off.contains(leader))
    };
    let mut tools: Vec<String> = caller
        .tools
        .iter()
        .filter(|tool| !caller.questions.contains(tool))
        .filter(|tool| !off(tool))
        .filter(|tool| spawn || !SPAWN_TOOLS.contains(&tool.as_str()))
        .filter(|tool| *tool != RENAME_AGENT)
        .cloned()
        .chain([REPORT.to_owned()])
        .collect();
    tools.sort();
    tools.dedup();
    tools
}

impl AgentsService {
    fn new(
        kit: Arc<Kit>,
        setup: TreeSetup,
        events: mpsc::Sender<AgentEvent>,
        opening_replay: bool,
    ) -> Self {
        let (worst, _) = watch::channel(Duration::ZERO);
        Self {
            shared: Arc::new(Shared {
                started: Instant::now(),
                kit,
                opening_replay,
                presets: setup.presets,
                tools_off: setup.tools_off,
                followers: setup.followers,
                limits: setup.limits,
                instructions: setup.instructions,
                keep_warm_main: setup.keep_warm_main,
                restarted: setup.restarted,
                recount: Notify::new(),
                tree: Mutex::new(Tree {
                    events: Some(events),
                    ..Tree::default()
                }),
                mail_ids: AtomicU64::new(0),
                worst,
                news: Arc::default(),
            }),
        }
    }

    fn tree(&self) -> MutexGuard<'_, Tree> {
        self.shared
            .tree
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
    }

    pub fn handle(&self, path: AgentPath) -> AgentsHandle {
        let cell = self.tree().get(&path).map(|e| e.path.subscribe());
        self.handle_on(cell.unwrap_or_else(|| watch::channel(path).1))
    }

    fn handle_on(&self, path: watch::Receiver<AgentPath>) -> AgentsHandle {
        AgentsHandle {
            service: Some(self.clone()),
            path,
            caller: None,
        }
    }

    pub fn limits(&self) -> Limits {
        self.shared.limits
    }

    pub fn presets(&self) -> &CheckedPresets {
        &self.shared.presets
    }

    fn now_ms(&self) -> u64 {
        u64::try_from(self.shared.kit.clock.now().as_millisecond()).unwrap_or(0)
    }

    fn next_mail_id(&self) -> MailId {
        let n = self.shared.mail_ids.fetch_add(1, Ordering::SeqCst) + 1;
        MailId(format!("m{n}"))
    }

    pub fn snapshot(&self) -> Vec<AgentEntry> {
        self.tree()
            .entries
            .values()
            .filter(|e| !e.path().is_root())
            .map(Entry::shown)
            .collect()
    }

    fn environment(
        &self,
        path: &AgentPath,
        tools: &[String],
        instructions: Option<&str>,
        news: bool,
    ) -> Option<Vec<String>> {
        let spawns = tools.iter().any(|t| t == SPAWN_AGENT);
        if path.is_root() && !spawns && instructions.is_none() {
            return None;
        }
        let at_limit = self
            .shared
            .limits
            .max_depth
            .filter(|max| path.depth() >= *max);
        Some(brief::other_lines(
            path,
            spawns,
            at_limit,
            &self.shared.presets.usable(),
            instructions,
            news,
        ))
    }

    fn watch_bound(&self, mut bound: watch::Receiver<Duration>) {
        self.tree().bounds.push(bound.clone());
        self.publish_worst();
        let service = self.clone();
        let handle = tokio::spawn(async move {
            while bound.changed().await.is_ok() {
                service.publish_worst();
            }
            service.publish_worst();
        });
        drop(handle);
    }

    fn publish_worst(&self) {
        let worst = {
            let mut tree = self.tree();
            tree.bounds.retain(|b| b.has_changed().is_ok());
            tree.bounds
                .iter()
                .map(|b| *b.borrow())
                .max()
                .unwrap_or(Duration::ZERO)
        };
        self.shared.worst.send_replace(worst);
    }

    fn register_root(&self, agent: &Agent) {
        let caller = agent.caller();
        let marked = self.shared.keep_warm_main && agent.keeps_warm();
        let entry = Entry {
            processes: agent.processes(),
            cwd: caller
                .cwd
                .as_deref()
                .map(|c| shorten_path(c, caller.home.as_deref())),
            config: SpawnChoices {
                keep_warm: marked,
                ..SpawnChoices::default()
            },
            watched: true,
            context: ContextFill::of(&agent.context_now()),
            warm: self.warm_at_start(marked, true),
            kind: Some(agent.account_kind()),
            ..Entry::new(
                watch::channel(AgentPath::root()).0,
                caller.session_id.clone(),
                0,
                caller.model.clone(),
                caller.effort,
                AgentState::Running,
                agent.mail_sender(),
                Place {
                    endpoint: Some(caller.endpoint.clone()),
                    provider: Some(agent.provider_kind().as_str().to_owned()),
                    priced: agent.is_priced(),
                },
                self.now_ms(),
            )
        };
        self.tree().entries.insert(AgentPath::root(), entry);
    }

    fn warm_at_start(&self, marked: bool, paused: bool) -> Option<KeepWarm> {
        let limit = self.shared.limits.warm_refreshes;
        (marked && limit > 0).then_some(KeepWarm {
            refreshes: 0,
            limit,
            paused,
        })
    }

    fn check(
        &self,
        tree: &Tree,
        view: &EndpointView,
        parent: &AgentPath,
        caller: &Caller,
        request: &SpawnRequest,
        catalog: Option<&crate::endpoints::catalog::CatalogReader>,
    ) -> Result<Admission<Box<Plan>>, String> {
        if tree.closed {
            return Err(CLOSING.to_owned());
        }
        if !caller.tools.iter().any(|t| t == SPAWN_AGENT) {
            return Err(format!(
                "{} can't spawn agents: it has no {SPAWN_AGENT}",
                wisp_protocol::agents::shown_agent(parent)
            ));
        }
        check_name(&request.name)?;
        let path = parent.join(&request.name);
        tree.taken(parent, &request.name, None)?;
        let preset = match &request.preset {
            None => None,
            Some(name) => {
                if let Some(off) = self.shared.presets.off_named(name) {
                    return Err(format!(
                        "preset {name} is off: {}; the user fixes the file it names or {CONFIG_FILE}, and the fix applies from the next session start",
                        problem_text(&off.problem)
                    ));
                }
                Some(self.shared.presets.named(name)?.clone())
            }
        };
        let mut tools_off: Vec<String> = Vec::new();
        let named = preset
            .iter()
            .flat_map(|p| p.tools_off.iter())
            .chain(request.tools_off.iter());
        for tool in named {
            tools_off.push(tool_off(tool, &self.shared.tools_off).map_err(|p| match p {
                PresetProblem::AgentTool { tool } => agent_tool_refusal(tool, &caller.tools),
                p => problem_text(&p),
            })?);
        }
        tools_off.sort();
        tools_off.dedup();
        if request.task.trim().is_empty() {
            return Err("task is empty: say what the agent should do".to_owned());
        }
        wisp_tool_text::agents::check_todos(&request.todos)?;
        let (name, fixed) = match (
            &request.endpoint,
            preset
                .as_ref()
                .and_then(|p| Some((p.endpoint.as_ref()?, &p.name))),
        ) {
            (Some(name), _) => (name.as_str(), Fixed::Spawn),
            (None, Some((name, preset))) => (name.as_str(), Fixed::Preset(preset.as_str())),
            (None, None) => (caller.endpoint.as_str(), Fixed::Inherited),
        };
        let mut choices = choices(request, preset.as_ref(), caller);
        let cascading = choices.len() > 1;
        let adopted = match fixed {
            Fixed::Preset(_) if !cascading => fallback(tree, view, name, caller),
            _ => None,
        };
        let fixed = if let Some(target) = adopted {
            choices[0].endpoint.clone_from(&target.name);
            Fixed::Inherited
        } else {
            fixed
        };
        let unknown_override = request.endpoint.as_deref().is_some_and(|name| {
            view.ready(name).is_none()
                && view.unavailable(name).is_none()
                && !tree.gone.contains_key(name)
        }) || request.model.as_deref().is_some_and(|model| {
            crate::models::ModelTable::shared().find(model).is_none()
                && view.serving(model).is_none()
        });
        let now = self.shared.kit.clock.now().as_second();
        let mut reasons = Vec::new();
        let mut chosen = None;
        for (index, choice) in choices.iter().enumerate() {
            let fixed = if index == 0 {
                fixed
            } else {
                Fixed::Preset(preset.as_ref().map_or("", |preset| &preset.name))
            };
            let preset_model = (index == 0 && request.model.is_some())
                .then(|| preset.as_ref().and_then(|preset| preset.model.as_deref()))
                .flatten();
            let refuse_account = !cascading || (index == 0 && unknown_override);
            let target = match target(tree, view, &choice.endpoint, !refuse_account) {
                Ok(target) => target,
                Err(reason) if refuse_account => return Err(reason),
                Err(reason) => {
                    reasons.push(reason);
                    continue;
                }
            };
            let landing = self.land(
                choice,
                target,
                (
                    fixed,
                    preset_model,
                    adopted.is_some() && request.model.is_none(),
                ),
                caller,
                (view, &tree.gone),
                catalog,
            );
            let (target, model, effort) = match landing {
                Ok(Admission::Ready(landing)) => landing,
                Ok(Admission::Refresh(alias, model)) => {
                    return Ok(Admission::Refresh(alias, model));
                }
                Err(reason)
                    if !cascading
                        || (index == 0
                            && (request.model.is_some() || request.effort.is_some())) =>
                {
                    return Err(reason);
                }
                Err(_) => {
                    reasons.push(format!(
                        "{} can't use its selected model or effort",
                        endpoints::plain(&choice.endpoint)
                    ));
                    continue;
                }
            };
            if let crate::plan_usage::Room::Used {
                model,
                resets_at,
                cause,
            } = self.shared.kit.plan_usage.room(&target.name, &model, now)
            {
                if !cascading {
                    return Err(endpoints::used_up_text(
                        &target.name,
                        model.as_deref(),
                        resets_at,
                        now,
                        &cause,
                    ));
                }
                reasons.push(endpoints::limit_text(
                    &target.name,
                    model.as_deref(),
                    resets_at,
                    now,
                    &cause,
                ));
                continue;
            }
            chosen = Some((target, model, effort));
            break;
        }
        let (target, model, effort) = chosen.ok_or_else(|| {
            endpoints::no_room_text(preset.as_ref().map_or("", |preset| &preset.name), &reasons)
        })?;
        let fallback_note = (!reasons.is_empty()).then(|| {
            brief::fallback_text(
                wisp_tool_text::agents::Fallback::Started,
                &target.name,
                &model,
                effort,
                target.billing().is_metered(),
                &reasons.join("; "),
            )
        });
        let cwd = resolve_cwd(request.cwd.as_deref(), caller)?;
        let depth = parent.depth() + 1;
        if let Some(max) = self.shared.limits.max_depth
            && depth > max
        {
            return Err(format!(
                "the depth limit ({max}) is reached: {} is at depth {}, so it can't spawn",
                wisp_protocol::agents::shown_agent(parent),
                parent.depth()
            ));
        }
        let live =
            tree.entries.values().filter(|e| e.state.is_live()).count() + tree.starting.len();
        if live >= limits::cap(self.shared.limits.max_agents) {
            return Err(format!(
                "the tree already holds {live} agents that haven't ended (main included), the agents.max_agents limit; end a finished one first"
            ));
        }
        let spawn = request
            .spawn
            .or_else(|| preset.as_ref().and_then(|p| p.spawn))
            .unwrap_or(true);
        let instructions = preset.as_ref().and_then(|p| {
            let found = self.shared.presets.instructions(&p.name)?;
            Some(PresetSource {
                preset: p.name.clone(),
                path: found.path.clone(),
            })
        });
        let tools = inherited_tools(caller, &tools_off, spawn, &self.shared.followers);
        let todos = if tools.iter().any(|t| t == WRITE_TODOS) {
            request
                .todos
                .iter()
                .map(|text| TodoItem {
                    text: text.trim().to_owned(),
                    status: TodoStatus::Pending,
                    section: None,
                })
                .collect()
        } else {
            Vec::new()
        };
        Ok(Admission::Ready(Box::new(Plan {
            path,
            depth,
            model,
            effort,
            cwd,
            tools,
            todos,
            target: target.clone(),
            fell_back: adopted.map(|_| endpoints::plain(name).to_owned()),
            fallback_note,
            config: SpawnChoices {
                preset: preset.as_ref().map(|p| p.name.clone()),
                tools_off,
                spawn,
                instructions: instructions
                    .as_ref()
                    .map(|i| shorten_path(&i.path, caller.home.as_deref())),
                preset_values: preset.map(Box::new),
                keep_warm: request.keep_warm && target.keeps_warm(),
                choices: if cascading { choices } else { Vec::new() },
            },
            instructions,
        })))
    }

    fn land(
        &self,
        choice: &Choice,
        target: &Ready,
        selection: (Fixed<'_>, Option<&str>, bool),
        caller: &Caller,
        places: (&EndpointView, &Gone),
        catalog: Option<&crate::endpoints::catalog::CatalogReader>,
    ) -> Result<Admission<(Ready, String, Effort)>, String> {
        let (fixed, preset_model, adopted) = selection;
        let (view, gone) = places;
        let effort = choice.effort;
        let landing_model = || {
            let listing = agent::Listing::from_provider(target.provider.as_ref());
            let entries = self.shared.kit.start_layers.model_entries();
            wisp_protocol::settings::lands_on(
                &wisp_protocol::settings::offers(&entries),
                listing.landing(target.kind().as_str()),
                view.ready(&caller.endpoint)
                    .is_some_and(|own| own.name == target.name),
                (agent::chat(&caller.model), Some(caller.effort)),
                None,
            )
            .model
        };
        let table = crate::models::ModelTable::shared();
        let mut explicit = choice.model.as_deref();
        let (model, named) = loop {
            let model = explicit.map_or_else(landing_model, str::to_owned);
            let mapped = effort.and_then(|level| table.sibling(&model, level));
            let sibling = mapped.is_some_and(|id| id != model);
            let named = explicit.is_some() || sibling;
            let model = mapped.unwrap_or(&model).to_owned();
            if named && catalog.is_some_and(|catalog| catalog.needs_refetch(&target.name, &model)) {
                return Ok(Admission::Refresh(target.name.clone(), model));
            }
            if adopted && explicit.is_some() && target.row(&model).is_none() {
                explicit = None;
                continue;
            }
            if sibling && target.row(&model).is_none() {
                return Err(PresetProblem::ModelNotServed {
                    model,
                    endpoint: target.name.clone(),
                }
                .to_string());
            }
            break (model, named);
        };
        let row = match target.row(&model) {
            Some(row) => Some(row),
            None if !named => None,
            None if fixed == Fixed::Inherited => {
                return Err(endpoints::unlisted_text(
                    &model,
                    view.serving_where(&model, gone),
                ));
            }
            None => {
                return Err(endpoints::not_served_text(
                    &model,
                    target,
                    fixed,
                    preset_model,
                ));
            }
        };
        endpoints::provider_mismatch(target.provider.as_ref(), &model)?;
        if let Some(row) = row {
            row.check_selectable(target.kind(), &model)?;
        }
        let effort =
            effort.filter(|_| !wisp_protocol::settings::takes_no_effort(row.map(|r| r.efforts)));
        if let Some(row) = row
            && !row.accepts_effort(effort, caller.effort)
        {
            return Err(PresetProblem::EffortNotTaken {
                model: model.clone(),
                effort: effort.unwrap_or(caller.effort),
            }
            .to_string());
        }
        let effort = table.own_effort(&model, effort.unwrap_or(caller.effort));
        Ok(Admission::Ready((target.clone(), model, effort)))
    }

    async fn spawn(
        &self,
        parent: &AgentPath,
        caller: &Caller,
        request: &SpawnRequest,
    ) -> Result<Spawned, String> {
        let child_path = parent.join(&request.name);
        let catalog = self
            .shared
            .kit
            .endpoint_host
            .as_ref()
            .and_then(|host| host.catalog())
            .map(|catalog| catalog.reader());
        let (plan, reserved, stopped_before) = loop {
            let view = match self.shared.kit.connect.connect(child_path.as_str()) {
                Ok(connected) => EndpointView::connected(&connected, &caller.endpoint),
                Err(_) => {
                    return Err(
                        "couldn't start the agent: its account couldn't be connected".to_owned(),
                    );
                }
            };
            let (alias, model) = {
                let mut tree = self.tree();
                let parent = &tree.resolve(parent);
                match self.check(&tree, &view, parent, caller, request, catalog.as_ref())? {
                    Admission::Ready(plan) => {
                        tree.starting.insert(plan.path.clone());
                        let reserved = Reserved {
                            service: self,
                            path: Some(plan.path.clone()),
                        };
                        let stopped = tree.get(parent).is_some_and(|e| e.stop.is_some());
                        break (*plan, reserved, stopped);
                    }
                    Admission::Refresh(alias, model) => (alias, model),
                }
            };
            if let Some(catalog) = &catalog {
                catalog.refetch_unlisted(&alias, &model).await;
            }
        };
        let _admission = crate::endpoints::generation::admit(plan.target.provider.generation())
            .map_err(|error| error.to_string())?;
        let spec = ChildSpec {
            path: plan.path.clone(),
            model: plan.model.clone(),
            effort: plan.effort,
            cache_ttl: caller.cache_ttl,
            message_time: caller.message_time,
            context_note: caller.context_note,
            instructions: self.shared.instructions.clone(),
            cwd: plan.cwd.clone(),
            tools: plan.tools.clone(),
            parent: ParentLink {
                session_id: caller.session_id.clone(),
                agent: plan.path.clone(),
                depth: plan.depth,
                config: plan.config.clone(),
                todos: plan.todos.clone(),
            },
            endpoint: plan.target.name.clone(),
            preset_instructions: plan.instructions.clone(),
            plugins: caller.plugins.clone(),
        };
        let built = self.shared.kit.build_child(spec).await;
        let mut tree = self.tree();
        reserved.release(&mut tree);
        let parent = &tree.resolve(parent);
        let path = tree.resolve(&plan.path);
        let built = match built {
            Ok(built) if tree.closed => {
                drop(tree);
                drop(built);
                return Err(CLOSING.to_owned());
            }
            Ok(built)
                if tree.get(parent).is_none_or(|e| {
                    !e.state.is_live() || (e.stop.is_some() && !stopped_before)
                }) =>
            {
                drop(tree);
                drop(built);
                return Err(format!(
                    "{} was stopped while {path} was starting, so it didn't start",
                    shown_agent(parent),
                ));
            }
            Ok(built) => built,
            Err(e) => return Err(format!("couldn't start {path}: {}", build_failure(&e))),
        };
        let lines = self.environment(
            &path,
            &plan.tools,
            plan.config.instructions.as_deref(),
            built.hooks_word_news(),
        );
        let granted = Arc::new(Notify::new());
        let context = ContextFill::of(&built.context_now());
        let (cell, cell_rx) = watch::channel(path.clone());
        let child = built
            .with_agents(self.handle_on(cell_rx.clone()), lines)
            .with_permit_signal(granted.clone());
        let session_id = child.session_id().to_owned();
        let mail = child.mail_sender();
        let shown_cwd = shorten_path(&plan.cwd, caller.home.as_deref());
        let text = brief::brief(&request.task, &request.todos);
        let brief_mail = Mail {
            id: self.next_mail_id(),
            from: parent.clone(),
            to: path.clone(),
            kind: MailKind::Task,
            wake: true,
            text,
        };
        if mail.send(brief_mail).is_err() {
            drop(tree);
            drop(child);
            return Err(format!("couldn't start {path}: its mailbox is closed"));
        }
        let (commands, command_rx) = mpsc::channel(CHILD_COMMANDS);
        let worker = self.shared.presets.worker(plan.config.preset.as_deref());
        let (permit, state) = tree.admit(parent, &path, worker, self.shared.limits);
        let mut entry = Entry {
            processes: child.processes(),
            worker,
            cwd: Some(shown_cwd.clone()),
            config: plan.config.clone(),
            activity: Activity::Streaming,
            commands: Some(commands),
            leave: Some(child.leave_sender()),
            checklist: checklist_of(&plan.todos),
            owes: true,
            permit,
            granted,
            context,
            warm: self.warm_at_start(plan.config.keep_warm, false),
            kind: Some(plan.target.account_kind()),
            ..Entry::new(
                cell,
                session_id.clone(),
                plan.depth,
                plan.model.clone(),
                plan.effort,
                state,
                mail,
                Place::of(&plan.target, child.is_priced()),
                self.now_ms(),
            )
        };
        entry.track_running();
        let shown = entry.shown();
        let node = crate::plugin::convert::node(entry.bare());
        tree.entries.insert(path, entry);
        self.shared.news.push(TreeNews::Spawned(Box::new(node)));
        let root_events = tree.events.clone();
        let bound = child.queue_watch();
        let handles = self.run_child(cell_rx, child, command_rx, root_events);
        tree.handles.extend(handles);
        drop(tree);
        self.watch_bound(bound);
        if let Some(text) = plan.fallback_note {
            let _ = self.post(shown.path.clone(), parent, MailKind::Notice, false, text);
        }
        Ok(Spawned {
            entry: shown,
            session_id,
            config: plan.config,
            fell_back: plan.fell_back,
        })
    }

    fn run_child(
        &self,
        path: watch::Receiver<AgentPath>,
        mut child: Agent,
        mut commands: mpsc::Receiver<ClientCommand>,
        root: Option<mpsc::Sender<AgentEvent>>,
    ) -> [JoinHandle<()>; 2] {
        let (events, mut child_events) = mpsc::channel(CHILD_EVENTS);
        let serve = tokio::spawn(async move {
            child.serve(&mut commands, &events).await;
        });
        let service = self.clone();
        let forward = tokio::spawn(async move {
            while let Some(event) = child_events.recv().await {
                let path = path.borrow().clone();
                let changed = service.observe(&path, &event);
                let recount = Watch::reads(&event);
                let Some(root) = &root else {
                    continue;
                };
                if let Some(event) = service.forwarded(&path, event) {
                    let _ = root.send(event).await;
                }
                if let Some(changed) = changed {
                    let _ = root.send(changed).await;
                }
                if recount {
                    service.shared.recount.notify_one();
                }
            }
        });
        [serve, forward]
    }

    fn forwarded(&self, path: &AgentPath, event: AgentEvent) -> Option<AgentEvent> {
        match event {
            AgentEvent::AgentStateChanged { .. }
            | AgentEvent::AccountsGone { .. }
            | AgentEvent::AgentReported { .. }
            | AgentEvent::AgentSpawned { .. }
            | AgentEvent::KeepWarmChanged { .. } => Some(event),
            AgentEvent::ToolOutput { .. } if !self.watched(path) => None,
            event => Some(AgentEvent::Agent {
                agent: path.clone(),
                event: Box::new(event),
            }),
        }
    }

    fn watch(&self, target: &AgentPath, live: bool) -> Result<(), String> {
        let mut tree = self.tree();
        match tree.get_mut(target).filter(|e| !e.path().is_root()) {
            Some(entry) if !entry.state.is_live() && live => Err(format!("{target} has ended")),
            Some(entry) => {
                entry.watched = live;
                Ok(())
            }
            None => Err(format!("there is no agent {target}")),
        }
    }

    fn watched(&self, path: &AgentPath) -> bool {
        self.tree().get(path).is_some_and(|e| e.watched)
    }

    fn observe(&self, path: &AgentPath, event: &AgentEvent) -> Option<AgentEvent> {
        if let AgentEvent::TodosUpdated { items, .. } = event {
            self.with_entry(path, |e| e.checklist = checklist_of(items));
        }
        if !Watch::reads(event) {
            return None;
        }
        let mut tree = self.tree();
        let entry = tree.get_mut(path)?;
        let activity = entry.watch.activity(event)?;
        if path.is_root() || !entry.state.is_running() || entry.activity == activity {
            return None;
        }
        entry.activity = activity;
        Some(entry.announce(self.now_ms(), &self.shared.news))
    }

    fn change(
        &self,
        path: &AgentPath,
        change: Change,
        spent: Option<(f64, bool)>,
        activity: Option<Activity>,
    ) -> Option<AgentEvent> {
        let mut tree = self.tree();
        let path = &tree.resolve(path);
        if tree.get(path).is_none_or(|e| e.path().is_root()) {
            return None;
        }
        let now = self.now_ms();
        let news = &self.shared.news;
        let (before_state, after_state, event) = tree.track_active(path, |entry| {
            let before = (
                entry.state,
                entry.activity.clone(),
                entry.spent_usd,
                entry.lower_bound,
            );
            entry.state = state::next(entry.state, change);
            if change != Change::Stopping {
                entry.waking = false;
            }
            if matches!(change, Change::Settled(_)) {
                entry.stop = None;
            }
            entry.track_running();
            if let Some(activity) = activity.or_else(|| state::activity_of(entry.state)) {
                entry.activity = activity;
            } else if change == Change::Started {
                entry.activity = Activity::Streaming;
            }
            if let Some((usd, lower)) = spent {
                entry.spent_usd = usd;
                entry.lower_bound = lower;
            }
            let after = (
                entry.state,
                entry.activity.clone(),
                entry.spent_usd,
                entry.lower_bound,
            );
            let event = (before != after || entry.unannounced()).then(|| entry.announce(now, news));
            (before.0, entry.state, event)
        })?;
        tree.remember_before_queue(path, before_state, after_state);
        event
    }

    fn route(
        &self,
        target: &AgentPath,
        command: ClientCommand,
    ) -> Result<(), (RejectKind, String)> {
        let mut tree = self.tree();
        let Some(entry) = tree.get(target).filter(|e| !e.path().is_root()) else {
            return Err(refused(format!("there is no agent {target}")));
        };
        if !entry.state.is_live() {
            return Err(refused(format!("{target} has ended")));
        }
        if command == ClientCommand::Cancel {
            drop(tree);
            return self
                .stop_tree(target, None, false)
                .map(|_| ())
                .map_err(refused);
        }
        let wakes = matches!(
            command,
            ClientCommand::Send { .. } | ClientCommand::Release | ClientCommand::Retry { .. }
        );
        let sent = matches!(command, ClientCommand::Send { .. });
        let Some(commands) = entry.commands.clone() else {
            return Err(refused(format!("{target} has ended")));
        };
        commands.try_send(command).map_err(|e| match e {
            mpsc::error::TrySendError::Full(_) => (
                RejectKind::TooMany,
                format!("{target} has too many commands waiting; try again"),
            ),
            mpsc::error::TrySendError::Closed(_) => refused(format!("{target} has ended")),
        })?;
        if let Some(entry) = tree.get_mut(target) {
            if sent {
                entry.peers.clear();
            }
            if wakes && !limits::stoppable(entry.state) {
                entry.stop = None;
            }
        }
        Ok(())
    }

    fn stop_tree(
        &self,
        target: &AgentPath,
        by: Option<&AgentPath>,
        end: bool,
    ) -> Result<(u32, Vec<AgentEvent>), String> {
        let mut notices = Vec::new();
        let mut ended = Vec::new();
        let mut stopping = Vec::new();
        let now = self.now_ms();
        let count = {
            let mut tree = self.tree();
            let target = &tree.resolve(target);
            let by = by.map(|by| tree.resolve(by));
            let by = by.as_ref();
            let parent = target.parent();
            let cause = match by {
                None => StopCause::User,
                Some(by) if parent.as_ref() == Some(by) => StopCause::ByParent,
                Some(_) => StopCause::ByAncestor,
            };
            match tree.get(target) {
                Some(entry) if entry.path().is_root() => {
                    return Err(format!("{ROOT_NAME} can't be stopped or ended this way"));
                }
                Some(entry) if !entry.state.is_live() => {
                    return Err(format!("{target} has ended"));
                }
                Some(_) => {}
                None => return Err(format!("there is no agent {target}")),
            }
            if end
                && let Some(caller) = by
                && let Some((who, reason)) = end_refusal(&tree, caller, target)
            {
                if let Some(entry) = tree.get_mut(caller) {
                    entry.halted_reply = true;
                }
                let below = (&who != target).then_some(who.as_str());
                return Err(wisp_tool_text::agents::end_refused(
                    target.as_str(),
                    below,
                    reason,
                ));
            }
            let below: Vec<(AgentPath, bool)> = tree
                .entries
                .values()
                .filter(|e| target.is_ancestor_of(&e.path()) && e.state.is_live())
                .map(|e| (e.path(), limits::stoppable(e.state)))
                .collect();
            let count = below
                .iter()
                .filter(|(_, stoppable)| end || *stoppable)
                .count();
            let first = if end { StopCause::ByParent } else { cause };
            let events = tree.events.clone();
            let news = &self.shared.news;
            let told = |path: &AgentPath| StopNews {
                agent: path.clone(),
                target: target.clone(),
                by: by.cloned(),
            };
            for (path, cause, stoppable) in std::iter::once((target.clone(), first, true)).chain(
                below
                    .into_iter()
                    .map(|(p, stoppable)| (p, StopCause::WithParent, stoppable)),
            ) {
                if end {
                    let Some(entry) = tree.get_mut(&path) else {
                        continue;
                    };
                    entry.stop = Some(cause);
                    entry.waking = false;
                    if let Some(commands) = &entry.commands {
                        let _ = commands.try_send(ClientCommand::Cancel);
                    }
                    entry.commands = None;
                    entry.state = state::next(entry.state, Change::Ended);
                    entry.track_running();
                    entry.activity = Activity::Idle;
                    ended.push(entry.announce(now, news));
                    news.push(TreeNews::Ended(told(&path)));
                } else {
                    tree.track_active(&path, |entry| {
                        entry.stop = Some(cause);
                        entry.waking = false;
                        if let Some(warm) = entry.warm.as_mut().filter(|w| !w.paused) {
                            warm.paused = true;
                            stopping.extend(entry.warm_changed());
                        }
                        if entry.state == AgentState::Running {
                            entry.state = state::next(entry.state, Change::Stopping);
                            stopping.push(entry.announce(now, news));
                        }
                        if stoppable {
                            news.push(TreeNews::Stopped(told(&path)));
                        }
                        if let Some(commands) = &entry.commands {
                            let _ = commands.try_send(ClientCommand::Cancel);
                        }
                    });
                }
            }
            if end {
                let told = parent
                    .as_ref()
                    .filter(|p| by != Some(*p))
                    .filter(|p| tree.get(p).is_some_and(|e| e.state.is_live()));
                if let Some(parent) = told {
                    let text = brief::ended_text(target, cause);
                    notices.push((target.clone(), parent.clone(), false, text));
                }
                let gone: Vec<AgentPath> = ended
                    .iter()
                    .filter_map(|e| match e {
                        AgentEvent::AgentStateChanged { agent, .. } => Some(agent.clone()),
                        _ => None,
                    })
                    .collect();
                for asker in tree.entries.values().filter(|e| e.state.is_live()) {
                    for asked in gone.iter().filter(|g| asker.waits_on(g)) {
                        let text = brief::unanswered_ended_text(asked);
                        notices.push((asked.clone(), asker.path(), true, text));
                    }
                }
            }
            if let Some(events) = events {
                for event in stopping {
                    let _ = events.try_send(event);
                }
            }
            u32::try_from(count).unwrap_or(u32::MAX)
        };
        for (from, to, wake, text) in notices {
            let _ = self.post(from, &to, MailKind::Notice, wake, text);
        }
        Ok((count, ended))
    }

    fn take_handles(&self) -> Vec<JoinHandle<()>> {
        std::mem::take(&mut self.tree().handles)
    }

    fn close(&self) {
        let now = self.now_ms();
        let mut tree = self.tree();
        if tree.closed {
            return;
        }
        tree.closed = true;
        let events = tree.events.take();
        let mut changes = Vec::new();
        for entry in tree.entries.values_mut().filter(|e| !e.path().is_root()) {
            entry.commands = None;
            if entry.state.is_live() {
                entry.state = state::next(entry.state, Change::Ended);
                entry.track_running();
                entry.activity = Activity::Idle;
                entry.permit = false;
                changes.push(entry.announce(now, &self.shared.news));
            }
        }
        if let Some(events) = events {
            for change in changes {
                let _ = events.try_send(change);
            }
        }
    }

    fn with_entry<T>(&self, path: &AgentPath, f: impl FnOnce(&mut Entry) -> T) -> Option<T> {
        self.tree().get_mut(path).map(f)
    }

    fn send_from(
        &self,
        from: &AgentPath,
        target: &AgentPath,
        text: String,
        wake: bool,
    ) -> Result<(Reach, Delivery), String> {
        let (reach, from, target) = {
            let tree = self.tree();
            let from = &tree.resolve(from);
            let target = &tree.resolve(target);
            let Some(entry) = tree.get(target) else {
                return Err(format!("there is no agent {target}"));
            };
            let Some(reach) = reach(from, target) else {
                return Err(if target.is_ancestor_of(from) {
                    format!(
                        "{} is above your parent, so you can't message it; send it to your parent (`..`) to pass on",
                        shown_agent(target)
                    )
                } else {
                    format!(
                        "{target} isn't yours to message: you reach your parent, your siblings and the agents below you, so ask your parent to pass it on"
                    )
                });
            };
            if !entry.state.is_live() {
                return Err(format!("{target} has ended"));
            }
            if matches!(reach, Reach::Child | Reach::Below)
                && let Some(gone) = entry
                    .place
                    .endpoint
                    .as_ref()
                    .filter(|e| tree.gone.contains_key(*e))
            {
                return Err(endpoints::gone_text(gone));
            }
            if reach == Reach::Sibling
                && entry.peers.get(from).copied().unwrap_or(0) >= MAX_PEER_MESSAGES
            {
                return Err(format!(
                    "{target} already has {MAX_PEER_MESSAGES} messages from you since its parent last wrote to it; ask your parent (`..`) to take this further"
                ));
            }
            (reach, from.clone(), target.clone())
        };
        let (from, target) = (&from, &target);
        let kind = if reach == Reach::Child && wake {
            MailKind::Task
        } else {
            MailKind::Message
        };
        let text = if reach == Reach::Below && from.depth() + 1 < target.depth() {
            format!(
                "{text}\n\n(You can't message {} directly: reply through your parent.)",
                shown_agent(from)
            )
        } else {
            text
        };
        let delivery = self.post(from.clone(), target, kind, wake, text)?;
        let mut tree = self.tree();
        let from = &tree.resolve(from);
        let target = &tree.resolve(target);
        if let Some(sender) = tree.get_mut(from) {
            sender.questions.remove(target);
            sender.turn.to_parent |= reach == Reach::Parent;
            if wake && matches!(reach, Reach::Parent | Reach::Sibling) {
                sender.turn.asked = true;
                sender.asking.insert(target.clone());
            }
        }
        if let Some(entry) = tree.get_mut(target) {
            entry.asking.remove(from);
            let above = matches!(reach, Reach::Child | Reach::Below);
            if above {
                entry.peers.clear();
            } else if reach == Reach::Sibling {
                *entry.peers.entry(from.clone()).or_default() += 1;
            }
        }
        Ok((reach, delivery))
    }

    fn post(
        &self,
        from: AgentPath,
        to: &AgentPath,
        kind: MailKind,
        wake: bool,
        text: String,
    ) -> Result<Delivery, String> {
        let mail = Mail {
            id: self.next_mail_id(),
            from,
            to: to.clone(),
            kind,
            wake,
            text,
        };
        self.deliver(to, mail)
    }

    fn deliver(&self, to: &AgentPath, mail: Mail) -> Result<Delivery, String> {
        let mut tree = self.tree();
        let to = tree.resolve(to);
        let from = tree.resolve(&mail.from);
        let above = from == to || from.is_ancestor_of(&to);
        let wake = mail.wake;
        let Some(entry) = tree.get_mut(&to).filter(|e| e.state.is_live()) else {
            return Err(format!("{to} has ended"));
        };
        let delivery = delivery_for(&to, entry.state, wake, above);
        entry.mail.send(mail).map_err(|e| match e {
            agent::MailRefused::TooMany => format!(
                "{to} already has {} of your messages waiting; wait for it to read them",
                agent::MAX_MAIL_PER_SENDER
            ),
            agent::MailRefused::Closed => format!("{to} has ended"),
        })?;
        if delivery == Delivery::Woken {
            entry.waking = true;
            entry.stop = None;
        }
        Ok(delivery)
    }

    fn rename(
        &self,
        target: &AgentPath,
        name: &str,
    ) -> Result<(AgentPath, AgentPath), (RejectKind, String)> {
        let refuse = |reason: String| (RejectKind::Other(REFUSED.to_owned()), reason);
        let mut tree = self.tree();
        let Some(entry) = tree.get(target) else {
            return Err(refused(format!("there is no agent {target}")));
        };
        let from = entry.path();
        let Some(parent) = from.parent() else {
            return Err(refuse(format!("{ROOT_NAME} can't be renamed")));
        };
        if !entry.state.is_live() {
            return Err(refused(format!("{from} has ended")));
        }
        if from.name() == name {
            return Err(refuse(format!("{from} is already named {name}")));
        }
        check_name(name).map_err(refuse)?;
        tree.taken(&parent, name, Some(&from)).map_err(refuse)?;
        let to = parent.join(name);
        let mut after = tree.renames.clone();
        after.push((from.clone(), to.clone()));
        let strays = tree.entries.keys().chain(&tree.starting).find_map(|path| {
            let moved = path.moved(&from, &to)?;
            (moved.followed(&after) != moved).then_some(moved)
        });
        if let Some(moved) = strays {
            return Err(refuse(format!(
                "{from} can't take the name {name}: {moved} would then lead to another agent; choose another"
            )));
        }
        tree.renaming = Some(to.clone());
        Ok((from, to))
    }

    fn apply(
        &self,
        from: &AgentPath,
        to: &AgentPath,
        by: Option<&AgentPath>,
    ) -> Option<AgentEvent> {
        let (changed, parent) = {
            let mut tree = self.tree();
            tree.rename(from, to);
            let parent = to
                .parent()
                .filter(|p| by != Some(p) && tree.get(p).is_some_and(|e| e.state.is_live()));
            let now = self.now_ms();
            let changed = tree
                .entries
                .get_mut(to)
                .filter(|e| e.state.is_live())
                .map(|e| e.announce(now, &self.shared.news));
            (changed, parent)
        };
        if let Some(parent) = parent {
            let text = brief::renamed_text(from, to, by);
            let _ = self.post(to.clone(), &parent, MailKind::Notice, false, text);
        }
        changed
    }
}

struct Reserved<'a> {
    service: &'a AgentsService,
    path: Option<AgentPath>,
}

impl Reserved<'_> {
    fn release(mut self, tree: &mut Tree) {
        if let Some(path) = self.path.take() {
            let path = tree.resolve(&path);
            tree.starting.remove(&path);
        }
    }
}

impl Drop for Reserved<'_> {
    fn drop(&mut self) {
        if let Some(path) = self.path.take() {
            let mut tree = self.service.tree();
            let path = tree.resolve(&path);
            tree.starting.remove(&path);
        }
    }
}

enum Admission<T> {
    Ready(T),
    Refresh(String, String),
}

struct Plan {
    path: AgentPath,
    depth: u32,
    model: String,
    effort: Effort,
    cwd: PathBuf,
    tools: Vec<String>,
    todos: Vec<TodoItem>,
    target: Ready,
    fell_back: Option<String>,
    fallback_note: Option<String>,
    config: SpawnChoices,
    instructions: Option<PresetSource>,
}

fn choices(
    request: &SpawnRequest,
    preset: Option<&wisp_protocol::agents::Preset>,
    caller: &Caller,
) -> Vec<Choice> {
    let first = Choice {
        endpoint: request
            .endpoint
            .clone()
            .or_else(|| preset.and_then(|preset| preset.endpoint.clone()))
            .unwrap_or_else(|| caller.endpoint.clone()),
        model: request
            .model
            .clone()
            .or_else(|| preset.and_then(|preset| preset.model.clone())),
        effort: request
            .effort
            .or_else(|| preset.and_then(|preset| preset.effort)),
    };
    std::iter::once(first)
        .chain(
            preset
                .into_iter()
                .flat_map(|preset| preset.fallback.clone()),
        )
        .collect()
}

fn fallback<'v>(
    tree: &Tree,
    view: &'v EndpointView,
    name: &str,
    caller: &Caller,
) -> Option<&'v Ready> {
    let missing = tree.gone.contains_key(name)
        || (view.ready(name).is_none() && view.unavailable(name).is_none());
    let parent = view.ready(&caller.endpoint);
    let parent = parent.filter(|_| missing && !tree.gone.contains_key(&caller.endpoint))?;
    let from = tree.gone.get(name).copied().flatten();
    (!wisp_protocol::starts_charging(from, parent.billing())).then_some(parent)
}

fn target<'v>(
    tree: &Tree,
    view: &'v EndpointView,
    name: &str,
    concise: bool,
) -> Result<&'v Ready, String> {
    if tree.gone.contains_key(name) {
        return Err(if concise {
            format!("{} is no longer available", endpoints::plain(name))
        } else {
            endpoints::gone_text(name)
        });
    }
    if let Some(ready) = view.ready(name) {
        return Ok(ready);
    }
    if view.unavailable(name).is_some() {
        return Err(if concise {
            format!("{} can't be used right now", endpoints::plain(name))
        } else {
            endpoints::unavailable_text(name, &view.listed(&tree.gone))
        });
    }
    Err(if concise {
        format!("{} isn't set up", endpoints::plain(name))
    } else {
        endpoints::unknown_text(name, &view.listed(&tree.gone))
    })
}

pub(super) fn reach(me: &AgentPath, other: &AgentPath) -> Option<Reach> {
    let parent = me.parent();
    if other == me {
        None
    } else if parent.as_ref() == Some(other) {
        Some(Reach::Parent)
    } else if parent.is_some() && other.parent() == parent {
        Some(Reach::Sibling)
    } else if other.parent().as_ref() == Some(me) {
        Some(Reach::Child)
    } else if me.is_ancestor_of(other) {
        Some(Reach::Below)
    } else {
        None
    }
}

pub(super) fn address(me: &AgentPath, to: &str) -> Result<AgentPath, String> {
    let to = to.trim();
    match to {
        "" => Err("say which agent: `..`, a child's name, `../name` or a path".to_owned()),
        ".." => me
            .parent()
            .ok_or_else(|| format!("{ROOT_NAME} has no parent")),
        "/" | ROOT_NAME => Ok(AgentPath::root()),
        _ => match to.strip_prefix("../") {
            Some(name) => me
                .parent()
                .map(|parent| parent.join(name))
                .ok_or_else(|| format!("{ROOT_NAME} has no parent, so it has no siblings")),
            None if to.starts_with('/') => Ok(AgentPath::new(to.trim_end_matches('/'))),
            None => Ok(me.join(to)),
        },
    }
}

fn checklist_of(items: &[TodoItem]) -> Option<TodoProgress> {
    (!items.is_empty()).then(|| wisp_protocol::todos::progress(items))
}

fn resolve_cwd(given: Option<&str>, caller: &Caller) -> Result<PathBuf, String> {
    let base = caller.cwd.as_deref();
    let home = caller.home.as_deref();
    let (path, relative_to) = match given {
        None => {
            return base
                .map(Path::to_path_buf)
                .ok_or_else(|| "this agent has no working directory to give".to_owned());
        }
        Some(given) => match (given.strip_prefix('~'), home, base) {
            (Some(""), Some(home), _) => (home.to_owned(), None),
            (Some(rest), Some(home), _) if rest.starts_with('/') => {
                (home.join(rest.trim_start_matches('/')), None)
            }
            _ if Path::new(given).is_absolute() => (PathBuf::from(given), None),
            (_, _, Some(base)) => (base.join(given), Some(base)),
            (_, _, None) => (PathBuf::from(given), None),
        },
    };
    match (path.is_dir(), relative_to, given) {
        (true, ..) => Ok(path),
        (false, Some(base), Some(given)) => Err(format!(
            "cwd {given} isn't a directory (relative to {})",
            shorten_path(base, home)
        )),
        _ => Err(format!(
            "cwd {} isn't a directory",
            shorten_path(&path, home)
        )),
    }
}

impl AgentsHandle {
    pub(crate) fn opening_replay(&self) -> bool {
        self.service
            .as_ref()
            .is_some_and(|service| service.shared.opening_replay)
    }

    pub(crate) fn sessions_dir(&self) -> Option<std::path::PathBuf> {
        self.service.as_ref()?.shared.kit.logs.sessions_dir()
    }

    pub(crate) fn endpoint_host(&self) -> Option<Arc<dyn crate::endpoints::host::EndpointHost>> {
        self.service.as_ref()?.shared.kit.endpoint_host.clone()
    }

    pub fn none() -> Self {
        Self::default()
    }

    pub fn path(&self) -> AgentPath {
        self.path.borrow().clone()
    }

    pub(crate) fn fixed(path: AgentPath) -> Self {
        Self {
            service: None,
            path: watch::channel(path).1,
            caller: None,
        }
    }

    pub(crate) fn resolve(&self, path: &AgentPath) -> AgentPath {
        match &self.service {
            Some(service) => service.tree().resolve(path),
            None => path.clone(),
        }
    }

    pub fn in_tree(&self) -> bool {
        self.service.is_some()
    }

    #[must_use]
    pub fn for_call(&self, caller: Caller) -> Self {
        Self {
            caller: Some(Arc::new(caller)),
            ..self.clone()
        }
    }

    fn note(&self, event: AgentEvent) {
        if let Some(service) = &self.service {
            service.with_entry(&self.path(), |e| e.notes.push(event));
        }
    }

    pub(crate) fn refuse(&self, action: AgentAction, reason: String) -> String {
        self.note(AgentEvent::AgentRefused {
            by: self.path(),
            action,
            reason: reason.clone(),
        });
        reason
    }

    pub async fn spawn(
        &self,
        spawn: SpawnRequest,
        request: Option<RequestId>,
    ) -> Result<Spawned, String> {
        let (Some(service), Some(caller)) = (&self.service, &self.caller) else {
            return Err(self.refuse(
                AgentAction::Spawn,
                "this session has no agents, so it can't spawn one".to_owned(),
            ));
        };
        match service.spawn(&self.path(), caller, &spawn).await {
            Ok(spawned) => {
                self.note(AgentEvent::AgentSpawned {
                    request,
                    agent: spawned.entry.clone(),
                    session_id: spawned.session_id.clone(),
                    config: spawned.config.clone(),
                });
                Ok(spawned)
            }
            Err(reason) => Err(self.refuse(AgentAction::Spawn, reason)),
        }
    }

    pub fn report(&self, input: &Value) -> Result<String, String> {
        let Some(service) = &self.service else {
            return Err(self.refuse(
                AgentAction::Report,
                "only an agent with a parent reports".to_owned(),
            ));
        };
        let child = service
            .with_entry(&self.path(), |e| !e.path().is_root())
            .unwrap_or(false);
        if !child {
            return Err(self.refuse(
                AgentAction::Report,
                "only an agent with a parent reports".to_owned(),
            ));
        }
        match report::check(input) {
            Ok(text) => {
                service.with_entry(&self.path(), |e| e.turn.reported = Some(text));
                Ok("Reported.".to_owned())
            }
            Err(message) => {
                service.with_entry(&self.path(), |e| {
                    e.turn.invalid += 1;
                    e.turn.last_invalid = Some((input.clone(), message.clone()));
                });
                Err(self.refuse(AgentAction::Report, message))
            }
        }
    }

    pub fn send(
        &self,
        to: &str,
        text: String,
        wake: bool,
    ) -> Result<(AgentPath, Delivery), String> {
        let Some(service) = &self.service else {
            return Err(self.refuse(
                AgentAction::Send,
                "this session has no other agents".to_owned(),
            ));
        };
        if text.trim().is_empty() {
            return Err(self.refuse(AgentAction::Send, "text is empty".to_owned()));
        }
        let target = address(&self.path(), to).map_err(|r| self.refuse(AgentAction::Send, r))?;
        let target = self.resolve(&target);
        match service.send_from(&self.path(), &target, text, wake) {
            Ok((_, delivery)) => Ok((target, delivery)),
            Err(reason) => Err(self.refuse(AgentAction::Send, reason)),
        }
    }

    pub fn stop(&self, name: &str, end: bool) -> Result<(AgentPath, u32), String> {
        let Some(service) = &self.service else {
            return Err(self.refuse(
                AgentAction::Stop,
                "this session has no agents to stop".to_owned(),
            ));
        };
        let target = address(&self.path(), name).map_err(|r| self.refuse(AgentAction::Stop, r))?;
        let target = self.resolve(&target);
        if !self.path().is_ancestor_of(&target) {
            return Err(self.refuse(
                AgentAction::Stop,
                format!(
                    "{} isn't below you: you can only stop your children and the agents below them",
                    shown_agent(&target)
                ),
            ));
        }
        match service.stop_tree(&target, Some(&self.path()), end) {
            Ok((below, ended)) => {
                for event in ended {
                    self.note(event);
                }
                Ok((target, below))
            }
            Err(reason) => Err(self.refuse(AgentAction::Stop, reason)),
        }
    }

    pub(crate) fn end_agent(&self, target: &AgentPath) -> Result<(), (RejectKind, String)> {
        if target.is_root() {
            return Err((
                RejectKind::Other(REFUSED.to_owned()),
                format!("{ROOT_NAME} can't be ended; quit the session to end it"),
            ));
        }
        let Some(service) = self.service.as_ref().filter(|_| self.path().is_root()) else {
            return Err(refused(format!("there is no agent {target}")));
        };
        let (_, ended) = service.stop_tree(target, None, true).map_err(refused)?;
        for event in ended {
            self.note(event);
        }
        Ok(())
    }

    pub fn rename(&self, name: &str, new_name: &str) -> Result<(AgentPath, AgentPath), String> {
        let target =
            address(&self.path(), name).map_err(|r| self.refuse(AgentAction::Rename, r))?;
        self.rename_agent(&target, new_name, None)
            .map_err(|(_, reason)| self.refuse(AgentAction::Rename, reason))
    }

    pub(crate) fn rename_agent(
        &self,
        target: &AgentPath,
        name: &str,
        request: Option<RequestId>,
    ) -> Result<(AgentPath, AgentPath), (RejectKind, String)> {
        let Some(service) = self.service.as_ref().filter(|_| self.path().is_root()) else {
            return Err(refused(format!("there is no agent {target}")));
        };
        let (from, to) = service.rename(target, name)?;
        self.note(AgentEvent::AgentRenamed {
            request,
            agent: from.clone(),
            to: to.clone(),
        });
        Ok((from, to))
    }

    pub(crate) fn apply(
        &self,
        from: &AgentPath,
        to: &AgentPath,
        by: Option<&AgentPath>,
    ) -> Option<AgentEvent> {
        self.service.as_ref()?.apply(from, to, by)
    }

    pub(crate) fn watch(&self, target: &AgentPath, live: bool) -> Result<(), (RejectKind, String)> {
        if target.is_root() {
            return Err(refused(format!(
                "{ROOT_NAME} can't be watched; its own output is always shown"
            )));
        }
        let Some(service) = self.service.as_ref().filter(|_| self.path().is_root()) else {
            return Err(refused(format!("there is no agent {target}")));
        };
        service.watch(target, live).map_err(refused)
    }

    pub(crate) fn asked_by(&self, asker: &AgentPath) {
        if let Some(service) = &self.service {
            let mut tree = service.tree();
            let asker = tree.resolve(asker);
            if let Some(entry) = tree.get_mut(&self.path()) {
                entry.questions.insert(asker);
            }
        }
    }

    pub(crate) fn take_unanswered(&self) -> Vec<AgentPath> {
        let Some(service) = &self.service else {
            return Vec::new();
        };
        let mut tree = service.tree();
        let askers = tree
            .get_mut(&self.path())
            .map(|e| std::mem::take(&mut e.questions))
            .unwrap_or_default();
        askers
            .into_iter()
            .filter(|asker| tree.get(asker).is_some_and(|e| e.waits_on(&self.path())))
            .collect()
    }

    pub fn reachable(&self) -> Vec<(Reach, AgentEntry)> {
        let Some(service) = &self.service else {
            return Vec::new();
        };
        let tree = service.tree();
        let mut reached: Vec<(Reach, AgentEntry)> = tree
            .entries
            .values()
            .filter(|e| e.state.is_live())
            .filter_map(|e| reach(&self.path(), &e.path()).map(|r| (r, e.shown())))
            .collect();
        reached.sort_by(|(_, a), (_, b)| a.path.tree_cmp(&b.path));
        let me = tree.get(&self.path()).map(|e| (Reach::Caller, e.shown()));
        me.into_iter().chain(reached).collect()
    }

    pub(crate) fn take_notes(&self) -> Vec<AgentEvent> {
        self.service
            .as_ref()
            .and_then(|s| s.with_entry(&self.path(), |e| std::mem::take(&mut e.notes)))
            .unwrap_or_default()
    }

    pub(crate) fn begin_turn(&self) {
        if let Some(service) = &self.service {
            service.with_entry(&self.path(), |e| {
                e.turn = TurnSignals::default();
                e.stop = None;
                e.asking.clear();
            });
        }
    }

    pub(crate) fn begin_calls(&self) {
        if let Some(service) = &self.service {
            service.with_entry(&self.path(), |e| {
                e.turn.asked = false;
                e.asking.clear();
            });
        }
    }

    pub(crate) fn note_request(&self) {
        if let Some(service) = &self.service {
            let mut tree = service.tree();
            let path = tree.resolve(&self.path());
            let seq = tree.next_seq();
            if let Some(entry) = tree.get_mut(&path) {
                entry.request_seq = seq;
                entry.halted_reply = false;
            }
        }
    }

    pub(crate) fn take_reply_halted(&self) -> bool {
        self.service
            .as_ref()
            .and_then(|s| s.with_entry(&self.path(), |e| std::mem::take(&mut e.halted_reply)))
            .unwrap_or(false)
    }

    pub fn signals(&self) -> TurnSignals {
        self.service
            .as_ref()
            .and_then(|s| s.with_entry(&self.path(), |e| e.turn.clone()))
            .unwrap_or_default()
    }

    pub(crate) fn restarted(&self) -> bool {
        self.service.as_ref().is_some_and(|s| s.shared.restarted)
    }

    pub(crate) fn wake_when_stuck(&self) -> Option<Duration> {
        let minutes = self.service.as_ref()?.shared.limits.wake_when_stuck;
        (minutes > 0).then(|| Duration::from_secs(u64::from(minutes) * 60))
    }

    pub(crate) fn idle_since(&self) -> Option<Instant> {
        let service = self.service.as_ref()?;
        service
            .tree()
            .entries
            .values()
            .filter(|e| !e.path().is_root())
            .try_fold(service.shared.started, |since, e| {
                (!e.state.is_busy()).then(|| since.max(e.idle_at.unwrap_or(since)))
            })
    }

    pub(crate) fn keep_warm(&self) -> Option<u32> {
        let service = self.service.as_ref()?;
        let limit = service.shared.limits.warm_refreshes;
        service
            .with_entry(&self.path(), |e| e.config.keep_warm && e.stop.is_none())
            .filter(|marked| *marked && limit > 0)
            .map(|_| limit)
    }

    pub(crate) fn warm_changed(
        &self,
        refreshes: Option<u32>,
        paused: Option<bool>,
    ) -> Option<AgentEvent> {
        let service = self.service.as_ref()?;
        service.with_entry(&self.path(), |e| {
            let warm = e.warm.as_mut()?;
            let refreshes = refreshes.unwrap_or(warm.refreshes);
            let paused = paused.unwrap_or(warm.paused);
            if (warm.refreshes, warm.paused) == (refreshes, paused) {
                return None;
            }
            warm.refreshes = refreshes;
            warm.paused = paused;
            e.warm_changed()
        })?
    }

    pub(crate) fn stop_cause(&self) -> Option<StopCause> {
        self.service
            .as_ref()
            .and_then(|s| s.with_entry(&self.path(), |e| e.stop))
            .flatten()
    }

    pub(crate) fn children_owing(&self) -> bool {
        let Some(service) = &self.service else {
            return false;
        };
        service.tree().entries.values().any(|e| {
            e.path().parent().as_ref() == Some(&self.path()) && e.owes && still_working(e.state)
        })
    }

    pub(crate) fn set_owes(&self, owes: bool) {
        if let Some(service) = &self.service {
            service.with_entry(&self.path(), |e| e.owes = owes);
        }
    }

    pub(crate) fn set_split(
        &self,
        billing: wisp_protocol::Billing,
        plan_usd: Option<f64>,
        accounts: BTreeMap<String, wisp_protocol::AccountSpend>,
    ) {
        if let Some(service) = &self.service {
            service.with_entry(&self.path(), |entry| {
                entry.billing = billing;
                entry.plan_usd = plan_usd;
                entry.accounts = accounts;
            });
        }
    }

    pub(crate) fn change(&self, change: Change, spent: Option<(f64, bool)>) -> Option<AgentEvent> {
        self.service
            .as_ref()
            .and_then(|s| s.change(&self.path(), change, spent, None))
    }

    pub(crate) fn settle_idle(
        &self,
        outcome: Option<IdleOutcome>,
        processes: Vec<u32>,
        spent: (f64, bool),
    ) -> Option<AgentEvent> {
        let activity = (!processes.is_empty()).then(|| Activity::Waiting {
            processes: processes.into(),
        });
        let service = self.service.as_ref()?;
        service.change(
            &self.path(),
            Change::Settled(outcome),
            Some(spent),
            activity,
        )
    }

    pub(crate) fn mail_parent(&self, kind: MailKind, wake: bool, text: String) -> bool {
        self.path()
            .parent()
            .is_some_and(|parent| self.mail_to(&parent, kind, wake, text))
    }

    pub(crate) fn mail_to(&self, to: &AgentPath, kind: MailKind, wake: bool, text: String) -> bool {
        let Some(service) = &self.service else {
            return false;
        };
        service.post(self.path(), to, kind, wake, text).is_ok()
    }

    pub(crate) fn notice(&self, from: AgentPath, text: &str) -> Option<Mail> {
        let service = self.service.as_ref()?;
        Some(Mail {
            id: service.next_mail_id(),
            from,
            to: self.path(),
            kind: MailKind::Notice,
            wake: true,
            text: text.to_owned(),
        })
    }

    pub(crate) fn close_tree(&self) {
        if let Some(service) = self.service.as_ref().filter(|_| self.path().is_root()) {
            service.close();
        }
    }

    pub(crate) fn set_context(&self, event: &AgentEvent) {
        if let (Some(service), Some(fill)) = (&self.service, ContextFill::of(event)) {
            service.with_entry(&self.path(), |e| e.context = Some(fill));
        }
    }

    pub(crate) fn restore_endpoint(&self, name: &str) {
        if let Some(service) = &self.service {
            service.tree().gone.remove(name);
        }
    }

    pub(crate) fn on_accounts(&self, names: &[String]) -> Vec<OnAccount> {
        let Some(service) = &self.service else {
            return Vec::new();
        };
        service
            .tree()
            .entries
            .iter()
            .filter(|(path, entry)| !path.is_root() && entry.commands.is_some())
            .filter_map(|(path, entry)| {
                let account = entry.place.endpoint.clone()?;
                names.contains(&account).then(|| OnAccount {
                    agent: path.as_str().trim_start_matches('/').to_owned(),
                    account,
                    provider: entry.place.provider.clone(),
                    working: !matches!(
                        entry.state,
                        AgentState::Idle { .. } | AgentState::Held { .. }
                    ),
                    leave: entry.leave.clone(),
                })
            })
            .collect()
    }

    pub(crate) fn presets_on(&self, names: &[String]) -> (Vec<(String, String)>, Vec<String>) {
        let Some(service) = &self.service else {
            return (Vec::new(), Vec::new());
        };
        let presets = service.shared.presets.usable().into_iter();
        let presets = presets.filter(|p| p.fallback.is_empty()).filter_map(|p| {
            let account = p.endpoint.clone().filter(|e| names.contains(e))?;
            Some((p.name.clone(), account))
        });
        let tree = service.tree();
        let live = tree.entries.iter();
        let live = live.filter(|(path, entry)| !path.is_root() && entry.state.is_live());
        let places = live.filter_map(|(_, entry)| entry.place.endpoint.clone());
        let places = places.filter(|account| !names.contains(account));
        (presets.collect(), places.collect())
    }

    pub(crate) fn forget_endpoint(&self, name: &str, billing: Option<wisp_protocol::Billing>) {
        if let Some(service) = &self.service {
            service.tree().gone.insert(name.to_owned(), billing);
        }
    }

    pub(crate) fn account_kind(&self, path: &AgentPath) -> Option<&'static str> {
        self.service
            .as_ref()
            .and_then(|s| s.tree().get(path).and_then(|e| e.kind))
    }

    pub(crate) fn display_effort(&self, entry: &AgentEntry) -> Option<Effort> {
        let row = self.service.as_ref().and_then(|service| {
            let endpoint = entry.endpoint.as_deref()?;
            let connected = service
                .shared
                .kit
                .connect
                .connect(entry.path.as_str())
                .ok()?;
            EndpointView::connected(&connected, endpoint)
                .ready(endpoint)?
                .row(&entry.model)
        });
        let own = crate::models::ModelTable::shared()
            .find(&entry.model)
            .and_then(|row| row.effort_variant.as_ref());
        wisp_protocol::settings::display_effort(own, row.map(|r| r.efforts), Some(entry.effort))
    }

    pub(crate) fn endpoint_gone(&self, name: &str) -> bool {
        self.service
            .as_ref()
            .is_some_and(|s| s.tree().gone.contains_key(name))
    }

    pub(crate) fn settle(&self, settled: Settled) {
        if let Some(service) = &self.service {
            service.with_entry(&self.path(), |e| {
                e.model = settled.model;
                e.effort = settled.effort;
                e.place = Place {
                    endpoint: Some(settled.endpoint),
                    provider: Some(settled.provider.to_owned()),
                    priced: settled.priced,
                };
                e.kind = Some(settled.kind);
            });
        }
    }

    pub(crate) fn set_last_report(&self, line: Option<String>) {
        if let Some(service) = &self.service {
            service.with_entry(&self.path(), |e| e.last_report = line);
        }
    }

    pub(crate) fn route(
        &self,
        target: &AgentPath,
        command: ClientCommand,
    ) -> Result<(), (RejectKind, String)> {
        match &self.service {
            Some(service) => service.route(target, command),
            None => Err(refused(format!("there is no agent {target}"))),
        }
    }

    pub(crate) async fn window_gone(&self) {
        if !self.path().is_root() {
            return;
        }
        let Some(service) = &self.service else {
            return;
        };
        let commands: Vec<_> = {
            let mut tree = service.tree();
            tree.entries
                .values_mut()
                .filter(|entry| !entry.path().is_root())
                .filter_map(|entry| {
                    entry.watched = false;
                    entry.commands.clone()
                })
                .collect()
        };
        for commands in commands {
            let _ = commands.send(ClientCommand::WindowGone).await;
        }
    }

    pub(crate) fn warm_now(&self) -> Vec<AgentEvent> {
        self.service
            .as_ref()
            .map(|service| {
                service
                    .tree()
                    .entries
                    .values()
                    .filter_map(Entry::warm_changed)
                    .collect()
            })
            .unwrap_or_default()
    }

    pub(crate) fn live_processes(&self) -> Vec<AgentEvent> {
        let Some(service) = &self.service else {
            return Vec::new();
        };
        service
            .tree()
            .entries
            .values()
            .flat_map(|entry| {
                let path = entry.path();
                entry
                    .processes
                    .started_events()
                    .into_iter()
                    .map(move |event| {
                        if path.is_root() {
                            event
                        } else {
                            AgentEvent::Agent {
                                agent: path.clone(),
                                event: Box::new(event),
                            }
                        }
                    })
            })
            .collect()
    }

    pub(crate) fn snapshot(&self) -> Vec<AgentEntry> {
        self.service
            .as_ref()
            .map(AgentsService::snapshot)
            .unwrap_or_default()
    }

    pub(crate) fn news(&self) -> Option<Arc<News>> {
        self.service.as_ref().map(|s| s.shared.news.clone())
    }

    pub fn identity(&self) -> Option<AgentEntry> {
        let service = self.service.as_ref()?;
        service.tree().get(&self.path()).map(Entry::bare)
    }

    pub fn root_session_id(&self) -> Option<String> {
        let service = self.service.as_ref()?;
        let tree = service.tree();
        tree.get(&AgentPath::root())
            .map(|root| root.session_id.clone())
    }

    pub fn nodes(&self) -> Vec<AgentEntry> {
        let Some(service) = &self.service else {
            return Vec::new();
        };
        let mut nodes: Vec<AgentEntry> = service
            .tree()
            .entries
            .values()
            .filter(|e| !e.path().is_root())
            .map(Entry::bare)
            .collect();
        nodes.sort_by(|a, b| a.path.tree_cmp(&b.path));
        nodes
    }
}

pub fn start_root(
    kit: Kit,
    mut setup: TreeSetup,
    agent: Agent,
) -> (
    mpsc::Sender<ClientCommand>,
    mpsc::Receiver<AgentEvent>,
    AgentTask,
) {
    let (commands, mut command_rx) = mpsc::channel(ROOT_COMMANDS);
    let (events, mut stream) = mpsc::channel(ROOT_EVENTS);
    let (host, event_rx) = mpsc::channel(ROOT_EVENTS);
    let loaded = std::mem::take(&mut setup.loaded);
    let service = AgentsService::new(Arc::new(kit), setup, events.clone(), agent.opening_replay());
    service.register_root(&agent);
    let warm = service
        .tree()
        .get(&AgentPath::root())
        .and_then(Entry::warm_changed);
    if let Some(event) = warm {
        let _ = events.try_send(event);
    }
    let caller = agent.caller();
    let preset = agent
        .main_preset()
        .map(|p| shorten_path(&p.path, caller.home.as_deref()));
    let lines = service.environment(&AgentPath::root(), &caller.tools, preset.as_deref(), false);
    let mut agent = agent.with_agents(service.handle(AgentPath::root()), lines);
    let _ = events.try_send(agent.spend_now());
    let carry_on = service.shared.restarted && agent.carry_on(loaded.interrupted);
    service.start_loaded(loaded);
    if carry_on {
        let path = AgentPath::root();
        let _ = service.post(
            path.clone(),
            &path,
            MailKind::Notice,
            true,
            brief::RESTARTED.to_owned(),
        );
    }
    service.watch_bound(agent.queue_watch());
    let worst = service.shared.worst.subscribe();
    let observer = service.clone();
    let forward = tokio::spawn(async move {
        loop {
            let event = tokio::select! {
                biased;
                event = stream.recv() => match event {
                    Some(event) => Some(event),
                    None => break,
                },
                () = observer.shared.recount.notified() => None,
            };
            let Some(event) = event else {
                if let Some(report) = observer.report() {
                    let _ = host.send(report).await;
                }
                continue;
            };
            let initial = matches!(&event, AgentEvent::Restart {
                state: wisp_protocol::RestartState::GettingReady, open,
            } if open.is_empty());
            let recount = initial || Watch::reads(&event);
            if initial {
                let mut tree = observer.tree();
                tree.restart_pending = false;
                tree.restart_rows = None;
            } else {
                observer.observe(&AgentPath::root(), &event);
                let _ = host.send(event).await;
            }
            if recount && let Some(report) = observer.report() {
                let _ = host.send(report).await;
            }
        }
    });
    let tree = service.clone();
    let handle = tokio::spawn(async move {
        agent.serve(&mut command_rx, &events).await;
        drop(agent);
        tree.close();
        let handles = tree.take_handles();
        drop(events);
        for handle in handles {
            let _ = handle.await;
        }
        let _ = forward.await;
    });
    drop(service);
    (commands, event_rx, AgentTask { handle, worst })
}

mod limits;
mod load;
mod restart;

pub use load::{ChangedPreset, LoadedTree, load_tree};

#[cfg(test)]
mod tests;
