use std::cmp::Ordering;
use std::collections::BTreeMap;
use std::num::NonZeroU16;
use std::time::Duration;

use wisp_protocol::{AgentEvent, AgentPath, ProcessEnd, StoppedBy, TermSize};
use wisp_render::chrome::{ProcessRow, one_line};
use wisp_render::transcript::copy::ProcessName;
use wisp_render::transcript::{CallCaption, Keeps, clean_line};

use crate::app::list::Confirm;

pub const DEFAULT_SIZE: TermSize = TermSize { cols: 80, rows: 24 };
const SCROLLBACK: usize = 0;
pub const MAX_PASTE: usize = 1024 * 1024;
pub const LINE_LIMIT: usize = 4095;
pub const MIN_COLS: u16 = 20;
pub const MIN_ROWS: u16 = 5;

struct Process {
    command: String,
    caption: CallCaption,
    call_id: String,
    size: Option<TermSize>,
    screen: Option<vt100::Parser>,
    started: Duration,
    end: Option<(ProcessEnd, Duration)>,
    waiting: Option<Duration>,
    restored: bool,
    kept: Keeps,
    pid: Option<u32>,
    log: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum StepKind {
    Attach,
    Secret {
        name: String,
        show: wisp_protocol::Ends,
        destination: Option<wisp_protocol::SecretDestination>,
        asked_by: Option<wisp_protocol::AskedBy>,
    },
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct StepIn {
    pub call_id: String,
    pub id: Option<u32>,
    pub reason: String,
    pub kind: StepKind,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Key {
    pub agent: AgentPath,
    pub id: u32,
}

impl Key {
    pub fn main(id: u32) -> Self {
        Self {
            agent: AgentPath::root(),
            id,
        }
    }

    pub fn follow(&mut self, from: &AgentPath, to: &AgentPath) {
        if let Some(agent) = self.agent.moved(from, to) {
            self.agent = agent;
        }
    }

    pub fn name(&self, here: &AgentPath) -> ProcessName {
        ProcessName::of(self.id, &self.agent, here)
    }
}

impl Ord for Key {
    fn cmp(&self, other: &Self) -> Ordering {
        self.agent
            .tree_cmp(&other.agent)
            .then(self.id.cmp(&other.id))
    }
}

impl PartialOrd for Key {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

#[derive(Default)]
pub struct Processes {
    list: BTreeMap<Key, Process>,
    attached: Option<Key>,
    request: Option<StepIn>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Opened {
    Call(String),
    Screen(Key),
    Nothing,
}

impl Processes {
    pub fn follow(&mut self, from: &AgentPath, to: &AgentPath) {
        self.list = std::mem::take(&mut self.list)
            .into_iter()
            .map(|(mut key, process)| {
                key.follow(from, to);
                (key, process)
            })
            .collect();
        if let Some(key) = &mut self.attached {
            key.follow(from, to);
        }
    }

    pub fn apply(
        &mut self,
        agent: &AgentPath,
        event: &AgentEvent,
        at: Duration,
    ) -> Option<(u32, ProcessEnd)> {
        let key = |id: &u32| Key {
            agent: agent.clone(),
            id: *id,
        };
        match event {
            AgentEvent::ProcessStarted {
                id,
                call_id,
                target,
                tty,
                size,
                keeps_running,
                pid,
                log,
                ..
            } => {
                let size = tty.then(|| size.unwrap_or(DEFAULT_SIZE));
                self.list.insert(
                    key(id),
                    Process {
                        command: clean_line(target.lines().next().unwrap_or_default()),
                        call_id: call_id.clone(),
                        size,
                        screen: size
                            .map(|s| vt100::Parser::new(nz(s.rows), nz(s.cols), SCROLLBACK)),
                        started: at,
                        end: None,
                        caption: CallCaption::default(),
                        waiting: None,
                        restored: false,
                        kept: if *keeps_running {
                            Keeps::Running
                        } else {
                            Keeps::No
                        },
                        pid: *pid,
                        log: log.clone(),
                    },
                );
                None
            }
            AgentEvent::ProcessRestored {
                id,
                call_id,
                target,
                end,
                duration_ms,
                keeps_running,
                unchecked,
                pid,
                log,
                ..
            } => {
                let kept = match (keeps_running, unchecked) {
                    (false, _) => Keeps::No,
                    (true, false) => Keeps::Running,
                    (true, true) => Keeps::Unchecked,
                };
                self.list.entry(key(id)).or_insert(Process {
                    command: clean_line(target.lines().next().unwrap_or_default()),
                    call_id: call_id.clone().unwrap_or_default(),
                    size: None,
                    screen: None,
                    started: at,
                    end: end
                        .clone()
                        .map(|end| (end, Duration::from_millis(*duration_ms))),
                    caption: CallCaption::default(),
                    waiting: None,
                    restored: true,
                    kept,
                    pid: *pid,
                    log: log.clone(),
                });
                None
            }
            AgentEvent::ProcessOutput { id, text, .. } => {
                if let Some(parser) = self.list.get_mut(&key(id)).and_then(|p| p.screen.as_mut()) {
                    parser.process(text.as_bytes());
                }
                None
            }
            AgentEvent::ProcessResized { id, size } => {
                if let Some(p) = self.list.get_mut(&key(id))
                    && let Some(parser) = &mut p.screen
                {
                    parser.set_size(nz(size.rows), nz(size.cols));
                    p.size = Some(*size);
                }
                None
            }
            AgentEvent::ProcessEnded {
                id,
                end,
                duration_ms,
                ..
            } => {
                let p = self.list.get_mut(&key(id))?;
                if p.end.is_some() {
                    return None;
                }
                let took = Duration::from_millis(*duration_ms);
                p.end = Some((end.clone(), took));
                p.waiting = None;
                Some((*id, end.clone()))
            }
            AgentEvent::ProcessNeedsInput { id, .. } => {
                if let Some(p) = self.list.get_mut(&key(id)).filter(|p| p.end.is_none()) {
                    p.waiting = Some(at);
                }
                None
            }
            AgentEvent::ProcessNeedsInputCleared { id } => {
                if let Some(p) = self.list.get_mut(&key(id)) {
                    p.waiting = None;
                }
                None
            }
            AgentEvent::ProcessAttached { id, .. } => {
                self.attached = Some(key(id));
                None
            }
            AgentEvent::ProcessDetached { id, .. } => {
                if self.attached == Some(key(id)) {
                    self.attached = None;
                }
                None
            }
            _ if !agent.is_root() => None,
            AgentEvent::AttachRequested {
                call_id,
                id,
                reason,
            } => {
                self.request = Some(StepIn {
                    call_id: call_id.clone(),
                    id: Some(*id),
                    reason: one_line(reason),
                    kind: StepKind::Attach,
                });
                None
            }
            AgentEvent::SecretRequested {
                call_id,
                id,
                destination,
                asked_by,
                name,
                reason,
                show,
            } => {
                self.request = Some(StepIn {
                    call_id: call_id.clone(),
                    id: *id,
                    reason: one_line(reason),
                    kind: StepKind::Secret {
                        name: one_line(name),
                        show: *show,
                        destination: destination.clone(),
                        asked_by: asked_by.clone(),
                    },
                });
                None
            }
            AgentEvent::AttachRequestClosed { call_id, .. }
            | AgentEvent::SecretRequestClosed { call_id, .. } => {
                if self.request.as_ref().is_some_and(|r| &r.call_id == call_id) {
                    self.request = None;
                }
                None
            }
            AgentEvent::TurnEnded { .. } => {
                self.request = None;
                None
            }
            _ => None,
        }
    }

    pub fn attached(&self) -> Option<&Key> {
        self.attached.as_ref()
    }

    pub fn set_attached(&mut self, key: Option<Key>) {
        self.attached = key;
    }

    pub fn request(&self) -> Option<&StepIn> {
        self.request.as_ref()
    }

    pub fn attachable(&self, key: &Key) -> bool {
        self.list
            .get(key)
            .is_some_and(|p| p.end.is_none() && p.screen.is_some())
    }

    pub fn waiting(&self, key: &Key) -> bool {
        self.list.get(key).is_some_and(|p| p.waiting.is_some())
    }

    pub fn newest_waiting(&self) -> Option<Key> {
        self.list
            .iter()
            .filter(|(key, p)| p.end.is_none() && self.attached.as_ref() != Some(*key))
            .filter_map(|(key, p)| p.waiting.map(|at| (at, key)))
            .max()
            .map(|(_, key)| key.clone())
    }

    pub fn bracketed(&self, key: &Key) -> bool {
        self.list
            .get(key)
            .and_then(|p| p.screen.as_ref())
            .is_some_and(|parser| parser.screen().bracketed_paste())
    }

    pub fn running(&self) -> impl Iterator<Item = &Key> {
        self.list
            .iter()
            .filter(|(_, p)| p.end.is_none())
            .map(|(key, _)| key)
    }

    pub fn stopping(&self) -> usize {
        self.list
            .values()
            .filter(|p| p.end.is_none() && p.kept == Keeps::No)
            .count()
    }

    pub fn is_stoppable(&self, key: &Key) -> bool {
        self.list
            .get(key)
            .is_some_and(|p| p.end.is_none() && p.kept != Keeps::Unchecked)
    }

    pub fn stop_sent(&mut self, key: &Key) {
        if let Some(p) = self.list.get_mut(key).filter(|p| p.kept == Keeps::Running) {
            p.kept = Keeps::No;
        }
    }

    pub fn has_restored_kept(&self) -> bool {
        self.list
            .values()
            .any(|p| p.end.is_none() && p.restored && p.kept == Keeps::Running)
    }

    fn ordered(&self) -> impl Iterator<Item = (&Key, &Process)> {
        [true, false].into_iter().flat_map(|running| {
            self.list
                .iter()
                .filter(move |(_, p)| p.end.is_none() == running)
        })
    }

    pub fn ids(&self) -> Vec<Key> {
        self.ordered().map(|(key, _)| key.clone()).collect()
    }

    pub fn caption(&mut self, key: &Key, call_id: &str, caption: CallCaption) {
        if let Some(p) = self.list.get_mut(key).filter(|p| p.call_id == call_id) {
            p.caption = caption;
        }
    }

    pub fn is_running(&self, key: &Key) -> bool {
        self.list.get(key).is_some_and(|p| p.end.is_none())
    }

    pub fn rows(&self, at: Duration) -> Vec<ProcessRow> {
        self.ordered()
            .map(|(key, p)| {
                let clock = match &p.end {
                    Some((_, took)) => *took,
                    None => at.saturating_sub(p.started),
                };
                ProcessRow {
                    id: key.id,
                    command: p.command.clone(),
                    terminal: p.size.filter(|_| p.screen.is_some()),
                    end: p.end.as_ref().map(|(end, _)| end.clone()),
                    clock_ms: u64::try_from(clock.as_millis()).unwrap_or(u64::MAX),
                    caption: p.caption.clone(),
                    waiting: p.waiting.is_some() && self.attached.as_ref() != Some(key),
                    restored: p.restored,
                    kept: p.kept,
                    pid: p.pid,
                    log: p.log.clone(),
                    agent: key.agent.clone(),
                    agent_ended: false,
                    opens: self.open(key) != Opened::Nothing,
                    stoppable: self.is_stoppable(key),
                    attachable: self.attachable(key),
                }
            })
            .collect()
    }

    pub fn open(&self, key: &Key) -> Opened {
        match self.list.get(key) {
            Some(p) if p.screen.is_some() => Opened::Screen(key.clone()),
            Some(p) if !p.call_id.is_empty() && key.agent.is_root() => {
                Opened::Call(p.call_id.clone())
            }
            _ => Opened::Nothing,
        }
    }

    pub fn command(&self, key: &Key) -> Option<&str> {
        self.list.get(key).map(|p| p.command.as_str())
    }

    pub fn screen(&self, key: &Key) -> Option<(&str, &vt100::Screen, bool)> {
        let p = self.list.get(key)?;
        let parser = p.screen.as_ref()?;
        Some((p.command.as_str(), parser.screen(), p.end.is_some()))
    }
}

fn nz(n: u16) -> NonZeroU16 {
    NonZeroU16::new(n).unwrap_or(NonZeroU16::MIN)
}

pub fn told_on_exit(end: &ProcessEnd) -> bool {
    !matches!(
        end,
        ProcessEnd::Stopped {
            by: StoppedBy::User | StoppedBy::SessionEnded
        }
    )
}

#[derive(Default)]
pub struct List {
    pub focus: Option<Key>,
    pub confirm: Confirm<Key>,
    pub screen: Option<Key>,
    pub chat: bool,
}

impl List {
    pub fn follow(&mut self, from: &AgentPath, to: &AgentPath) {
        let pending = self.confirm.pending_mut();
        for key in [&mut self.focus, &mut self.screen]
            .into_iter()
            .flatten()
            .chain(pending)
        {
            key.follow(from, to);
        }
    }

    pub fn index(&self, ids: &[Key]) -> usize {
        let focused = ids.iter().position(|key| self.focus.as_ref() == Some(key));
        focused.unwrap_or(0)
    }
}

#[cfg(test)]
mod tests;
