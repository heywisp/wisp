use std::collections::{BTreeMap, BTreeSet};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::time::Duration;

use tokio::sync::{Notify, mpsc};
use wisp_plugin_api::Plugin;
use wisp_plugin_api::manifest::{Manifest, Switch};
use wisp_protocol::AgentEvent;
use wisp_protocol::slot::{SlotContent, SlotDisabled, SlotEntry, SlotFailure, SlotPlace, SlotView};

use super::hooks::HookProblem;
use super::hooks::points;
use super::hooks::resolve::{self, Kind, Name, Node};

pub(super) mod lane;

pub const SIDEBAR: &str = "sidebar";

pub const MIN_GAP: Duration = Duration::from_millis(wisp_plugin_api::slots::RENDER_GAP_MS);

pub const CALL_LIMIT: Duration = Duration::from_millis(500);

pub const PARK_AFTER: u32 = 3;

pub const FIRST_RETRY: Duration = Duration::from_secs(30);

pub const LAST_RETRY: Duration = Duration::from_secs(600);

pub const MESSAGE_CHARS: usize = 200;

fn locked<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(PoisonError::into_inner)
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Section {
    pub name: String,
    pub plugin: String,
    pub short: String,
    pub title: String,
    pub default_on: bool,
    pub marks: BTreeSet<String>,
    pub refresh: Option<Duration>,
    pub order: u32,
    pub cycle: Option<Vec<String>>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Sections {
    list: Vec<Section>,
    problems: Vec<HookProblem>,
}

impl Sections {
    pub fn build<'a>(manifests: impl IntoIterator<Item = &'a Manifest>) -> Self {
        let manifests: Vec<&Manifest> = manifests.into_iter().collect();
        let mut problems = Vec::new();
        let mut nodes = Vec::new();
        let mut found = Vec::new();
        for manifest in &manifests {
            let own = format!("{}/", manifest.id);
            for slot in &manifest.slots {
                let name = Name::new(&manifest.id, &slot.name);
                nodes.push(Node {
                    name: name.clone(),
                    kind: Kind::Hook,
                    sets: Vec::new(),
                    after: slot.after.clone(),
                    before: slot.before.clone(),
                    continues: None,
                });
                found.push(Section {
                    name: name.to_string(),
                    plugin: manifest.id.clone(),
                    short: slot.name.clone(),
                    title: slot.title.clone(),
                    default_on: slot.default == Switch::On,
                    marks: slot
                        .marks
                        .iter()
                        .filter(|m| points::core(m).is_some() || m.starts_with(&own))
                        .cloned()
                        .collect(),
                    refresh: slot.refresh_s.map(Duration::from_secs),
                    order: 0,
                    cycle: None,
                });
            }
            problems.extend(manifest.notes().into_iter().map(|message| HookProblem {
                plugin: manifest.id.clone(),
                message,
                cycle: false,
            }));
        }
        let names: BTreeSet<&str> = found.iter().map(|s| s.name.as_str()).collect();
        for node in &nodes {
            for (word, other) in node
                .after
                .iter()
                .map(|o| ("after", o))
                .chain(node.before.iter().map(|o| ("before", o)))
            {
                if !names.contains(other.as_str()) {
                    problems.push(HookProblem {
                        plugin: node.name.plugin.clone(),
                        message: format!(
                            "its section `{}` is ordered {word} `{other}`, which is no installed section; that name is ignored",
                            node.name
                        ),
                        cycle: false,
                    });
                }
            }
        }
        let order = resolve::resolve(SIDEBAR, &nodes);
        let mut place: BTreeMap<String, u32> = BTreeMap::new();
        for name in order.order.iter().chain(order.cycles.keys()) {
            let next = u32::try_from(place.len()).unwrap_or(u32::MAX);
            place.entry(name.to_string()).or_insert(next);
        }
        for section in &mut found {
            section.order = place.get(&section.name).copied().unwrap_or(u32::MAX);
            if let Some(cycle) = Name::parse(&section.name).and_then(|n| order.cycles.get(&n)) {
                section.cycle = Some(cycle.clone());
                problems.push(HookProblem {
                    plugin: section.plugin.clone(),
                    message: format!(
                        "its section `{}` is turned off: it is in a cycle, {}",
                        section.name,
                        cycle.join(" \u{2192} ")
                    ),
                    cycle: true,
                });
            }
        }
        found.sort_by(|a, b| (a.order, &a.name).cmp(&(b.order, &b.name)));
        problems.sort_by(|a, b| (&a.plugin, &a.message).cmp(&(&b.plugin, &b.message)));
        problems.dedup();
        Self {
            list: found,
            problems,
        }
    }

    pub fn list(&self) -> &[Section] {
        &self.list
    }

    pub fn is_empty(&self) -> bool {
        self.list.is_empty()
    }

    pub fn problems(&self) -> &[HookProblem] {
        &self.problems
    }

    pub fn entries(&self, plugin: &str) -> Vec<SlotEntry> {
        let mut entries: Vec<SlotEntry> = self
            .list
            .iter()
            .filter(|s| s.plugin == plugin)
            .map(|s| SlotEntry {
                name: s.name.clone(),
                slot: SlotPlace::Sidebar,
                title: s.title.clone(),
                default_on: s.default_on,
                order: s.order,
                disabled: s.cycle.clone().map(|cycle| SlotDisabled::Cycle { cycle }),
            })
            .collect();
        entries.sort_by(|a, b| a.name.cmp(&b.name));
        entries
    }

    fn wanted(&self) -> BTreeSet<String> {
        self.list
            .iter()
            .flat_map(|s| s.marks.iter().cloned())
            .collect()
    }
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Shown {
    pub sections: BTreeSet<String>,
    pub pages: BTreeSet<String>,
}

type Events = mpsc::WeakSender<AgentEvent>;

#[derive(Debug, Default)]
pub struct Signals {
    wanted: BTreeSet<String>,
    points: Mutex<BTreeSet<String>>,
    shown: Mutex<Option<(Shown, Option<Events>)>>,
    last: Mutex<Option<(Shown, Option<Events>)>>,
    off: Mutex<Option<BTreeSet<String>>>,
    wake: Notify,
}

impl Signals {
    pub fn mark(&self, point: &str) {
        if !self.wanted.contains(point) {
            return;
        }
        locked(&self.points).insert(point.to_owned());
        self.wake.notify_one();
    }

    fn show(&self, shown: Shown, events: Option<Events>) {
        *locked(&self.last) = Some((shown.clone(), events.clone()));
        *locked(&self.shown) = Some((shown, events));
        self.wake.notify_one();
    }

    pub fn set_off(&self, off: BTreeSet<String>) {
        *locked(&self.off) = Some(off);
        self.wake.notify_one();
    }

    fn take_off(&self) -> Option<BTreeSet<String>> {
        locked(&self.off).take()
    }

    fn take_points(&self) -> BTreeSet<String> {
        std::mem::take(&mut *locked(&self.points))
    }

    fn shown(&self) -> Option<(Shown, Option<Events>)> {
        locked(&self.last).clone()
    }

    fn take_shown(&self) -> Option<(Shown, Option<Events>)> {
        locked(&self.shown).take()
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct Failed {
    failure: SlotFailure,
    parked: bool,
    restarted: bool,
}

impl Failed {
    fn event(&self, section: String, view: SlotView) -> AgentEvent {
        AgentEvent::SlotFailed {
            section,
            view,
            failure: self.failure.clone(),
            parked: self.parked,
            restarted: self.restarted,
        }
    }
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
struct Current {
    content: Option<SlotContent>,
    failure: Option<Failed>,
}

type Board = Arc<Mutex<BTreeMap<String, Current>>>;

pub struct SlotLane {
    signals: Arc<Signals>,
    board: Board,
    task: tokio::task::JoinHandle<()>,
}

impl std::fmt::Debug for SlotLane {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SlotLane").finish_non_exhaustive()
    }
}

impl Drop for SlotLane {
    fn drop(&mut self) {
        self.task.abort();
    }
}

impl SlotLane {
    pub fn start(
        sections: &Sections,
        plugins: &BTreeMap<String, Arc<dyn Plugin>>,
        off: BTreeSet<String>,
    ) -> Self {
        let signals = Arc::new(Signals {
            wanted: sections.wanted(),
            off: Mutex::new(Some(off)),
            ..Signals::default()
        });
        let board = Board::default();
        let task =
            tokio::spawn(lane::Lane::new(sections, plugins, signals.clone(), board.clone()).run());
        Self {
            signals,
            board,
            task,
        }
    }

    #[must_use]
    pub fn restart(
        &self,
        sections: &Sections,
        plugins: &BTreeMap<String, Arc<dyn Plugin>>,
        off: BTreeSet<String>,
    ) -> Self {
        let lane = Self::start(sections, plugins, off);
        if let Some((shown, events)) = self.signals.shown() {
            lane.signals.show(shown, events);
        }
        lane
    }

    pub fn signals(&self) -> &Arc<Signals> {
        &self.signals
    }

    pub fn show(
        &self,
        sections: Vec<String>,
        pages: Vec<String>,
        events: &mpsc::Sender<AgentEvent>,
    ) {
        let shown = Shown {
            sections: sections.into_iter().collect(),
            pages: pages.into_iter().collect(),
        };
        self.signals.show(shown, Some(events.downgrade()));
    }

    pub fn detach(&self) {
        self.signals.show(Shown::default(), None);
    }

    pub fn current(&self) -> Vec<AgentEvent> {
        let board = locked(&self.board);
        let mut out = Vec::new();
        for (section, current) in board.iter() {
            if let Some(content) = &current.content {
                out.push(AgentEvent::SlotContent {
                    section: section.clone(),
                    view: SlotView::Panel,
                    content: content.clone(),
                });
            }
            if let Some(failed) = &current.failure {
                out.push(failed.event(section.clone(), SlotView::Panel));
            }
        }
        out
    }
}

pub fn one_line(message: &str) -> String {
    let flat: String = message
        .chars()
        .map(|c| if c.is_control() { ' ' } else { c })
        .collect();
    wisp_tool_text::output::cut_chars(flat.trim(), MESSAGE_CHARS)
}

pub fn retry_after(parks: u32) -> Duration {
    let doubled = FIRST_RETRY.saturating_mul(2u32.saturating_pow(parks.saturating_sub(1)));
    doubled.min(LAST_RETRY)
}

#[cfg(test)]
mod tests;
