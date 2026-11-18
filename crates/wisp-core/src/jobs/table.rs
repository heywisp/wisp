use std::collections::{BTreeMap, BTreeSet};
use std::fmt;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::time::Duration;

use tokio::sync::Notify;
use tokio::task::JoinHandle;
use tokio::time::Instant;
use tokio_util::sync::CancellationToken;
use wisp_protocol::{AgentEvent, BackgroundReason, LostReason, ProcessEnd, ReportedEnd, StoppedBy};

use wisp_plugin_api::prompt::{EndedJob, Item, LostJob, RewoundJob};
use wisp_tool_text::jobs::{item_line, typed_line};

use super::exec::Leftover;
use super::state::{Control, Cursor, Ended, JobState, Note};
use crate::call::OutputSink;

pub type SinkSlot = Arc<tokio::sync::Mutex<Option<OutputSink>>>;

pub const SHUTDOWN_LIMIT: Duration = Duration::from_secs(4);
const UNNUMBERED: u32 = 0;

fn awaited(id: u32, owed: Option<u32>) -> bool {
    owed.is_some_and(|first| id >= first)
}

pub struct Job {
    slot: SinkSlot,
    pump: Option<JoinHandle<()>>,
    state: Arc<JobState>,
}

impl Job {
    pub fn new(slot: SinkSlot, pump: JoinHandle<()>, state: Arc<JobState>) -> Self {
        Self {
            slot,
            pump: Some(pump),
            state,
        }
    }

    pub fn slot(&self) -> &SinkSlot {
        &self.slot
    }

    pub fn state(&self) -> &Arc<JobState> {
        &self.state
    }

    pub fn has_ended(&self) -> bool {
        !self.state.is_running()
    }

    pub async fn stop(mut self) {
        if let Some(pump) = self.pump.take() {
            pump.abort();
            let _ = pump.await;
        }
    }

    fn handle(&self, id: u32) -> Handle {
        Handle {
            id,
            state: self.state.clone(),
            slot: self.slot.clone(),
        }
    }
}

impl Drop for Job {
    fn drop(&mut self) {
        if let Some(pump) = &self.pump {
            pump.abort();
        }
    }
}

impl fmt::Debug for Job {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Job")
            .field("ended", &self.has_ended())
            .finish_non_exhaustive()
    }
}

#[derive(Clone)]
pub struct Handle {
    pub id: u32,
    pub state: Arc<JobState>,
    pub slot: SinkSlot,
}

impl fmt::Debug for Handle {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Handle")
            .field("id", &self.id)
            .finish_non_exhaustive()
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Restored {
    pub call_id: Option<String>,
    pub target: Option<String>,
    pub file: Option<PathBuf>,
    pub ended: RestoredEnd,
    pub terminal: Option<bool>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RestoredEnd {
    Ended(Ended),
    Kept(Kept),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Kept {
    pub target: Option<Leftover>,
    pub log: Option<PathBuf>,
    pub pid: Option<u32>,
}

impl Restored {
    pub fn ended(&self) -> Option<&Ended> {
        match &self.ended {
            RestoredEnd::Ended(ended) => Some(ended),
            RestoredEnd::Kept(_) => None,
        }
    }

    pub fn kept(&self) -> Option<&Kept> {
        match &self.ended {
            RestoredEnd::Kept(kept) => Some(kept),
            RestoredEnd::Ended(_) => None,
        }
    }
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Heard {
    pub ids: Vec<u32>,
    pub typed: Vec<u32>,
}

impl Heard {
    pub fn extend(&mut self, other: Heard) {
        self.ids.extend(other.ids);
        self.typed.extend(other.typed);
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Carrier {
    Call(String),
    Opening,
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum Hearing {
    Offered(Carrier),
    Heard,
    Rewound,
}

impl Hearing {
    fn unheard(hearing: Option<&Self>) -> bool {
        matches!(hearing, None | Some(Self::Rewound))
    }
}

#[derive(Debug)]
struct Table {
    calls: BTreeMap<String, Job>,
    numbered: BTreeMap<u32, Job>,
    links: BTreeMap<String, u32>,
    hearing: BTreeMap<u32, Hearing>,
    restored: BTreeMap<u32, Restored>,
    typed: BTreeMap<u32, Option<Carrier>>,
    masked: BTreeSet<String>,
    kept_ends: Vec<(u32, Ended)>,
    kept_lines: BTreeMap<u32, (Item, ProcessEnd)>,
    next: u32,
    moving: Option<(String, CancellationToken)>,
}

impl Default for Table {
    fn default() -> Self {
        Self {
            calls: BTreeMap::new(),
            numbered: BTreeMap::new(),
            links: BTreeMap::new(),
            hearing: BTreeMap::new(),
            restored: BTreeMap::new(),
            typed: BTreeMap::new(),
            masked: BTreeSet::new(),
            kept_ends: Vec::new(),
            kept_lines: BTreeMap::new(),
            next: 1,
            moving: None,
        }
    }
}

#[derive(Debug, Default)]
struct Shared {
    table: Mutex<Table>,
    notify: Arc<Notify>,
    child: AtomicBool,
}

#[derive(Debug, Clone, Default)]
pub struct Processes {
    shared: Arc<Shared>,
}

pub enum CallEnd {
    NoJob,
    Ended(Job),
    Abandoned(Job),
    Outlives(SinkSlot, Arc<JobState>),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StopRefusal {
    NoProcess,
    Ended,
    Unchecked,
}

impl Processes {
    fn table(&self) -> MutexGuard<'_, Table> {
        self.shared
            .table
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
    }

    pub fn set_child(&self) {
        self.shared.child.store(true, Ordering::Relaxed);
    }

    pub fn is_child(&self) -> bool {
        self.shared.child.load(Ordering::Relaxed)
    }

    fn end_kept(&self, id: u32, end: ProcessEnd, told: Option<Item>) -> bool {
        let mut table = self.table();
        let Some(restored) = table.restored.get_mut(&id).filter(|r| r.kept().is_some()) else {
            return false;
        };
        let ended = Ended::of(end.clone());
        restored.ended = RestoredEnd::Ended(ended.clone());
        table.kept_ends.push((id, ended));
        if let Some(item) = told {
            table.kept_lines.insert(id, (item, end));
        }
        drop(table);
        self.shared.notify.notify_one();
        true
    }

    fn kept_targets(&self) -> Vec<(u32, Option<String>, Leftover)> {
        self.table()
            .restored
            .iter()
            .filter_map(|(id, restored)| {
                let target = restored.kept()?.target.clone()?;
                Some((*id, restored.target.clone(), target))
            })
            .collect()
    }

    pub async fn check_kept(&self) {
        let targets = self.kept_targets();
        if targets.is_empty() {
            return;
        }
        let checked = tokio::task::spawn_blocking(move || {
            targets
                .into_iter()
                .filter(|(_, _, target)| !target.alive())
                .map(|(id, label, _)| (id, label))
                .collect::<Vec<_>>()
        })
        .await
        .unwrap_or_default();
        for (id, label) in checked {
            let end = ProcessEnd::Lost {
                reason: LostReason::Gone,
            };
            let item = Item::Lost(LostJob {
                id,
                target: label,
                end: super::job_end(&end),
            });
            self.end_kept(id, end, Some(item));
        }
    }

    pub async fn stop_kept(&self, id: u32, by: StoppedBy) -> Result<(), StopRefusal> {
        let (label, target) = {
            let table = self.table();
            let restored = table.restored.get(&id).ok_or(StopRefusal::NoProcess)?;
            let kept = restored.kept().ok_or(StopRefusal::Ended)?;
            let target = kept.target.clone().ok_or(StopRefusal::Unchecked)?;
            (restored.target.clone(), target)
        };
        let _ = tokio::task::spawn_blocking(move || target.stop()).await;
        let end = ProcessEnd::Stopped { by };
        let told = (by != StoppedBy::Model).then(|| {
            Item::Ended(EndedJob {
                id,
                target: label.unwrap_or_default(),
                end: super::job_end(&end),
                unread: 0,
            })
        });
        self.end_kept(id, end, told);
        Ok(())
    }

    pub fn add(&self, call_id: &str, job: Job) {
        job.state.connect(self.shared.notify.clone());
        let old = self.table().calls.insert(call_id.to_owned(), job);
        drop(old);
    }

    pub async fn reclaim(&self, call_id: &str) -> Option<OutputSink> {
        let slot = self.table().calls.get(call_id)?.slot().clone();
        let sink = slot.lock().await.take();
        let job = self.table().calls.remove(call_id);
        drop(job);
        sink
    }

    pub fn end_call(&self, call_id: &str, abandoned: bool) -> CallEnd {
        let mut table = self.table();
        if let Some(job) = table
            .links
            .get(call_id)
            .and_then(|id| table.numbered.get(id))
        {
            return CallEnd::Outlives(job.slot().clone(), job.state().clone());
        }
        let Some(job) = table.calls.get(call_id) else {
            return CallEnd::NoJob;
        };
        if abandoned {
            return table
                .calls
                .remove(call_id)
                .map_or(CallEnd::NoJob, CallEnd::Abandoned);
        }
        if job.has_ended() {
            return table
                .calls
                .remove(call_id)
                .map_or(CallEnd::NoJob, CallEnd::Ended);
        }
        CallEnd::Outlives(job.slot().clone(), job.state().clone())
    }

    fn enter(table: &mut Table, call_id: &str, id: u32, job: Job) -> Handle {
        let handle = job.handle(id);
        table.next = id + 1;
        table.links.insert(call_id.to_owned(), id);
        table.numbered.insert(id, job);
        handle
    }

    pub fn number_at_spawn(&self, call_id: &str, job: Job, reason: BackgroundReason) -> Handle {
        job.state.connect(self.shared.notify.clone());
        let handle = {
            let mut table = self.table();
            let id = table.next;
            job.state.number(id, Some(reason), true);
            Self::enter(&mut table, call_id, id, job)
        };
        self.shared.notify.notify_one();
        handle
    }

    fn take_running(table: &mut Table, call_id: &str, reason: BackgroundReason) -> Option<Handle> {
        let id = table.next;
        let numbered = table
            .calls
            .get(call_id)
            .is_some_and(|job| job.state.number(id, Some(reason), false));
        if !numbered {
            return None;
        }
        let job = table.calls.remove(call_id)?;
        job.state.send(Control::Release);
        Some(Self::enter(table, call_id, id, job))
    }

    pub fn number_ended(&self, call_id: &str, cursor: Cursor) -> Option<Handle> {
        let mut table = self.table();
        let id = table.next;
        let numbered = table
            .calls
            .get(call_id)
            .is_some_and(|job| job.has_ended() && job.state.number(id, None, false));
        if !numbered {
            return None;
        }
        let job = table.calls.remove(call_id)?;
        job.state.live().cursor = cursor;
        Some(Self::enter(&mut table, call_id, id, job))
    }

    pub fn number_after(&self, last: u32) {
        let mut table = self.table();
        table.next = table.next.max(last.saturating_add(1));
    }

    pub fn call_state(&self, call_id: &str) -> Option<Arc<JobState>> {
        self.table().calls.get(call_id).map(|job| job.state.clone())
    }

    pub fn call_handle(&self, call_id: &str) -> Option<Handle> {
        self.table()
            .calls
            .get(call_id)
            .map(|job| job.handle(UNNUMBERED))
    }

    pub fn hand_back(&self, call_id: &str, reason: BackgroundReason) -> Option<Handle> {
        let handle = {
            let mut table = self.table();
            let handle = Self::take_running(&mut table, call_id, reason)?;
            if table.moving.as_ref().is_some_and(|(id, _)| id == call_id) {
                table.moving = None;
            }
            handle
        };
        self.shared.notify.notify_one();
        Some(handle)
    }

    pub fn restore(&self, id: u32, restored: Restored) {
        let mut table = self.table();
        table.next = table.next.max(id.saturating_add(1));
        table.restored.insert(id, restored);
    }

    pub fn restored(&self, id: u32) -> Option<Restored> {
        let table = self.table();
        if table.numbered.contains_key(&id) {
            return None;
        }
        table.restored.get(&id).cloned()
    }

    pub fn restored_all(&self) -> Vec<(u32, Restored)> {
        let table = self.table();
        table
            .restored
            .iter()
            .filter(|(id, _)| !table.numbered.contains_key(id))
            .map(|(id, restored)| (*id, restored.clone()))
            .collect()
    }

    pub fn call_number(&self, call_id: &str) -> Option<u32> {
        self.table().links.get(call_id).copied()
    }

    pub fn take_number(&self, call_id: &str) -> Option<u32> {
        self.table().links.remove(call_id)
    }

    pub fn mark_masked(&self, call_id: &str) {
        self.table().masked.insert(call_id.to_owned());
    }

    pub fn take_masked(&self, call_id: &str) -> bool {
        self.table().masked.remove(call_id)
    }

    pub fn handle(&self, id: u32) -> Option<Handle> {
        self.table().numbered.get(&id).map(|job| job.handle(id))
    }

    pub fn handles(&self) -> Vec<Handle> {
        self.table()
            .numbered
            .iter()
            .map(|(id, job)| job.handle(*id))
            .collect()
    }

    pub(crate) fn started_events(&self) -> Vec<AgentEvent> {
        self.handles()
            .iter()
            .filter_map(|handle| handle.state.started_event(handle.id))
            .collect()
    }

    pub fn running(&self) -> usize {
        self.table()
            .numbered
            .values()
            .filter(|job| !job.has_ended())
            .count()
    }

    pub fn stop(&self, id: u32, by: StoppedBy) -> Result<(), StopRefusal> {
        let table = self.table();
        let Some(job) = table.numbered.get(&id) else {
            let restored = table.restored.get(&id).ok_or(StopRefusal::NoProcess)?;
            let kept = restored.kept().ok_or(StopRefusal::Ended)?;
            kept.target.as_ref().ok_or(StopRefusal::Unchecked)?;
            drop(table);
            let processes = self.clone();
            tokio::spawn(async move { processes.stop_kept(id, by).await });
            return Ok(());
        };
        if job.has_ended() {
            return Err(StopRefusal::Ended);
        }
        job.state.send(Control::Stop(by));
        Ok(())
    }

    pub fn typed(&self, id: u32) {
        self.table().typed.insert(id, None);
    }

    pub fn take_ends(&self, carrier: &Carrier) -> Vec<String> {
        let typed = self.take_typed(carrier);
        typed
            .into_iter()
            .map(typed_line)
            .chain(
                self.take_reports(carrier)
                    .iter()
                    .filter_map(|(item, _)| item_line(item)),
            )
            .collect()
    }

    pub fn take_typed(&self, carrier: &Carrier) -> Vec<u32> {
        let mut table = self.table();
        let mut typed = Vec::new();
        for (id, offered) in &mut table.typed {
            if offered.is_none() {
                *offered = Some(carrier.clone());
                typed.push(*id);
            }
        }
        typed
    }

    pub fn take_reports(&self, carrier: &Carrier) -> Vec<(Item, ReportedEnd)> {
        let mut table = self.table();
        let mut fresh: Vec<(Item, ReportedEnd)> = table
            .numbered
            .iter()
            .filter(|(id, _)| Hearing::unheard(table.hearing.get(id)))
            .filter_map(|(id, job)| {
                let item = job.state.end_item(*id)?;
                let end = job.state.end()?;
                Some((Item::Ended(item), ReportedEnd { id: *id, end }))
            })
            .collect();
        fresh.extend(
            table
                .kept_lines
                .iter()
                .filter(|(id, _)| Hearing::unheard(table.hearing.get(id)))
                .map(|(id, (item, end))| {
                    let reported = ReportedEnd {
                        id: *id,
                        end: end.clone(),
                    };
                    (item.clone(), reported)
                }),
        );
        for (_, reported) in &fresh {
            table
                .hearing
                .insert(reported.id, Hearing::Offered(carrier.clone()));
        }
        fresh
    }

    pub fn waking(&self, owed: Option<u32>) -> bool {
        !self.waking_ids(owed).is_empty()
    }

    pub fn waking_ids(&self, owed: Option<u32>) -> Vec<u32> {
        let table = self.table();
        table
            .numbered
            .iter()
            .filter(|(id, job)| {
                !table.hearing.contains_key(id) && job.state.wakes(awaited(**id, owed))
            })
            .map(|(id, _)| *id)
            .collect()
    }

    pub fn wakes_on(&self, id: u32, owed: Option<u32>) -> bool {
        let table = self.table();
        !table.hearing.contains_key(&id)
            && table
                .numbered
                .get(&id)
                .is_some_and(|job| job.state.wakes(awaited(id, owed)))
    }

    pub fn report(&self, carrier: &Carrier, id: u32) {
        let mut table = self.table();
        let tracked = table
            .numbered
            .get(&id)
            .is_some_and(|job| job.state.tracked_end());
        if tracked && Hearing::unheard(table.hearing.get(&id)) {
            table.hearing.insert(id, Hearing::Offered(carrier.clone()));
        }
    }

    pub fn hear(&self, carrier: &Carrier) -> Heard {
        let mut table = self.table();
        let offered = Hearing::Offered(carrier.clone());
        let typed: Vec<u32> = table
            .typed
            .iter()
            .filter(|(_, by)| by.as_ref() == Some(carrier))
            .map(|(id, _)| *id)
            .collect();
        table.typed.retain(|_, by| by.as_ref() != Some(carrier));
        let mut ids = Vec::new();
        for (id, hearing) in &mut table.hearing {
            if *hearing == offered {
                *hearing = Hearing::Heard;
                ids.push(*id);
            }
        }
        Heard { ids, typed }
    }

    pub fn heard_ids(&self) -> BTreeSet<u32> {
        self.table()
            .hearing
            .iter()
            .filter(|(_, hearing)| **hearing == Hearing::Heard)
            .map(|(id, _)| *id)
            .collect()
    }

    pub fn next_number(&self) -> u32 {
        self.table().next
    }

    pub fn running_from(&self, first: u32) -> Vec<u32> {
        self.table()
            .numbered
            .iter()
            .filter(|(id, job)| awaited(**id, Some(first)) && !job.has_ended())
            .map(|(id, _)| *id)
            .collect()
    }

    pub fn rewind(&self, heard: &BTreeSet<u32>, first: u32) -> Vec<RewoundJob> {
        let mut table = self.table();
        for (id, hearing) in &mut table.hearing {
            if !heard.contains(id) {
                *hearing = Hearing::Rewound;
            }
        }
        table
            .numbered
            .iter()
            .filter(|(id, job)| **id >= first && !job.has_ended())
            .map(|(id, job)| RewoundJob {
                id: *id,
                target: job.state.target().to_owned(),
            })
            .collect()
    }

    pub fn unoffer(&self, carrier: &Carrier) {
        let offered = Hearing::Offered(carrier.clone());
        let mut table = self.table();
        table.hearing.retain(|_, hearing| *hearing != offered);
        for by in table.typed.values_mut() {
            if by.as_ref() == Some(carrier) {
                *by = None;
            }
        }
    }

    pub fn set_moving(&self, call_id: &str, token: CancellationToken) {
        self.table().moving = Some((call_id.to_owned(), token));
    }

    pub fn clear_moving(&self, call_id: &str) {
        let mut table = self.table();
        if table.moving.as_ref().is_some_and(|(id, _)| id == call_id) {
            table.moving = None;
        }
    }

    pub fn fire_move(&self, call_id: &str) -> bool {
        {
            let mut table = self.table();
            let token = match &table.moving {
                Some((id, token)) if id == call_id => token.clone(),
                _ => return false,
            };
            if Self::take_running(&mut table, call_id, BackgroundReason::MovedByUser).is_none() {
                return false;
            }
            token.cancel();
        }
        self.shared.notify.notify_one();
        true
    }

    fn due(&self, now: Instant) -> Result<(), Option<Instant>> {
        let table = self.table();
        if !table.kept_ends.is_empty() {
            return Ok(());
        }
        let mut next: Option<Instant> = None;
        for job in table.numbered.values() {
            match job.state.due(now) {
                Ok(()) => return Ok(()),
                Err(Some(at)) => next = Some(next.map_or(at, |n| n.min(at))),
                Err(None) => {}
            }
        }
        Err(next)
    }

    pub async fn changed(&self) {
        loop {
            let notified = self.shared.notify.notified();
            match self.due(Instant::now()) {
                Ok(()) => return,
                Err(Some(at)) => {
                    tokio::select! {
                        () = notified => {}
                        () = tokio::time::sleep_until(at) => {}
                    }
                }
                Err(None) => notified.await,
            }
        }
    }

    pub fn notes(&self, all: bool) -> Vec<Note> {
        let now = Instant::now();
        let (states, kept): (Vec<Arc<JobState>>, Vec<(u32, Ended)>) = {
            let mut table = self.table();
            let states = table
                .numbered
                .values()
                .map(|job| job.state.clone())
                .collect();
            (states, std::mem::take(&mut table.kept_ends))
        };
        kept.into_iter()
            .map(|(id, ended)| Note::Ended { id, ended })
            .chain(states.iter().flat_map(|state| state.notes(now, all)))
            .collect()
    }

    pub async fn shutdown(&self) {
        self.check_kept().await;
        let states: Vec<Arc<JobState>> = self
            .table()
            .numbered
            .values()
            .filter(|job| !job.has_ended() && (job.state.log().is_none() || job.state.stop_sent()))
            .map(|job| job.state.clone())
            .collect();
        for state in states.iter().filter(|state| state.log().is_none()) {
            state.send(Control::Stop(StoppedBy::SessionEnded));
        }
        let all = async {
            for state in &states {
                let mut changes = state.subscribe();
                while state.is_running() {
                    if changes.changed().await.is_err() {
                        break;
                    }
                }
            }
        };
        let _ = tokio::time::timeout(SHUTDOWN_LIMIT, all).await;
    }
}
