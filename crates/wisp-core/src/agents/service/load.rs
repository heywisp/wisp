use std::collections::{BTreeMap, BTreeSet};
use std::sync::atomic::Ordering;
use std::sync::{Arc, OnceLock};
use std::time::Duration;

use serde::{Deserialize, Serialize};
use tokio::sync::{Notify, mpsc, watch};
use wisp_protocol::agents::Preset;
use wisp_protocol::log::{LogEvent, LogRecord};
use wisp_protocol::{
    AccountSpend, AgentPath, AgentState, ContextFill, IdleOutcome, MailKind, ParentLink,
    PresetChange,
};

use super::{
    AgentsService, CHILD_COMMANDS, Entry, Place, TreeSetup, brief, checklist_of, inherited_tools,
};
use crate::agent::{Agent, MailSender};
use crate::agents::endpoints::{EndpointView, ExactAccount, LoggedKind};
use crate::agents::fold::{
    EndedRow, Folded, Links, LoggedEnd, SpawnedRecord, ended_above, ended_row, fold, links,
};
use crate::endpoints::goes::{shown, starts_charging};
use crate::kit::{
    ChildResume, ChildSpec, InstructionsChoice, Kit, OpenLog, PresetSource, ReopenedLog, Resume,
    ended_lines, logged_root,
};
use crate::provider::Provider;
use crate::resume::{LoggedSession, billing_mode, carried, recover};
use crate::runtime::in_parallel;
use crate::session::{ResumeLock, Stamp, sha256_bytes_hex};
use wisp_protocol::subscription_auth::{CANT_BE_READ, Followed, Stayed, Unusable};

pub(super) struct Known {
    pub(super) spawned: SpawnedRecord,
    pub(super) folded: Folded,
    pub(super) spent: (f64, bool),
    pub(super) plan_usd: Option<f64>,
    pub(super) billing: wisp_protocol::Billing,
    pub(super) accounts: BTreeMap<String, AccountSpend>,
    pub(super) ended_at: Option<String>,
}

pub(super) struct Live {
    pub(super) known: Known,
    pub(super) agent: Agent,
}

pub(super) struct Unloaded {
    path: AgentPath,
    cause: String,
    owes: bool,
}

#[derive(Default)]
pub struct LoadedTree {
    pub(super) interrupted: bool,
    pub(super) live: Vec<Live>,
    pub(super) ended: Vec<Known>,
    pub(super) unloaded: Vec<Unloaded>,
    pub(super) mail_ids: u64,
    pub(super) renames: Vec<(AgentPath, AgentPath)>,
    pub warnings: Vec<String>,
    pub placed: Vec<String>,
    pub moved: Vec<Followed>,
    pub stay: Vec<Stayed>,
    pub preset_changes: Vec<ChangedPreset>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ChangedPreset {
    pub name: String,
    pub change: PresetChange,
    pub agents: Vec<AgentPath>,
}

impl ChangedPreset {
    pub fn warning(&self) -> String {
        let how = match self.change {
            PresetChange::Disabled => "is disabled",
            PresetChange::Removed => "was removed",
            PresetChange::Edited | PresetChange::Unknown => "has changed",
        };
        let agents: Vec<&str> = self.agents.iter().map(AgentPath::as_str).collect();
        let one = agents.len() == 1;
        format!(
            "preset {} {how}; {} keep{} the configuration {} spawned with, and new spawns use the preset as it is now",
            self.name,
            agents.join(", "),
            if one { "s" } else { "" },
            if one { "it was" } else { "they were" },
        )
    }
}

impl LoadedTree {
    pub fn live(&self) -> Vec<&AgentPath> {
        self.live.iter().map(|l| &l.known.spawned.path).collect()
    }

    pub fn ended(&self) -> Vec<&AgentPath> {
        self.ended.iter().map(|k| &k.spawned.path).collect()
    }
}

impl std::fmt::Debug for LoadedTree {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("LoadedTree")
            .field("live", &self.live())
            .field("ended", &self.ended())
            .field("warnings", &self.warnings)
            .field("preset_changes", &self.preset_changes)
            .finish_non_exhaustive()
    }
}

fn parent_link(records: &[LogRecord]) -> Option<ParentLink> {
    match &records.iter().find(|r| !carried(&r.event))?.event {
        LogEvent::SessionStarted { parent, .. } => parent.as_deref().cloned(),
        _ => None,
    }
}

struct Walk<'a> {
    kit: &'a Kit,
    setup: &'a TreeSetup,
    root: &'a Agent,
    lock: ResumeLock,
    ended: BTreeMap<AgentPath, LoggedEnd>,
    tree: LoadedTree,
    rebuilt: Vec<Option<f64>>,
    follow: &'a [String],
}

type EndedSpend = (
    (f64, bool),
    Option<f64>,
    wisp_protocol::Billing,
    BTreeMap<String, AccountSpend>,
);

fn own_spend(records: &[LogRecord]) -> Option<EndedSpend> {
    let logged = LoggedSession::read(records).ok()?;
    let lower_bound = logged.lower_bound || logged.repaired_turn().is_some();
    Some((
        (logged.session_usd, lower_bound),
        (!logged.plan_unknown).then_some(logged.plan_usd),
        crate::resume::billing(records).unwrap_or_default(),
        logged.accounts,
    ))
}

fn end_spend(logged_end: Option<&LoggedEnd>) -> EndedSpend {
    let (plan_usd, billing) =
        logged_end.map_or((None, wisp_protocol::Billing::Metered), LoggedEnd::split);
    let usd = logged_end.map_or(0.0, |end| end.spent_usd);
    ((usd, true), plan_usd, billing, BTreeMap::new())
}

#[derive(Clone, Default, PartialEq, Serialize, Deserialize)]
pub(super) struct EndedLog {
    pub(super) spend: Option<EndedSpend>,
    pub(super) row: EndedRow,
    pub(super) links: Links,
}

pub(super) fn ended_log(records: &[LogRecord]) -> EndedLog {
    EndedLog {
        spend: own_spend(records),
        row: ended_row(records),
        links: links(records),
    }
}

pub(super) const FIXTURES: [&str; 4] = [
    include_str!("kept/whole.jsonl"),
    include_str!("kept/broken.jsonl"),
    include_str!("kept/reported.jsonl"),
    include_str!("kept/typed.jsonl"),
];

pub(super) fn kept_version() -> Option<&'static str> {
    static VERSION: OnceLock<Option<String>> = OnceLock::new();
    VERSION
        .get_or_init(|| {
            let facts: Vec<_> = FIXTURES
                .iter()
                .map(|log| ended_lines(log.as_bytes()).map(|records| ended_log(&records)))
                .collect::<Result<_, _>>()
                .ok()?;
            Some(sha256_bytes_hex(&serde_json::to_vec(&facts).ok()?))
        })
        .as_deref()
}

pub(super) type Kept = BTreeMap<String, (Stamp, EndedLog)>;

pub(super) fn read_kept(logs: &dyn OpenLog, root: &str) -> Kept {
    let Some(text) = logs.ended(root) else {
        return Kept::new();
    };
    let mut lines = text.lines();
    if lines.next() != kept_version() {
        return Kept::new();
    }
    lines
        .filter_map(|line| serde_json::from_str(line).ok())
        .collect()
}

pub(super) fn keep(logs: &dyn OpenLog, root: &str, facts: &Kept) {
    let Some(version) = kept_version() else {
        return;
    };
    let mut text = format!("{version}\n");
    for entry in facts {
        let Ok(line) = serde_json::to_string(&entry) else {
            return;
        };
        text.push_str(&line);
        text.push('\n');
    }
    logs.keep_ended(root, &text);
}

pub(super) fn ended_facts(
    logs: &dyn OpenLog,
    spawned: &SpawnedRecord,
    kept: &Kept,
) -> (Option<Stamp>, Option<EndedLog>) {
    let id = &spawned.session_id;
    let stamp = logs.stamp(id);
    let log = kept
        .get(id)
        .filter(|(known, _)| Some(*known) == stamp)
        .map(|(_, log)| log.clone())
        .or_else(|| logs.read(id).ok().map(|records| ended_log(&records)));
    (stamp, log)
}

fn preset_changed(now: &Preset, spawned: &SpawnedRecord) -> bool {
    match &spawned.config.preset_values {
        Some(then) => {
            let tools = |p: &Preset| p.tools_off.iter().cloned().collect::<BTreeSet<_>>();
            (
                &now.model,
                now.effort,
                now.spawn,
                &now.instructions,
                tools(now),
                &now.endpoint,
            ) != (
                &then.model,
                then.effort,
                then.spawn,
                &then.instructions,
                tools(then),
                &then.endpoint,
            )
        }
        None => {
            now.model.as_ref().is_some_and(|m| *m != spawned.model)
                || now.effort.is_some_and(|e| e != spawned.effort)
                || now.spawn.is_some_and(|s| s != spawned.config.spawn)
                || now.instructions.is_some() != spawned.config.instructions.is_some()
                || now
                    .endpoint
                    .as_ref()
                    .is_some_and(|e| spawned.endpoint.as_ref() != Some(e))
                || now
                    .tools_off
                    .iter()
                    .any(|t| !spawned.config.tools_off.contains(t))
        }
    }
}

impl Walk<'_> {
    fn learn(&mut self, found: Links, next: &mut Vec<(SpawnedRecord, String)>, parent: &str) {
        self.tree.renames.extend(found.renames);
        let renames = &self.tree.renames;
        for (path, end) in found.ended {
            self.ended.insert(path.followed(renames), end);
        }
        self.tree.mail_ids = self.tree.mail_ids.max(found.mail);
        next.extend(found.spawned.into_iter().map(|mut s| {
            s.path = s.path.followed(renames);
            (s, parent.to_owned())
        }));
    }

    fn read_ended(
        &mut self,
        spawned: SpawnedRecord,
        log: Option<EndedLog>,
        next: &mut Vec<(SpawnedRecord, String)>,
    ) {
        let session = spawned.session_id.clone();
        let EndedLog { spend, row, links } = log.unwrap_or_default();
        let known = self.known(spawned, spend, row.into());
        self.learn(links, next, &session);
        self.tree.ended.push(known);
    }

    fn known(&self, spawned: SpawnedRecord, spend: Option<EndedSpend>, folded: Folded) -> Known {
        let end = self.ended.get(&spawned.path);
        let (spent, plan_usd, billing, accounts) = spend.unwrap_or_else(|| end_spend(end));
        let ended_at = end.map(|e| e.at.clone());
        Known {
            spawned,
            folded,
            spent,
            plan_usd,
            billing,
            accounts,
            ended_at,
        }
    }

    fn not_loaded(&mut self, spawned: SpawnedRecord, why: &str, cause: &str) {
        self.tree.warnings.push(not_resumed(&spawned.path, why));
        self.unload(spawned, cause);
    }

    fn unload(&mut self, spawned: SpawnedRecord, cause: &str) {
        let read = self.kit.logs.read(&spawned.session_id).ok();
        let renames = &self.tree.renames;
        let owes = read
            .as_deref()
            .is_some_and(|records| fold(&spawned.path, records, renames).owes);
        let log = read.as_deref().map(ended_log).unwrap_or_default();
        let unseen_children = !log.links.spawned.is_empty();
        self.tree.unloaded.push(Unloaded {
            path: spawned.path.clone(),
            cause: cause.to_owned(),
            owes,
        });
        let mut known = self.known(spawned, log.spend, log.row.into());
        known.spent.1 |= unseen_children;
        self.tree.ended.push(known);
    }

    async fn load(
        &mut self,
        spawned: SpawnedRecord,
        parent_session: &str,
        next: &mut Vec<(SpawnedRecord, String)>,
    ) {
        let path = spawned.path.clone();
        let reopened = match self.kit.logs.reopen(&spawned.session_id, self.lock) {
            Ok(reopened) => reopened,
            Err(e) => {
                return self.not_loaded(
                    spawned,
                    &format!("its log can't be opened ({e})"),
                    UNREADABLE,
                );
            }
        };
        let ReopenedLog {
            log,
            lines,
            truncated_bytes,
            unlocked: _,
        } = reopened;
        let connected = match self.kit.connect.connect(path.as_str()) {
            Ok(connected) => connected,
            Err(e) => return self.not_loaded(spawned, &e, "its accounts couldn't be connected"),
        };
        let providers = |name: Option<&str>| -> Option<Arc<dyn Provider>> {
            let wanted = name.unwrap_or(&connected.active);
            connected
                .ready
                .iter()
                .find(|c| c.name == wanted)
                .map(|c| c.provider.clone())
                .or_else(|| Some(connected.provider.clone()))
        };
        let mut recovered = match recover(lines, log.store(), &providers) {
            Ok(recovered) if !recovered.fresh() => recovered,
            Ok(_) => {
                return self.not_loaded(
                    spawned,
                    "its log can't be read from its start",
                    UNREADABLE,
                );
            }
            Err(e) => {
                return self.not_loaded(
                    spawned,
                    &format!("its log can't be read ({e})"),
                    UNREADABLE,
                );
            }
        };
        let (logged, rebuilt) = recovered.take_checked();
        let records = recovered.records.as_slice();
        let link = parent_link(records);
        let renames = &self.tree.renames;
        let at = |l: &ParentLink| l.agent.followed(renames) == path;
        let Some(link) = link.filter(|l| at(l) && l.session_id == parent_session) else {
            let why = "its log names another parent";
            return self.not_loaded(spawned, why, why);
        };
        let logged = match logged {
            Ok(logged) => logged,
            Err(e) => {
                return self.not_loaded(
                    spawned,
                    &format!("its log can't be read ({e})"),
                    UNREADABLE,
                );
            }
        };
        let kind = LoggedKind::read(
            logged
                .current
                .provider
                .as_deref()
                .or(spawned.provider.as_deref()),
        );
        if kind == LoggedKind::Unknown {
            return self.not_loaded(
                spawned,
                "it ran on a provider this Wisp doesn't know (a newer Wisp wrote its log)",
                "its log names a provider this Wisp doesn't know",
            );
        }
        let caller = self.root.caller();
        let view = EndpointView::connected(&connected, &caller.endpoint);
        let agent_name = path.as_str().trim_start_matches('/').to_owned();
        let consented = self.follow.contains(&agent_name);
        let start = self.setup.start.as_deref().unwrap_or(&caller.endpoint);
        let saved_billing = logged
            .endpoint()
            .and_then(|name| billing_mode(records, name));
        let mut placement = ChildResume::Legacy;
        let mut pair = (logged.current.model.as_str(), logged.current.effort);
        let selected = logged
            .pending
            .as_ref()
            .and_then(|p| p.destination().map(|to| (p, to)));
        let exact = selected.and_then(|(pending, (name, provider))| {
            let mut selected = ExactAccount {
                name: name.to_owned(),
                kind: LoggedKind::read(Some(provider)),
                billing: pending.billing,
            };
            let Some(ready) = view.exact(&selected) else {
                self.tree.warnings.push(format!(
                    "{path}'s selected {} is no longer available as chosen",
                    named(name)
                ));
                placement = ChildResume::Rejected;
                return None;
            };
            selected.billing = Some(ready.billing());
            let origin = logged.endpoint().map(|origin| ExactAccount {
                name: origin.to_owned(),
                kind,
                billing: Some(saved_billing.unwrap_or_default()),
            });
            if let Some(source) = origin
                && view.exact(&source).is_some()
            {
                let name = source.name.clone();
                placement = ChildResume::Pending {
                    source,
                    destination: selected,
                };
                return Some(Placed::There(name));
            }
            placement = ChildResume::Unbound {
                source: None,
                destination: selected,
            };
            pair = (&pending.model, pending.effort);
            Some(Placed::There(name.to_owned()))
        });
        let (model, effort) = pair;
        let placed = exact.unwrap_or_else(|| {
            placed(
                &view,
                Logged {
                    name: logged.endpoint(),
                    model,
                    kind,
                    unpriced: logged.lower_bound,
                    billing: saved_billing.unwrap_or_default(),
                },
                (&caller.endpoint, start),
                (consented, &self.setup.unusable),
            )
        });
        let (endpoint, followed) = match placed {
            Placed::There(name) => (name, None),
            Placed::Fallback(name, said, from) => {
                self.tree.warnings.push(format!("{path} {said}"));
                self.tree.placed.push(format!("{path} {said}"));
                let to = from.is_some().then(|| name.clone());
                (name, Some((from, to)))
            }
            Placed::Unusable { cause, from } => {
                self.tree.stay.push(Stayed {
                    agent: agent_name,
                    account: from.account.clone(),
                    from: Some(from),
                    ..Stayed::default()
                });
                return self.unload(spawned, &cause);
            }
            Placed::NotLoaded {
                why,
                cause,
                stayed: Some(charges),
            } => {
                self.tree.placed.push(not_resumed(&path, &why));
                self.tree.stay.push(Stayed {
                    agent: agent_name,
                    charges,
                    account: logged.endpoint().unwrap_or_default().to_owned(),
                    ..Stayed::default()
                });
                return self.not_loaded(spawned, &why, &cause);
            }
            Placed::NotLoaded { why, cause, .. } => return self.not_loaded(spawned, &why, &cause),
        };
        let home = caller.home.clone();
        let cwd = logged
            .started
            .cwd
            .as_deref()
            .or(spawned.cwd.as_deref())
            .map(|cwd| logged_root(cwd, home.as_deref()));
        let Some(cwd) = cwd else {
            let why = "its log names no working directory";
            return self.not_loaded(spawned, why, why);
        };
        if !cwd.is_dir() {
            self.tree.warnings.push(format!(
                "{path} ran in {}, which no longer exists; its workspace tools say so",
                logged.started.cwd.as_deref().unwrap_or_default()
            ));
        }
        let preset_instructions = link
            .config
            .instructions
            .as_deref()
            .map(|file| PresetSource {
                preset: link.config.preset.clone().unwrap_or_default(),
                path: logged_root(file, home.as_deref()),
            });
        let parent = self.parent_of(parent_session);
        let allowed: BTreeSet<String> = inherited_tools(
            &parent.caller(),
            &link.config.tools_off,
            link.config.spawn,
            &self.setup.followers,
        )
        .into_iter()
        .collect();
        let names = self.kit.preload(link.config.preset_values.as_deref());
        let selected = parent.catalog().preloaded(
            &allowed,
            &names,
            parent.toolbox().provider_of(crate::tool::SEARCH_CAPABILITY),
        );
        let held: BTreeSet<String> = allowed.difference(&selected).cloned().collect();
        let logged_tools = match placement {
            ChildResume::Pending { .. } | ChildResume::Rejected => logged.last_tools(),
            _ => &logged.tools,
        };
        let tools = logged_tools
            .names
            .iter()
            .cloned()
            .chain(held)
            .collect::<BTreeSet<_>>()
            .into_iter()
            .collect();
        let spec = ChildSpec {
            path: path.clone(),
            model: model.to_owned(),
            effort,
            cache_ttl: logged.current.cache_ttl,
            message_time: logged.started.message_time,
            context_note: logged.started.context_note,
            instructions: InstructionsChoice {
                source: logged
                    .started
                    .instructions_source
                    .clone()
                    .unwrap_or_else(|| self.setup.instructions.source.clone()),
                warn_tokens: self.setup.instructions.warn_tokens,
            },
            cwd,
            tools,
            parent: link,
            endpoint,
            preset_instructions,
            plugins: crate::plugin::PluginSwitches {
                off: std::collections::BTreeSet::new(),
                removed: &removed_ids(&logged)
                    | &self.parent_of(parent_session).caller().plugins.removed,
            },
        };
        let folded = fold(&path, records, &self.tree.renames);
        let found = links(records);
        let resume = Resume {
            log,
            recovered: &recovered,
            truncated_bytes,
            logged: Some(&logged),
            rebuilt,
        };
        let (agent, missing) = match self
            .kit
            .resume_connected_child(spec, resume, connected, placement)
            .await
        {
            Ok(built) => built,
            Err(e) => {
                return self.not_loaded(
                    spawned,
                    &format!("it couldn't be built ({e})"),
                    "it couldn't be built",
                );
            }
        };
        let adopted = agent.adopted_copy();
        let gone = if adopted {
            "it's turned off from its next message"
        } else {
            "it resumes without it"
        };
        for tool in missing
            .into_iter()
            .filter(|tool| !adopted || agent.still_offers(tool))
        {
            self.tree.warnings.push(format!(
                "{path}'s tool {tool} isn't provided by any plugin now, so {gone}"
            ));
        }
        if let Some((from, to)) = followed {
            self.tree.moved.push(Followed {
                agent: agent_name,
                model: model.to_owned(),
                effort,
                rebuild_usd: agent.cache_rebuild().and_then(|r| r.usd),
                from,
                to,
            });
        }
        if let Some(rebuild) = agent.cache_rebuild() {
            self.rebuilt.push(rebuild.usd);
        } else if agent.rebuild_due() {
            self.rebuilt.push(None);
        }
        let (usd, lower, accounts) = agent.lifetime_spend();
        let plan_usd = agent.session_plan_split();
        let crashed = logged.repaired_turn().is_some();
        let agent = agent.with_owes(folded.owes);
        let session = spawned.session_id.clone();
        self.learn(found, next, &session);
        self.tree.live.push(Live {
            known: Known {
                spawned,
                folded,
                spent: (usd, lower || crashed),
                plan_usd,
                billing: crate::resume::billing(records).unwrap_or_default(),
                accounts,
                ended_at: None,
            },
            agent,
        });
    }

    fn rebuild_warning(&mut self) {
        let count = self.rebuilt.len();
        if count == 0 {
            return;
        }
        let cost = rebuild_cost(&self.rebuilt);
        let rebuilds = if count == 1 {
            "1 agent rebuilds its cache on its next request".to_owned()
        } else {
            format!("{count} agents rebuild their cache on their next request")
        };
        self.tree.warnings.push(format!(
            "Wisp's prompt or tools changed since this tree ran: {rebuilds}{cost}, dropping earlier reasoning; the conversations are kept"
        ));
    }

    fn preset_changes(&mut self) {
        let mut by_preset: BTreeMap<&str, Vec<&SpawnedRecord>> = BTreeMap::new();
        for live in &self.tree.live {
            if let Some(name) = &live.known.spawned.config.preset {
                by_preset
                    .entry(name.as_str())
                    .or_default()
                    .push(&live.known.spawned);
            }
        }
        let presets = &self.setup.presets;
        let mut changes = Vec::new();
        for (name, agents) in by_preset {
            let changed: Vec<AgentPath> = match presets.find(name) {
                Some(preset) => agents
                    .iter()
                    .filter(|a| preset_changed(preset, a))
                    .map(|a| a.path.clone())
                    .collect(),
                None => agents.iter().map(|a| a.path.clone()).collect(),
            };
            if changed.is_empty() {
                continue;
            }
            let change = if presets.find(name).is_some() {
                PresetChange::Edited
            } else if presets
                .off()
                .iter()
                .any(|o| o.name.as_deref() == Some(name))
            {
                PresetChange::Disabled
            } else {
                PresetChange::Removed
            };
            changes.push(ChangedPreset {
                name: name.to_owned(),
                change,
                agents: changed,
            });
        }
        self.tree.preset_changes = changes;
    }
}

pub async fn load_tree(
    kit: &Kit,
    setup: &TreeSetup,
    root: &Agent,
    records: &[LogRecord],
    lock: ResumeLock,
    follow: &[String],
) -> LoadedTree {
    let mut walk = Walk {
        kit,
        setup,
        root,
        lock,
        ended: BTreeMap::new(),
        tree: LoadedTree {
            interrupted: fold(&AgentPath::root(), records, &[]).interrupted,
            ..LoadedTree::default()
        },
        rebuilt: Vec::new(),
        follow,
    };
    let kept = read_kept(kit.logs.as_ref(), root.session_id());
    let mut facts = Kept::new();
    let mut level = Vec::new();
    walk.learn(links(records), &mut level, root.session_id());
    while !level.is_empty() {
        level.sort_by(|(a, _), (b, _)| a.path.tree_cmp(&b.path));
        let mut next = Vec::new();
        let logs = walk.kit.logs.as_ref();
        let read = in_parallel(&level, |(spawned, _)| {
            ended_above(&walk.ended, &spawned.path).then(|| ended_facts(logs, spawned, &kept))
        });
        for ((spawned, parent_session), log) in std::mem::take(&mut level).into_iter().zip(read) {
            if ended_above(&walk.ended, &spawned.path) {
                let (stamp, log) = log.unwrap_or_else(|| ended_facts(logs, &spawned, &kept));
                if let Some(stamp) = stamp
                    && let Some(log) = &log
                    && log.spend.is_some()
                {
                    facts.insert(spawned.session_id.clone(), (stamp, log.clone()));
                }
                walk.read_ended(spawned, log, &mut next);
            } else {
                walk.load(spawned, &parent_session, &mut next).await;
            }
        }
        level = next;
    }
    if facts != kept {
        keep(kit.logs.as_ref(), root.session_id(), &facts);
    }
    walk.preset_changes();
    walk.rebuild_warning();
    walk.tree
}

impl Known {
    fn entry(
        self,
        path: watch::Sender<AgentPath>,
        state: AgentState,
        mail: MailSender,
        granted: Arc<Notify>,
        now_ms: u64,
        view: &EndpointView,
    ) -> Entry {
        let Known {
            spawned,
            folded,
            spent,
            plan_usd,
            billing,
            accounts,
            ended_at,
        } = self;
        let settled = if state.is_live() {
            folded.since
        } else {
            ended_at
        };
        let state_since_ms = settled
            .and_then(|at| at.parse::<jiff::Timestamp>().ok())
            .and_then(|at| u64::try_from(at.as_millisecond()).ok())
            .unwrap_or(now_ms);
        let place = Place {
            priced: spawned
                .endpoint
                .as_deref()
                .is_none_or(|name| view.priced(name)),
            provider: LoggedKind::read(spawned.provider.as_deref())
                .shown()
                .map(str::to_owned),
            endpoint: spawned.endpoint,
        };
        Entry {
            cwd: spawned.cwd,
            config: spawned.config,
            spent_usd: spent.0,
            plan_usd: plan_usd.filter(|_| !billing.is_metered()),
            billing,
            lower_bound: spent.1,
            accounts,
            last_report: folded.last_report,
            checklist: checklist_of(&folded.checklist),
            owes: folded.owes,
            granted,
            announced: spent,
            peers: folded.peers,
            questions: folded.questions,
            asking: folded.asking,
            ran: folded.running_ms.map(Duration::from_millis),
            ..Entry::new(
                path,
                spawned.session_id,
                spawned.depth,
                spawned.model,
                spawned.effort,
                state,
                mail,
                place,
                state_since_ms,
            )
        }
    }
}

impl AgentsService {
    pub(super) fn start_loaded(&self, loaded: LoadedTree) {
        let LoadedTree {
            live,
            ended,
            unloaded,
            mail_ids,
            renames,
            preset_changes,
            ..
        } = loaded;
        self.shared.mail_ids.fetch_max(mail_ids, Ordering::SeqCst);
        let now = self.now_ms();
        let own = self
            .tree()
            .entries
            .get(&AgentPath::root())
            .and_then(|e| e.place.endpoint.clone())
            .unwrap_or_default();
        let view = self
            .shared
            .kit
            .connect
            .connect(crate::kit::ROOT)
            .map(|c| EndpointView::connected(&c, &own))
            .unwrap_or_default();
        let mut entries = Vec::new();
        for known in ended {
            let cell = watch::channel(known.spawned.path.clone()).0;
            let entry = known.entry(
                cell,
                AgentState::Ended,
                MailSender::closed(),
                Arc::default(),
                now,
                &view,
            );
            entries.push(entry);
        }
        let mut children = Vec::new();
        let mut notices = Vec::new();
        for Live { mut known, agent } in live {
            let path = known.spawned.path.clone();
            let news = agent.hooks_word_news();
            let lines = self.environment(
                &path,
                &agent.caller().tools,
                known.spawned.config.instructions.as_deref(),
                news,
            );
            let folded = &mut known.folded;
            if !self.shared.restarted
                && folded.owes
                && news
                && matches!(
                    folded.outcome,
                    Some(IdleOutcome::Waiting | IdleOutcome::Asked)
                )
                && agent.restart_news()
            {
                folded.outcome = Some(IdleOutcome::Waiting);
                folded.asking.clear();
                notices.push((path.clone(), brief::unwoken_text(&path)));
            }
            let granted = Arc::new(Notify::new());
            let (cell, cell_rx) = watch::channel(path.clone());
            let mut child = agent
                .with_agents(self.handle_on(cell_rx.clone()), lines)
                .with_permit_signal(granted.clone());
            let (commands, command_rx) = mpsc::channel(CHILD_COMMANDS);
            let carry_on = self.shared.restarted && child.carry_on(known.folded.interrupted);
            if carry_on {
                notices.push((path.clone(), brief::RESTARTED.to_owned()));
            } else if !self.shared.restarted && known.folded.interrupted {
                notices.push((path.clone(), brief::interrupted_text(&path)));
            }
            let state = AgentState::Idle {
                outcome: known.folded.outcome,
            };
            let mut entry = known.entry(cell, state, child.mail_sender(), granted, now, &view);
            entry.worker = self.shared.presets.worker(entry.config.preset.as_deref());
            let session = child.session();
            entry.model = session.model.clone();
            entry.effort = session.effort;
            entry.place = Place {
                endpoint: child.bound_endpoint().map(str::to_owned),
                provider: Some(child.provider_kind().as_str().to_owned()),
                priced: child.is_priced(),
            };
            entry.kind = Some(child.account_kind());
            entry.commands = Some(commands);
            entry.processes = child.processes();
            entry.leave = Some(child.leave_sender());
            entry.context = ContextFill::of(&child.context_now());
            entry.warm = self.warm_at_start(entry.config.keep_warm && child.keeps_warm(), true);
            entry.preset_change = preset_changes
                .iter()
                .find(|c| c.agents.contains(&path))
                .map(|c| c.change);
            entries.push(entry);
            children.push((cell_rx, child, command_rx));
        }
        let root_events = {
            let mut tree = self.tree();
            for entry in entries {
                tree.entries.insert(entry.path(), entry);
            }
            tree.renames = renames;
            tree.events.clone()
        };
        for (path, text) in notices {
            let (to, wake) = if self.shared.restarted {
                (path.clone(), true)
            } else {
                let Some(parent) = path.parent() else {
                    continue;
                };
                let wake = !parent.is_root();
                (parent, wake)
            };
            let _ = self.post(path, &to, MailKind::Notice, wake, text);
        }
        for Unloaded { path, cause, owes } in unloaded {
            let Some(parent) = path.parent() else {
                continue;
            };
            let wake = owes && !parent.is_root();
            let text = brief::not_loaded_text(&path, &cause);
            let _ = self.post(path, &parent, MailKind::Notice, wake, text);
        }
        for (path, child, command_rx) in children {
            let bound = child.queue_watch();
            let handles = self.run_child(path, child, command_rx, root_events.clone());
            self.tree().handles.extend(handles);
            self.watch_bound(bound);
        }
    }
}

const UNREADABLE: &str = "its log can't be opened or read";

struct Logged<'a> {
    name: Option<&'a str>,
    model: &'a str,
    kind: LoggedKind,
    unpriced: bool,
    billing: wisp_protocol::Billing,
}

enum Placed {
    There(String),
    Fallback(String, String, Option<Unusable>),
    NotLoaded {
        why: String,
        cause: String,
        stayed: Option<bool>,
    },
    Unusable {
        cause: String,
        from: Unusable,
    },
}

enum Moved<'a> {
    Unavailable(&'a str),
    Gone,
    Another,
    Billing,
}

fn named(name: &str) -> String {
    crate::endpoints::goes::named(crate::agents::endpoints::plain(name))
}

fn not_resumed(path: &AgentPath, why: &str) -> String {
    format!("{path} wasn't resumed: {why}; it stays ended for this run")
}

fn its(name: &str) -> String {
    let named = named(name);
    let bare = named.strip_prefix("the ").unwrap_or(&named);
    format!("its {bare}")
}

fn placed(
    view: &EndpointView,
    logged: Logged<'_>,
    (root, start): (&str, &str),
    (consented, unusable): (bool, &[Unusable]),
) -> Placed {
    let model = wisp_protocol::shown_model(logged.model);
    let takes = |ready: &crate::agents::endpoints::Ready| logged.kind.takes(ready, logged.model);
    let charges = |ready: &crate::agents::endpoints::Ready| {
        starts_charging(Some(logged.billing), ready.billing())
    };
    let fits =
        |ready: &crate::agents::endpoints::Ready| takes(ready) && (consented || !charges(ready));
    let Some(name) = logged.name else {
        return match view.ready(root).filter(|r| fits(r)) {
            Some(_) => Placed::There(root.to_owned()),
            None => Placed::NotLoaded {
                why: format!(
                    "its log names no account, and {} can't take its history",
                    shown(root)
                ),
                cause: "its log names no account".to_owned(),
                stayed: None,
            },
        };
    };
    let moved = match view.ready(name) {
        Some(ready) => match logged.kind {
            LoggedKind::Known(kind) if kind != ready.kind() => Moved::Another,
            _ if charges(ready) && !consented => Moved::Billing,
            _ => return Placed::There(ready.name.clone()),
        },
        None => view
            .unavailable(name)
            .map_or(Moved::Gone, Moved::Unavailable),
    };
    let ran_on = named(name);
    let said = match moved {
        Moved::Unavailable(reason) => format!("which is unavailable now ({reason})"),
        Moved::Gone => "which isn't configured now".to_owned(),
        Moved::Another => "which is another provider now".to_owned(),
        Moved::Billing => "which bills differently now".to_owned(),
    };
    let from = match moved {
        Moved::Unavailable(_) => Some(
            unusable
                .iter()
                .find(|u| u.account == name)
                .cloned()
                .unwrap_or_else(|| Unusable {
                    account: name.to_owned(),
                    state: CANT_BE_READ.to_owned(),
                    sign_in: false,
                }),
        ),
        _ => None,
    };
    let root = if from.is_some() { start } else { root };
    let root_shown = shown(root);
    let fallback = view
        .ready(root)
        .filter(|r| takes(r) && ((consented && from.is_none()) || !charges(r)));
    if let Some(fallback) = fallback {
        let was_unpriced = match moved {
            Moved::Another => !view.priced(name),
            Moved::Billing | Moved::Unavailable(_) | Moved::Gone => logged.unpriced,
        };
        let metered = if was_unpriced && fallback.priced {
            "; it is metered now"
        } else {
            ""
        };
        return Placed::Fallback(
            root.to_owned(),
            format!(
                "ran on {ran_on}, {said}; it continues on {root_shown} with model {model}{metered}"
            ),
            from,
        );
    }
    let its = its(name);
    if let Some(from) = from {
        return Placed::Unusable {
            cause: format!("{its} is unavailable"),
            from,
        };
    }
    let charges = view.ready(root).is_some_and(takes);
    let cannot = match logged.kind {
        _ if charges => format!("{root_shown} bills differently"),
        LoggedKind::Unrecorded => format!("{root_shown} doesn't serve its model {model}"),
        LoggedKind::Known(_) | LoggedKind::Unknown => format!("{root_shown} is another provider"),
    };
    let cause = match moved {
        Moved::Unavailable(_) => format!("{its} is unavailable"),
        Moved::Gone => format!("{its} isn't configured"),
        Moved::Another => format!("{its} is another provider now"),
        Moved::Billing => format!("{its} bills differently now"),
    };
    let why = if name == root {
        format!("it ran on {ran_on}, {said}")
    } else {
        format!("it ran on {ran_on}, {said}, and {cannot}")
    };
    let stayed = matches!(moved, Moved::Gone).then_some(charges);
    Placed::NotLoaded { why, cause, stayed }
}

impl Walk<'_> {
    fn parent_of(&self, session: &str) -> &Agent {
        self.tree
            .live
            .iter()
            .find(|l| l.known.spawned.session_id == session)
            .map_or(self.root, |l| &l.agent)
    }
}

fn removed_ids(logged: &LoggedSession) -> std::collections::BTreeSet<String> {
    logged
        .session_values
        .iter()
        .filter_map(|v| match &v.target {
            wisp_protocol::settings::SettingTarget::Path(path)
                if path.spec().name == wisp_protocol::settings::PLUGIN_INSTALLED =>
            {
                let wisp_protocol::settings::Place::Plugin(id) = &path.place else {
                    return None;
                };
                v.value
                    .as_ref()
                    .is_some_and(crate::settings::is_off)
                    .then(|| id.clone())
            }
            _ => None,
        })
        .collect()
}

pub(super) fn rebuild_cost(rebuilt: &[Option<f64>]) -> String {
    let priced: Vec<f64> = rebuilt.iter().flatten().copied().collect();
    if priced.is_empty() {
        return String::new();
    }
    let amount = wisp_protocol::format_usd(priced.iter().sum());
    format!(
        " ({})",
        wisp_protocol::marked_amount(&amount, true, priced.len() < rebuilt.len())
    )
}
