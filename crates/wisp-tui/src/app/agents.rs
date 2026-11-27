use std::collections::BTreeMap;
use std::time::Duration;

use serde_json::Value;
use wisp_protocol::agents::shown_agent;
use wisp_protocol::todos::progress;
use wisp_protocol::{
    AccountSpend, Activity, AgentEntry, AgentEvent, AgentPath, AgentState, ContextFill, HoldReason,
    IdleOutcome, InputId, KeepWarm, RequestId, WakeSource,
};
use wisp_render::chrome::{AgentRow, EndpointFacts, Facts, Status, one_line, timed};
use wisp_render::transcript::{
    AgentLink, Config, Detail, Transcript, View as TranscriptView, copy,
};

use super::inputs::{Applied, Inputs, Stage};
use super::picker::display_effort;
use super::settings::Tracker;
use super::view::View;
use crate::widgets::composer::{Composer, Saved};

#[derive(Clone, Debug, Default)]
pub struct Agents {
    rows: Vec<AgentRow>,
    hold: Option<HoldReason>,
    main_warm: Option<KeepWarm>,
    main_spend: (AccountSpend, BTreeMap<String, AccountSpend>),
    renames: Vec<(AgentPath, AgentPath)>,
}

pub fn report_line(report: &Value) -> Option<String> {
    let text = match report {
        Value::String(text) => text.as_str(),
        Value::Object(map) => map.get("summary").and_then(Value::as_str)?,
        _ => return None,
    };
    text.lines()
        .map(str::trim)
        .find(|l| !l.is_empty())
        .map(one_line)
}

fn shown(mut entry: AgentEntry) -> AgentEntry {
    entry.name = one_line(&entry.name);
    entry.session_id = one_line(&entry.session_id);
    entry.last_report = entry.last_report.as_deref().map(one_line);
    entry.cwd = entry.cwd.as_deref().map(one_line);
    entry.preset = entry.preset.as_deref().map(one_line);
    entry.provider = entry.provider.as_deref().map(one_line);
    if let Activity::Tool { name } = &mut entry.activity {
        *name = one_line(name);
    }
    entry
}

pub fn agent_facts(facts: &Facts, entry: &AgentEntry, listed: Option<&[EndpointFacts]>) -> Facts {
    Facts {
        model: entry.model.clone(),
        effort: entry.effort.as_str().to_owned(),
        endpoint_name: entry.endpoint.clone(),
        provider: (entry.provider.clone()).unwrap_or_else(|| facts.provider.clone()),
        endpoints: listed.map_or_else(|| facts.endpoints.clone(), <[EndpointFacts]>::to_vec),
        next: None,
        ..facts.clone()
    }
}

fn find(rows: &[AgentRow], path: &AgentPath) -> Result<usize, usize> {
    rows.binary_search_by(|row| row.entry.path.tree_cmp(path))
}

impl Agents {
    fn put(&mut self, entry: AgentEntry, facts: &Facts) {
        if entry.path.is_root() {
            return;
        }
        let mut entry = shown(entry);
        let old = self.row(&entry.path);
        if let Some(old) = old.map(|row| &row.entry) {
            if entry.spent_usd < old.spent_usd {
                entry.spent_usd = old.spent_usd;
                entry.plan_usd = old.plan_usd;
                entry.billing = old.billing;
                entry.lower_bound = old.lower_bound;
                entry.accounts.clone_from(&old.accounts);
            } else {
                entry.lower_bound |= old.lower_bound;
            }
        }
        let last_outcome = match entry.state {
            AgentState::Idle {
                outcome: Some(outcome),
            } => Some(outcome),
            AgentState::Ended => match old {
                Some(old) => old.last_outcome,
                None => entry.last_report.as_ref().map(|_| IdleOutcome::Reported),
            },
            _ => None,
        };
        let last_mail = old.and_then(|row| row.last_mail.clone());
        self.place(AgentRow {
            effort: display_effort(facts, &entry.model, Some(entry.effort)),
            entry,
            last_outcome,
            last_mail,
        });
    }

    fn place(&mut self, row: AgentRow) {
        match find(&self.rows, &row.entry.path) {
            Ok(i) => self.rows[i] = row,
            Err(i) => self.rows.insert(i, row),
        }
    }

    pub fn apply(&mut self, event: &AgentEvent, now_ms: u64, facts: &Facts) {
        match event {
            AgentEvent::AgentsSnapshot { agents, .. } => {
                let old = std::mem::take(&mut self.rows);
                for entry in agents {
                    if let Some(row) = find(&old, &entry.path).ok().and_then(|i| old.get(i)) {
                        self.place(row.clone());
                    }
                    self.put(entry.clone(), facts);
                }
            }
            AgentEvent::AgentSpawned { agent, .. } => self.put(agent.clone(), facts),
            AgentEvent::AgentStateChanged {
                plan_usd,
                billing,
                agent,
                state,
                activity,
                spent_usd,
                lower_bound,
                accounts,
                since_ms,
            } => {
                if let Some(row) = self.row(agent) {
                    let mut entry = row.entry.clone();
                    entry.state = *state;
                    entry.activity = activity.clone();
                    entry.spent_usd = *spent_usd;
                    entry.billing = *billing;
                    entry.plan_usd = *plan_usd;
                    entry.lower_bound = *lower_bound;
                    entry.accounts.clone_from(accounts);
                    entry.since_ms = *since_ms;
                    self.put(entry, facts);
                }
            }
            AgentEvent::SpendChanged { total, accounts } => {
                self.main_spend = (*total, accounts.clone());
            }
            AgentEvent::KeepWarmChanged { agent, keep_warm } => {
                if agent.is_root() {
                    self.main_warm = Some(*keep_warm);
                } else if let Some(row) = self.row_mut(agent) {
                    row.entry.keep_warm = Some(*keep_warm);
                }
            }
            AgentEvent::AgentReported { agent, report, .. } => {
                if let Some(row) = self.row_mut(agent)
                    && let Some(line) = report.as_ref().and_then(report_line)
                {
                    row.entry.last_report = Some(line);
                }
            }
            AgentEvent::Agent { agent, event } => self.wrapped(agent, event, now_ms),
            AgentEvent::WakeHeld { sources, reason } => {
                if sources.iter().any(|s| matches!(s, WakeSource::Mail { .. })) {
                    self.hold = Some(*reason);
                }
            }
            AgentEvent::TurnStarted { .. } => self.hold = None,
            _ => {}
        }
    }

    fn wrapped(&mut self, agent: &AgentPath, event: &AgentEvent, now_ms: u64) {
        let Some(row) = self.row_mut(agent) else {
            return;
        };
        match event {
            AgentEvent::ContextChanged { .. } => row.entry.context = ContextFill::of(event),
            AgentEvent::TodosUpdated { items, .. } => {
                row.entry.checklist = (!items.is_empty()).then(|| progress(items));
            }
            AgentEvent::InputAccepted {
                from: Some(from), ..
            } => {
                let label = copy::mail_label(from.kind, !from.wake, shown_agent(&from.agent));
                row.last_mail = Some((label, now_ms));
            }
            _ => {}
        }
    }

    pub fn renamed(&mut self, from: &AgentPath, to: &AgentPath) {
        self.renames.push((from.clone(), to.clone()));
        for entry in self.rows.iter_mut().map(|row| &mut row.entry) {
            if let Some(path) = entry.path.moved(from, to) {
                entry.name = path.name().to_owned();
                entry.parent = path.parent();
                entry.path = path;
            }
        }
        self.rows
            .sort_by(|a, b| a.entry.path.tree_cmp(&b.entry.path));
    }

    pub fn followed(&self, event: &AgentEvent) -> Option<AgentEvent> {
        if self.renames.is_empty() {
            return None;
        }
        let mut event = event.clone();
        let follow = |path: &mut AgentPath| *path = path.followed(&self.renames);
        match &mut event {
            AgentEvent::Agent { agent, .. }
            | AgentEvent::AgentStateChanged { agent, .. }
            | AgentEvent::KeepWarmChanged { agent, .. }
            | AgentEvent::AgentReported { agent, .. } => follow(agent),
            AgentEvent::AgentSpawned { agent, .. } => {
                follow(&mut agent.path);
                agent.parent.iter_mut().for_each(follow);
            }
            _ => return None,
        }
        Some(event)
    }

    fn old_names(&self, path: &AgentPath) -> impl Iterator<Item = &str> {
        let renames = &self.renames;
        (renames.iter().enumerate())
            .filter(move |(i, (from, _))| {
                from.depth() == 1 && from.followed(&renames[*i..]) == *path
            })
            .map(|(_, (from, _))| from.name())
    }

    pub fn main_spend(&self) -> AccountSpend {
        self.main_spend.0
    }

    pub(super) fn spends(&self) -> impl Iterator<Item = &BTreeMap<String, AccountSpend>> {
        let rows = self.rows.iter().map(|row| &row.entry.accounts);
        std::iter::once(&self.main_spend.1).chain(rows)
    }

    pub fn rows(&self) -> &[AgentRow] {
        &self.rows
    }

    pub fn row(&self, agent: &AgentPath) -> Option<&AgentRow> {
        self.rows.get(find(&self.rows, agent).ok()?)
    }

    fn row_mut(&mut self, agent: &AgentPath) -> Option<&mut AgentRow> {
        let i = find(&self.rows, agent).ok()?;
        self.rows.get_mut(i)
    }

    pub fn main_warm(&self) -> Option<KeepWarm> {
        self.main_warm
    }

    pub fn busy(&self) -> usize {
        self.rows.iter().filter(|r| r.entry.state.is_busy()).count()
    }

    pub fn any_live(&self) -> bool {
        self.rows.iter().any(AgentRow::is_live)
    }

    pub fn mail_hold(&self) -> Option<HoldReason> {
        self.hold
    }

    pub fn links(&self, now_ms: u64, sep: &str) -> Vec<(String, AgentLink)> {
        self.rows
            .iter()
            .filter(|r| r.entry.depth == 1)
            .flat_map(|row| {
                let entry = &row.entry;
                let report = match (row.last_outcome, &entry.last_report) {
                    (Some(outcome), Some(line)) => Some(copy::reported_line(
                        outcome.as_str(),
                        Some(line.as_str()),
                        sep,
                    )),
                    _ => None,
                };
                let state = match entry.state {
                    AgentState::Idle { .. } if report.is_some() => {
                        copy::agent_state(AgentState::Idle { outcome: None })
                    }
                    _ => timed(row, now_ms),
                };
                let link = AgentLink {
                    state,
                    cwd: entry.cwd.clone(),
                    report,
                };
                let names = std::iter::once(entry.name.as_str()).chain(self.old_names(&entry.path));
                names.map(move |name| (name.to_owned(), link.clone()))
            })
            .collect()
    }
}

pub struct Model {
    pub transcript: Transcript,
    pub inputs: Inputs,
    pub composer: Composer,
    pub status: Status,
    pub view: View,
    pub tracker: Tracker,
    pub listed: Option<Vec<EndpointFacts>>,
    pub runs: BTreeMap<RequestId, u64>,
    pub plugin_echo: BTreeMap<RequestId, (String, wisp_protocol::Secret)>,
}

impl Model {
    fn take(&mut self, event: &AgentEvent, at: Duration) -> Applied {
        self.transcript.apply(event, at);
        self.inputs.apply(event, &mut self.transcript)
    }

    fn kept(mut self) -> Vec<Saved> {
        let mut kept: Vec<Saved> = (self.inputs.tracked().iter())
            .filter(|t| matches!(t.stage, Stage::Sent | Stage::Waiting | Stage::Held))
            .map(|t| t.saved.clone())
            .collect();
        kept.extend(self.draft());
        kept
    }

    fn draft(&mut self) -> Option<Saved> {
        let draft = self.composer.set_aside(&Saved::default());
        (!draft.text().trim().is_empty()).then_some(draft)
    }
}

pub(super) struct Child {
    pub(super) agent: AgentPath,
    pub(super) unconfirmed: Vec<(InputId, Saved)>,
    pub(super) texts: Vec<Saved>,
}

impl Child {
    pub(super) fn kept(self) -> (AgentPath, Vec<Saved>) {
        let mut texts: Vec<_> = self
            .unconfirmed
            .into_iter()
            .map(|(_, saved)| saved)
            .collect();
        texts.extend(self.texts);
        (self.agent, texts)
    }
}

pub enum Change {
    Applied(AgentPath, Applied),
    Lost(Vec<(AgentPath, Vec<Saved>)>),
}

pub struct Models {
    reset_time: std::sync::Arc<dyn Fn(i64) -> String + Send + Sync>,
    config: Config,
    home: Option<String>,
    transcript_view: TranscriptView,
    live: BTreeMap<AgentPath, Model>,
    carried: Vec<(AgentPath, Vec<Saved>)>,
}

impl Models {
    pub fn clear_plugin_echo(&mut self) {
        for model in self.live.values_mut() {
            model.plugin_echo.clear();
        }
    }

    pub fn new(config: Config, home: Option<String>) -> Models {
        Models {
            reset_time: std::sync::Arc::new(|_| copy::TIME_UNAVAILABLE.into()),
            config,
            home,
            transcript_view: TranscriptView::default(),
            live: BTreeMap::new(),
            carried: Vec::new(),
        }
    }

    pub fn set_view(&mut self, view: TranscriptView) {
        let view = TranscriptView {
            detail: Detail::Full,
            ..view
        };
        self.transcript_view = view;
        for model in self.live.values_mut() {
            model.transcript.set_view(view);
            model.view.rewrap();
        }
    }

    pub fn with_reset_time(mut self, time: impl Fn(i64) -> String + Send + Sync + 'static) -> Self {
        self.reset_time = std::sync::Arc::new(time);
        self
    }

    fn end(&mut self, agent: &AgentPath) -> Option<(AgentPath, Vec<Saved>)> {
        let model = self.live.remove(agent)?;
        Some((agent.clone(), model.kept()))
    }

    pub(super) fn hold(&mut self, children: impl IntoIterator<Item = (AgentPath, Vec<Saved>)>) {
        self.carried.extend(children);
    }

    pub(super) fn unsent(&mut self) -> Vec<Child> {
        let mut children: Vec<_> = std::mem::take(&mut self.carried)
            .into_iter()
            .map(|(agent, texts)| Child {
                agent,
                unconfirmed: Vec::new(),
                texts,
            })
            .collect();
        children.extend(self.live.iter_mut().map(|(agent, model)| Child {
            agent: agent.clone(),
            unconfirmed: model.inputs.take_sent(),
            texts: model.draft().into_iter().collect(),
        }));
        children.retain(|child| !child.unconfirmed.is_empty() || !child.texts.is_empty());
        children
    }

    fn open_all(&mut self, entries: &[AgentEntry], facts: &Facts) -> Option<Change> {
        let lost: Vec<_> = entries.iter().filter_map(|e| self.open(e, facts)).collect();
        (!lost.is_empty()).then_some(Change::Lost(lost))
    }

    fn open(&mut self, entry: &AgentEntry, facts: &Facts) -> Option<(AgentPath, Vec<Saved>)> {
        if entry.path.is_root() {
            return None;
        }
        if !entry.state.is_live() {
            return self.end(&entry.path);
        }
        let model = self.live.entry(entry.path.clone()).or_insert_with(|| {
            let mut tracker = Tracker::new(&agent_facts(facts, entry, None));
            if let Some(fill) = &entry.context {
                tracker.sized(fill.used);
            }
            Model {
                transcript: Transcript::new(self.config.clone())
                    .for_child()
                    .with_view(self.transcript_view)
                    .with_home(self.home.clone())
                    .with_reset_time({
                        let time = self.reset_time.clone();
                        move |at| time(at)
                    }),
                inputs: Inputs::new(),
                composer: Composer::new().with_placeholder(copy::message_to(entry.path.as_str())),
                status: Status::new(),
                view: View::new(0),
                tracker,
                listed: None,
                runs: BTreeMap::new(),
                plugin_echo: BTreeMap::new(),
            }
        });
        if let Some(index) = self
            .carried
            .iter()
            .position(|(agent, _)| *agent == entry.path)
        {
            let (_, texts) = self.carried.remove(index);
            model.composer.restore_ahead(&texts);
        }
        model.transcript.endpoint(entry.endpoint.as_deref());
        None
    }

    pub fn apply(&mut self, event: &AgentEvent, at: Duration, facts: &Facts) -> Option<Change> {
        match event {
            AgentEvent::AgentsSnapshot { agents, .. } => {
                let mut ended = match self.open_all(agents, facts) {
                    Some(Change::Lost(ended)) => ended,
                    _ => Vec::new(),
                };
                ended.append(&mut self.carried);
                (!ended.is_empty()).then_some(Change::Lost(ended))
            }
            AgentEvent::AgentSpawned { agent, .. } => {
                self.open_all(std::slice::from_ref(agent), facts)
            }
            AgentEvent::AgentStateChanged {
                agent,
                state: AgentState::Ended,
                ..
            } => self.end(agent).map(|lost| Change::Lost(vec![lost])),
            AgentEvent::Agent { agent, event } => {
                let model = self.live.get_mut(agent)?;
                Some(Change::Applied(agent.clone(), model.take(event, at)))
            }
            AgentEvent::Rejected { id: Some(id), .. } => {
                let (agent, model) = self
                    .live
                    .iter_mut()
                    .find(|(_, m)| m.inputs.tracked_by(id).is_some())?;
                Some(Change::Applied(agent.clone(), model.take(event, at)))
            }
            _ => None,
        }
    }

    pub fn follow(&mut self, from: &AgentPath, to: &AgentPath) {
        let moved: Vec<AgentPath> = (self.live.keys())
            .filter(|path| path.moved(from, to).is_some())
            .cloned()
            .collect();
        for old in moved {
            if let Some(mut model) = self.live.remove(&old) {
                let path = old.renamed(from, to);
                model
                    .composer
                    .set_placeholder(copy::message_to(path.as_str()));
                self.live.insert(path, model);
            }
        }
    }

    pub fn get(&self, agent: &AgentPath) -> Option<&Model> {
        self.live.get(agent)
    }

    pub fn transcript(&self, agent: &AgentPath) -> Option<&Transcript> {
        self.get(agent).map(|model| &model.transcript)
    }

    pub fn get_mut(&mut self, agent: &AgentPath) -> Option<&mut Model> {
        self.live.get_mut(agent)
    }

    pub fn has(&self, agent: &AgentPath) -> bool {
        self.live.contains_key(agent)
    }

    pub fn running_turn(&self, agent: &AgentPath) -> Option<u64> {
        self.live.get(agent)?.inputs.running_turn()
    }
}

#[cfg(test)]
mod tests;
