use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, MutexGuard};

use tokio::sync::{mpsc, watch};
use wisp_protocol::plan_usage::{
    Availability, PlanUsage, PlanUsageError, USAGE_MAX_AGE, UsageScope, UsageSource, UsageState,
    UsageWindow, WINDOWS,
};
use wisp_protocol::{AgentEvent, RequestId};
use wisp_tool_text::agents::Cause;

use crate::clock::Clock;
use crate::endpoints::ProviderKind;
use crate::models::ModelTable;
use crate::provider::Provider;
use record::{Claim, Failure, Record};

pub(crate) mod record;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Display {
    Disabled,
    Enabled,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Generation(u64);

#[derive(Clone, Copy, PartialEq, Eq)]
pub(crate) struct GuardStamp(u64);

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Room {
    Open,
    Used {
        model: Option<String>,
        resets_at: Option<i64>,
        cause: Cause,
    },
}

#[derive(Clone, Default)]
pub struct PlanUsageCache {
    table: Arc<Mutex<Table>>,
    folder: Option<Arc<Path>>,
    changed: watch::Sender<u64>,
}

#[derive(Default)]
struct Table {
    next: u64,
    entries: BTreeMap<String, Entry>,
    holders: BTreeSet<String>,
}

impl Table {
    fn stamp(&mut self) -> u64 {
        self.next += 1;
        self.next
    }
}

struct Entry {
    generation: Generation,
    provider: Arc<dyn Provider>,
    valid: bool,
    snapshot: Option<PlanUsage>,
    held: Vec<HeldWindow>,
    lookup_windows: BTreeSet<String>,
    unconfirmed: BTreeSet<String>,
    error: Option<PlanUsageError>,
    failure: Option<Failure>,
    checked: i64,
    observed: Option<i64>,
    last_attempt: Option<i64>,
    retry_at: i64,
    failures: u32,
    suspended: bool,
    revision: u64,
    passive_windows: BTreeMap<String, u64>,
    flight: u64,
    warnings: BTreeMap<String, Warning>,
    task: Option<tokio::task::AbortHandle>,
    listeners: BTreeMap<String, Listener>,
}

struct HeldWindow {
    window: UsageWindow,
    stamp: GuardStamp,
}

impl HeldWindow {
    fn guards(&self, model: &str) -> bool {
        self.window.is_retry_guard()
            && matches!(&self.window.scope, UsageScope::Model(id) if id == model)
    }
}

struct Warning {
    reset: Option<i64>,
    percent: f64,
    threshold: u8,
}

struct Listener {
    model: String,
    sender: mpsc::WeakSender<AgentEvent>,
    requests: Vec<RequestId>,
    outgoing: mpsc::UnboundedSender<AgentEvent>,
    worker: tokio::task::AbortHandle,
}

impl Drop for Listener {
    fn drop(&mut self) {
        self.worker.abort();
    }
}

impl PlanUsageCache {
    pub fn shared(folder: PathBuf) -> Self {
        Self {
            table: Arc::default(),
            folder: Some(folder.into()),
            changed: watch::Sender::default(),
        }
    }

    fn table(&self) -> MutexGuard<'_, Table> {
        self.table
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    pub fn bind(&self, alias: &str, provider: Arc<dyn Provider>) -> Option<Generation> {
        let mut table = self.table();
        if let Some(entry) = table.entries.get(alias) {
            return (entry.valid && Arc::ptr_eq(&entry.provider, &provider))
                .then_some(entry.generation);
        }
        Some(self.replace(&mut table, alias, provider))
    }

    pub fn rebind(&self, alias: &str, provider: Arc<dyn Provider>) -> Generation {
        let mut table = self.table();
        if let Some(entry) = table.entries.get(alias)
            && entry.valid
            && Arc::ptr_eq(&entry.provider, &provider)
        {
            return entry.generation;
        }
        self.replace(&mut table, alias, provider)
    }

    fn replace(&self, table: &mut Table, alias: &str, provider: Arc<dyn Provider>) -> Generation {
        if let Some(entry) = table.entries.remove(alias)
            && let Some(task) = entry.task
        {
            task.abort();
        }
        let generation = Generation(table.stamp());
        table.entries.insert(
            alias.to_owned(),
            Entry {
                generation,
                provider,
                valid: true,
                snapshot: None,
                held: Vec::new(),
                lookup_windows: BTreeSet::new(),
                unconfirmed: BTreeSet::new(),
                error: None,
                failure: None,
                checked: 0,
                observed: None,
                last_attempt: None,
                retry_at: 0,
                failures: 0,
                suspended: false,
                revision: 0,
                passive_windows: BTreeMap::new(),
                flight: 0,
                warnings: BTreeMap::new(),
                task: None,
                listeners: BTreeMap::new(),
            },
        );
        self.wake();
        generation
    }

    pub async fn lookup(
        &self,
        alias: &str,
        provider: Arc<dyn Provider>,
        clock: Arc<dyn Clock>,
    ) -> Option<UsageState> {
        let generation = self.rebind(alias, provider);
        let (sender, mut inbox) = mpsc::channel(8);
        self.subscribe(alias, generation, "lookup", RequestId(0), &sender, "");
        self.refresh(alias, generation, clock, false);
        let state = loop {
            match inbox.recv().await {
                Some(AgentEvent::PlanUsageChanged { state, .. }) if !state.loading => {
                    break Some(state);
                }
                Some(_) => {}
                None => break None,
            }
        };
        self.release("lookup", Some(alias));
        state
    }

    pub(crate) fn reset_lookup(&self, alias: &str, now: i64) {
        if let Some(entry) = self.table().entries.get_mut(alias) {
            entry.flight += 1;
            if let Some(task) = entry.task.take() {
                task.abort();
            }
            entry.snapshot = None;
            entry.lookup_windows.clear();
            entry.unconfirmed.clear();
            entry.passive_windows.clear();
            entry.error = None;
            entry.failure = None;
            entry.checked = 0;
            entry.observed = None;
            entry.last_attempt = None;
            entry.retry_at = 0;
            entry.failures = 0;
            entry.suspended = false;
            entry.warnings.clear();
            publish(alias, entry, now, &self.changed);
        }
    }

    pub(crate) fn changes(&self) -> watch::Receiver<u64> {
        self.changed.subscribe()
    }

    pub(crate) fn wake(&self) {
        self.changed.send_modify(|n| *n = n.wrapping_add(1));
    }

    pub(crate) fn hold(&self, holder: &str) -> bool {
        self.table().holders.insert(holder.to_owned())
    }

    pub(crate) fn held(&self, holder: &str) -> bool {
        self.table().holders.contains(holder)
    }

    pub(crate) fn unhold(&self, holder: &str) {
        let mut table = self.table();
        table.holders.remove(holder);
        if table.holders.is_empty() {
            for entry in table.entries.values_mut() {
                stop_unawaited(entry);
            }
        }
        self.wake();
    }

    pub(crate) fn awaited(&self, alias: &str) -> bool {
        let table = self.table();
        !table.holders.is_empty() || table.entries.get(alias).is_some_and(listened)
    }

    pub fn invalidate(&self, alias: &str) {
        let mut table = self.table();
        if let Some(entry) = table.entries.get_mut(alias) {
            entry.valid = false;
            entry.snapshot = None;
            entry.listeners.clear();
            if let Some(task) = entry.task.take() {
                task.abort();
            }
        }
        self.wake();
    }

    pub(crate) fn state(&self, alias: &str, now: i64) -> Option<UsageState> {
        self.table()
            .entries
            .get(alias)
            .filter(|entry| entry.valid)
            .map(|entry| usage_state(entry, now))
    }

    pub(crate) fn reached(&self, alias: &str, generation: Generation, window: &UsageWindow) {
        if window.is_retry_guard() && !matches!(window.scope, UsageScope::Model(_)) {
            return;
        }
        let mut table = self.table();
        let stamp = GuardStamp(table.stamp());
        let Some(entry) = table
            .entries
            .get_mut(alias)
            .filter(|entry| entry.valid && entry.generation == generation)
        else {
            return;
        };
        entry
            .held
            .retain(|old| !old.window.is_retry_guard() || live(&old.window, window.observed_at));
        let held = HeldWindow {
            window: window.clone(),
            stamp,
        };
        if let Some(old) = entry.held.iter_mut().find(|old| {
            old.window.scope == window.scope
                && old.window.is_retry_guard() == window.is_retry_guard()
        }) {
            if old.window.observed_at > window.observed_at {
                if window.is_retry_guard() {
                    old.stamp = stamp;
                }
                return;
            }
            *old = held;
        } else {
            entry.held.push(held);
        }
        self.wake();
    }

    pub(crate) fn guard_stamp(
        &self,
        alias: &str,
        generation: Generation,
        model: &str,
    ) -> Option<GuardStamp> {
        self.table()
            .entries
            .get(alias)
            .filter(|entry| entry.valid && entry.generation == generation)?
            .held
            .iter()
            .find(|held| held.guards(model))
            .map(|held| held.stamp)
    }

    pub(crate) fn succeeded(
        &self,
        alias: &str,
        generation: Generation,
        model: &str,
        sent_at: GuardStamp,
    ) {
        if let Some(entry) = self
            .table()
            .entries
            .get_mut(alias)
            .filter(|entry| entry.valid && entry.generation == generation)
        {
            let before = entry.held.len();
            entry
                .held
                .retain(|held| held.stamp != sent_at || !held.guards(model));
            if entry.held.len() != before {
                self.wake();
            }
        }
    }

    pub(crate) fn room(&self, alias: &str, model: &str, now: i64) -> Room {
        let table = self.table();
        let Some(entry) = table.entries.get(alias).filter(|entry| entry.valid) else {
            return Room::Open;
        };
        let reading = reading(entry, now);
        if reading.is_some_and(|s| s.availability == Some(Availability::Limited)) {
            return Room::Used {
                model: None,
                resets_at: None,
                cause: Cause::Limit,
            };
        }
        let kind = entry.provider.kind();
        let held = entry
            .held
            .iter()
            .map(|held| &held.window)
            .filter(|window| live(window, now))
            .map(|window| (window, window.is_retry_guard()));
        held.chain(used_up(reading, now).map(|window| (window, false)))
            .filter(|(window, _)| applies(kind, window, model))
            .filter(|(window, guard)| {
                !guard
                    || !reading
                        .is_some_and(|snapshot| room_after(entry, snapshot, window, model, now))
            })
            .max_by_key(|(window, guard)| {
                (
                    window.resets_at.unwrap_or(i64::MAX),
                    window.scope == UsageScope::All,
                    !guard,
                )
            })
            .map_or(Room::Open, |(window, guard)| Room::Used {
                model: (window.scope != UsageScope::All).then(|| model.to_owned()),
                resets_at: window.resets_at,
                cause: if guard {
                    Cause::Retry
                } else if listed(kind, window, model) == Some(true) {
                    Cause::Named(window.label.clone())
                } else {
                    Cause::Limit
                },
            })
    }

    pub(crate) fn model_limit(
        &self,
        alias: &str,
        generation: Generation,
        model: &str,
        now: i64,
    ) -> Option<Box<UsageWindow>> {
        let table = self.table();
        let entry = table
            .entries
            .get(alias)
            .filter(|entry| entry.valid && entry.generation == generation)?;
        let kind = entry.provider.kind();
        used_up(reading(entry, now), now)
            .filter(|w| !entry.unconfirmed.contains(&w.key) && w.observed_at <= now && w.valid())
            .filter(|w| match w.scope {
                UsageScope::All => false,
                UsageScope::Model(_) => applies(kind, w, model),
                UsageScope::ModelFamily(_) => listed(kind, w, model) == Some(true),
            })
            .max_by_key(|w| w.resets_at)
            .map(|w| Box::new(w.clone()))
    }

    pub(crate) fn available(&self, alias: &str, generation: Generation) -> bool {
        self.table()
            .entries
            .get(alias)
            .is_some_and(|entry| entry.valid && entry.generation == generation)
    }

    pub(crate) fn model(&self, alias: &str, client: &str, model: &str, now: impl FnOnce() -> i64) {
        if let Some(entry) = self.table().entries.get_mut(alias)
            && entry.valid
            && let Some(listener) = entry.listeners.get_mut(client)
            && listener.model != model
        {
            listener.model = model.to_owned();
            publish(alias, entry, now(), &self.changed);
        }
    }

    pub(crate) fn subscribe(
        &self,
        alias: &str,
        generation: Generation,
        client: &str,
        request: RequestId,
        sender: &mpsc::Sender<AgentEvent>,
        model: &str,
    ) {
        if let Some(entry) = self.table().entries.get_mut(alias)
            && entry.valid
            && entry.generation == generation
        {
            if let Some(listener) = entry.listeners.get_mut(client) {
                listener.model = model.to_owned();
                listener.requests.push(request);
                return;
            }
            let (outgoing, mut inbox) = mpsc::unbounded_channel();
            let weak = sender.downgrade();
            let task = tokio::spawn(async move {
                while let Some(event) = inbox.recv().await {
                    let Some(sender) = weak.upgrade() else { break };
                    if sender.send(event).await.is_err() {
                        break;
                    }
                }
            });
            entry.listeners.insert(
                client.to_owned(),
                Listener {
                    model: model.to_owned(),
                    sender: sender.downgrade(),
                    requests: vec![request],
                    outgoing,
                    worker: task.abort_handle(),
                },
            );
        }
    }

    pub(crate) fn release(&self, client: &str, alias: Option<&str>) {
        let mut table = self.table();
        let held = !table.holders.is_empty();
        for (name, entry) in &mut table.entries {
            if alias.is_none_or(|alias| alias == name) {
                entry.listeners.remove(client);
                if !held {
                    stop_unawaited(entry);
                }
            }
        }
        self.wake();
    }

    pub(crate) fn observe(
        &self,
        alias: &str,
        generation: Generation,
        observation: Result<PlanUsage, PlanUsageError>,
        now: i64,
    ) {
        let mut table = self.table();
        let Some(entry) = table
            .entries
            .get_mut(alias)
            .filter(|e| e.valid && e.generation == generation)
        else {
            return;
        };
        entry.revision += 1;
        match observation {
            Ok(partial) => {
                let mut full = entry.snapshot.take().unwrap_or(PlanUsage {
                    plan_tier: None,
                    availability: None,
                    windows: Vec::new(),
                });
                if partial.availability.is_some() {
                    full.availability = partial.availability;
                }
                for window in partial.windows {
                    let key = window.key.clone();
                    if let Some(old) = full.windows.iter_mut().find(|old| old.key == window.key) {
                        if window.observed_at < old.observed_at {
                            continue;
                        }
                        let percent =
                            if window.resets_at.is_some() && window.resets_at != old.resets_at {
                                window.used_percent
                            } else {
                                window.used_percent.or(old.used_percent)
                            };
                        let reset = window.resets_at.or(old.resets_at);
                        let duration = window.duration_seconds.or(old.duration_seconds);
                        if window.used_percent.is_none() && percent.is_some() {
                            entry.unconfirmed.insert(key.clone());
                        } else if (percent, reset, duration)
                            == (
                                window.used_percent,
                                window.resets_at,
                                window.duration_seconds,
                            )
                        {
                            entry.unconfirmed.remove(&key);
                        }
                        *old = window;
                        old.used_percent = percent;
                        old.resets_at = reset;
                        old.duration_seconds = duration;
                        entry.passive_windows.insert(key, entry.revision);
                    } else if full.windows.len() < 64 {
                        full.windows.push(window);
                        entry.passive_windows.insert(key, entry.revision);
                    }
                }
                full.windows.sort_by(|a, b| a.key.cmp(&b.key));
                entry.snapshot = Some(full);
                entry.observed = Some(now);
                entry.error = None;
            }
            Err(error) => entry.error = Some(error),
        }
        publish(alias, entry, now, &self.changed);
    }

    pub(crate) fn refresh(
        &self,
        alias: &str,
        generation: Generation,
        clock: Arc<dyn Clock>,
        explicit: bool,
    ) {
        let now = clock.now().as_second();
        let stored = self
            .folder
            .as_deref()
            .and_then(|folder| record::read(folder, alias));
        let mut table = self.table();
        let held = !table.holders.is_empty();
        let Some(entry) = table
            .entries
            .get_mut(alias)
            .filter(|e| e.valid && e.generation == generation)
        else {
            return;
        };
        entry
            .listeners
            .retain(|_, listener| listener.sender.strong_count() > 0);
        if !held && entry.listeners.is_empty() {
            return;
        }
        if let Some(stored) = stored {
            adopt(entry, stored);
        }
        let service_wait = entry
            .failure
            .filter(|failure| Some(failure.at) > entry.observed)
            .and_then(|failure| match failure.error {
                PlanUsageError::Throttled { retry_at } => retry_at,
                _ => None,
            });
        let reset_passed = entry
            .snapshot
            .as_ref()
            .is_some_and(|s| s.windows.iter().any(|w| w.reset_passed(now)));
        let blocked = entry.task.is_some()
            || entry.suspended
            || entry
                .last_attempt
                .is_some_and(|last| now.saturating_sub(last) < if explicit { 10 } else { 60 })
            || if explicit {
                service_wait.is_some_and(|at| at > now)
            } else {
                entry.retry_at > now && !(entry.failures == 0 && reset_passed)
            };
        if blocked {
            publish(alias, entry, now, &self.changed);
            return;
        }
        let Ok(admission) = crate::endpoints::generation::admit(entry.provider.generation()) else {
            publish(alias, entry, now, &self.changed);
            return;
        };
        entry.last_attempt = Some(now);
        let revision = entry.revision;
        entry.flight += 1;
        let flight = entry.flight;
        let known = entry.checked;
        let shared = self
            .folder
            .clone()
            .filter(|_| !entry.provider.billing().is_metered());
        let provider = entry.provider.clone();
        let cache = self.clone();
        let alias = alias.to_owned();
        let task_alias = alias.clone();
        let task = tokio::spawn(async move {
            let _admission = admission;
            let mut held = None;
            if let Some(folder) = shared.as_deref() {
                match record::claim(folder, &task_alias) {
                    Claim::Busy => {
                        let now = clock.now().as_second();
                        return cache.settle(&task_alias, generation, flight, None, now);
                    }
                    Claim::Held(file) => {
                        let newer = record::read::<Record>(folder, &task_alias)
                            .filter(|stored| stored.checked > known);
                        if newer.is_some() {
                            let now = clock.now().as_second();
                            return cache.settle(&task_alias, generation, flight, newer, now);
                        }
                        held = Some(file);
                    }
                    Claim::Unshared => {}
                }
            }
            let result = provider.plan_usage().await;
            let now = clock.now().as_second();
            let written = {
                let mut table = cache.table();
                let Some(entry) = table
                    .entries
                    .get_mut(&task_alias)
                    .filter(|e| e.valid && e.generation == generation && e.flight == flight)
                else {
                    return;
                };
                entry.task = None;
                let shareable = !matches!(result, Ok(None) | Err(PlanUsageError::Disabled));
                finish(entry, result, revision, now);
                publish(&task_alias, entry, now, &cache.changed);
                shareable.then(|| to_record(entry))
            };
            if let (Some(folder), Some(written), Some(_)) = (shared, written, &held) {
                let _ = record::write(&folder, &task_alias, &written);
            }
        });
        entry.task = Some(task.abort_handle());
        publish(&alias, entry, now, &self.changed);
    }

    fn settle(
        &self,
        alias: &str,
        generation: Generation,
        flight: u64,
        stored: Option<Record>,
        now: i64,
    ) {
        let mut table = self.table();
        if let Some(entry) = table
            .entries
            .get_mut(alias)
            .filter(|e| e.valid && e.generation == generation && e.flight == flight)
        {
            entry.task = None;
            if let Some(stored) = stored {
                adopt(entry, stored);
            }
            publish(alias, entry, now, &self.changed);
        }
    }
}

fn finish(
    entry: &mut Entry,
    result: Result<Option<PlanUsage>, PlanUsageError>,
    revision: u64,
    now: i64,
) {
    entry.checked = now;
    match result {
        Ok(Some(snapshot)) => {
            merge_full(entry, snapshot, revision, ReadingOrigin::Local);
            entry.observed = Some(now);
            entry.error = None;
            entry.failures = 0;
            entry.retry_at = now.saturating_add(600);
        }
        _ if entry.revision != revision => {}
        Ok(None) => {
            entry.error = Some(PlanUsageError::Unsupported);
            entry.suspended = true;
        }
        Err(error) if error.needs_user() => {
            entry.suspended = true;
            entry.retry_at = now;
            entry.error = Some(error);
            entry.failure = Some(Failure { error, at: now });
        }
        Err(error) => {
            entry.failures = entry.failures.saturating_add(1);
            let service = match error {
                PlanUsageError::Throttled { retry_at } => retry_at.filter(|at| *at > now),
                _ => None,
            };
            let backoff = if entry.failures > 1 { 3600 } else { 1800 };
            entry.retry_at = now.saturating_add(backoff).max(service.unwrap_or(0));
            let error = match error {
                PlanUsageError::Throttled { .. } => PlanUsageError::Throttled { retry_at: service },
                other => other,
            };
            entry.error = Some(error);
            if error != PlanUsageError::Disabled {
                entry.failure = Some(Failure { error, at: now });
            }
        }
    }
}

fn adopt(entry: &mut Entry, stored: Record) {
    if stored.checked <= entry.checked {
        return;
    }
    entry.checked = stored.checked;
    entry.retry_at = stored.next_attempt;
    entry.failures = stored.failures;
    if stored.observed > entry.observed
        && let Some(reading) = stored.reading
    {
        merge_full(entry, reading, entry.revision, ReadingOrigin::Shared);
        entry.observed = stored.observed;
        if !entry.suspended {
            entry.error = None;
        }
    }
    if let Some(failure) = stored
        .failure
        .filter(|failure| Some(failure.at) > entry.failure.map(|known| known.at))
    {
        entry.failure = Some(failure);
        if !failure.error.needs_user() && entry.snapshot.is_none() {
            entry.error = Some(failure.error);
        }
    }
}

fn to_record(entry: &Entry) -> Record {
    Record {
        reading: entry.snapshot.clone(),
        observed: entry.observed,
        next_attempt: entry.retry_at,
        failures: entry.failures,
        failure: entry.failure,
        checked: entry.checked,
    }
}

enum ReadingOrigin {
    Local,
    Shared,
}

fn merge_full(
    entry: &mut Entry,
    mut snapshot: PlanUsage,
    started_revision: u64,
    origin: ReadingOrigin,
) {
    let inventory: BTreeSet<String> = snapshot
        .windows
        .iter()
        .map(|window| window.key.clone())
        .collect();
    let previous = entry.snapshot.take();
    for window in &mut snapshot.windows {
        if let Some(newer) = previous.iter().flat_map(|s| &s.windows).find(|old| {
            old.key == window.key
                && (old.observed_at > window.observed_at
                    || (entry
                        .passive_windows
                        .get(&old.key)
                        .is_some_and(|revision| *revision > started_revision)
                        && old.source != UsageSource::UsageEndpoint
                        && old.observed_at == window.observed_at))
        }) {
            *window = newer.clone();
        } else {
            match origin {
                ReadingOrigin::Local => {
                    entry.unconfirmed.remove(&window.key);
                }
                ReadingOrigin::Shared => {
                    entry.unconfirmed.insert(window.key.clone());
                }
            }
        }
    }
    if let Some(previous) = previous {
        for window in previous.windows {
            if !inventory.contains(&window.key)
                && (!entry.lookup_windows.contains(&window.key)
                    || entry
                        .passive_windows
                        .get(&window.key)
                        .is_some_and(|revision| *revision > started_revision))
                && window.source != UsageSource::UsageEndpoint
                && snapshot.windows.len() < 64
            {
                snapshot.windows.push(window);
            }
        }
    }
    snapshot.windows.sort_by(|a, b| a.key.cmp(&b.key));
    entry.lookup_windows = inventory;
    entry
        .passive_windows
        .retain(|key, _| snapshot.windows.iter().any(|window| &window.key == key));
    entry
        .unconfirmed
        .retain(|key| snapshot.windows.iter().any(|window| &window.key == key));
    entry.snapshot = Some(snapshot);
}

fn listened(entry: &Entry) -> bool {
    entry
        .listeners
        .values()
        .any(|listener| listener.sender.strong_count() > 0)
}

fn stop_unawaited(entry: &mut Entry) {
    if !listened(entry)
        && let Some(task) = entry.task.take()
    {
        entry.flight += 1;
        task.abort();
    }
}

fn reading_stale(entry: &Entry, now: i64) -> bool {
    entry.error.is_some()
        || entry
            .observed
            .is_none_or(|at| now.saturating_sub(at) >= USAGE_MAX_AGE)
}

fn usage_state(entry: &Entry, now: i64) -> UsageState {
    UsageState {
        snapshot: entry.snapshot.clone(),
        loading: entry.task.is_some(),
        stale: reading_stale(entry, now)
            || entry
                .snapshot
                .as_ref()
                .is_some_and(|s| s.windows.iter().any(|w| w.stale(now))),
        error: entry.error,
    }
}

fn reading(entry: &Entry, now: i64) -> Option<&PlanUsage> {
    entry
        .snapshot
        .as_ref()
        .filter(|_| !reading_stale(entry, now))
}

fn room_after(
    entry: &Entry,
    reading: &PlanUsage,
    guard: &UsageWindow,
    model: &str,
    now: i64,
) -> bool {
    let kind = entry.provider.kind();
    let mut covering = reading.windows.iter().filter(|w| applies(kind, w, model));
    let has_room = |w: &UsageWindow| {
        !w.is_retry_guard()
            && !matches!(w.source, UsageSource::RetryAfter | UsageSource::ErrorBody)
            && w.valid()
            && !w.stale(now)
            && !entry.unconfirmed.contains(&w.key)
            && guard.observed_at < w.observed_at
            && w.observed_at <= now
            && w.used_percent
                .is_some_and(|percent| percent.value() < 100.0)
    };
    covering.next().is_some_and(has_room) && covering.all(has_room)
}

fn used_up(reading: Option<&PlanUsage>, now: i64) -> impl Iterator<Item = &UsageWindow> {
    reading
        .into_iter()
        .flat_map(|s| &s.windows)
        .filter(move |w| {
            !w.stale(now)
                && w.used_percent
                    .is_some_and(|percent| percent.value() >= 100.0)
                && w.resets_at.is_some_and(|at| at > now)
        })
}

fn live(window: &UsageWindow, now: i64) -> bool {
    window
        .resets_at
        .map_or_else(|| !window.stale(now), |at| at > now)
}

fn listed(kind: ProviderKind, window: &UsageWindow, model: &str) -> Option<bool> {
    let UsageScope::ModelFamily(name) = &window.scope else {
        return None;
    };
    let limits = &ModelTable::shared().provider(kind)?.weekly_limits;
    (!limits.is_empty()).then(|| {
        window.duration_seconds == Some(WINDOWS[1].0)
            && limits
                .get(name)
                .is_some_and(|ids| ids.iter().any(|id| id == model))
    })
}

fn applies(kind: ProviderKind, window: &UsageWindow, model: &str) -> bool {
    listed(kind, window, model).unwrap_or_else(|| window.scope.applies_to(model))
}

fn publish(alias: &str, entry: &mut Entry, now: i64, changed: &watch::Sender<u64>) {
    changed.send_modify(|n| *n = n.wrapping_add(1));
    let state = usage_state(entry, now);
    let kind = entry.provider.kind();
    let mut warnings = Vec::new();
    if entry.error.is_none()
        && let Some(snapshot) = &state.snapshot
    {
        for window in &snapshot.windows {
            let Some(percent) = window.used_percent.map(|p| p.value()) else {
                continue;
            };
            if window.stale(now) {
                continue;
            }
            if !entry
                .listeners
                .values()
                .any(|listener| applies(kind, window, &listener.model))
            {
                continue;
            }
            let previous = entry.warnings.entry(window.key.clone()).or_insert(Warning {
                reset: window.resets_at,
                percent,
                threshold: 0,
            });
            if previous.reset != window.resets_at
                || (window.resets_at.is_none() && percent < previous.percent)
            {
                previous.threshold = 0;
                previous.reset = window.resets_at;
            }
            previous.percent = percent;
            let threshold = [75_u8, 90, 95, 100]
                .into_iter()
                .filter(|t| f64::from(*t) <= percent)
                .max()
                .unwrap_or(0);
            if threshold > previous.threshold {
                previous.threshold = threshold;
                warnings.push((window.clone(), threshold));
            }
        }
    }
    entry.listeners.retain(|_, listener| {
        if listener.sender.strong_count() == 0 {
            return false;
        }
        let requests = if listener.requests.is_empty() {
            vec![None]
        } else {
            listener.requests.iter().copied().map(Some).collect()
        };
        let mut state = state.clone();
        if let Some(snapshot) = &mut state.snapshot {
            for window in &mut snapshot.windows {
                if listed(kind, window, &listener.model) == Some(true) {
                    let original = std::mem::replace(
                        &mut window.scope,
                        UsageScope::Model(listener.model.clone()),
                    );
                    if !window.valid() {
                        window.scope = original;
                    }
                }
            }
        }
        for request in requests {
            let event = AgentEvent::PlanUsageChanged {
                request,
                endpoint: alias.to_owned(),
                state: state.clone(),
            };
            if listener.outgoing.send(event).is_err() {
                return false;
            }
        }
        if !state.loading {
            listener.requests.clear();
        }
        for (window, threshold) in &warnings {
            let relevant = applies(kind, window, &listener.model);
            if !relevant {
                continue;
            }
            let _ = listener.outgoing.send(AgentEvent::PlanUsageWarning {
                endpoint: alias.to_owned(),
                window: window.clone(),
                threshold: *threshold,
            });
        }
        true
    });
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::clock::ShiftedClock;
    use crate::provider::{ProviderError, ProviderEvent, Request};
    use crate::test_guard::{loaded_usage, within};
    use futures::stream::BoxStream;
    use jiff::Timestamp;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use wisp_protocol::plan_usage::{Percent, UsageScope, UsageSource, UsageWindow};

    struct Fake {
        calls: AtomicUsize,
        ready: tokio::sync::Notify,
        result: Mutex<Result<Option<PlanUsage>, PlanUsageError>>,
        generation: Option<crate::endpoints::generation::Generation>,
        kind: ProviderKind,
    }
    impl Provider for Fake {
        fn kind(&self) -> ProviderKind {
            self.kind
        }
        fn generation(&self) -> Option<crate::endpoints::generation::Generation> {
            self.generation.clone()
        }
        fn stream(
            &self,
            _: &Request<'_>,
        ) -> BoxStream<'static, Result<ProviderEvent, ProviderError>> {
            Box::pin(futures::stream::empty())
        }
        fn cache_prefix(&self, _: &Request<'_>) -> Option<String> {
            None
        }
        fn plan_usage(
            &self,
        ) -> futures::future::BoxFuture<'static, Result<Option<PlanUsage>, PlanUsageError>>
        {
            self.calls.fetch_add(1, Ordering::SeqCst);
            self.ready.notify_one();
            let result = self.result.lock().unwrap().clone();
            Box::pin(async move { result })
        }
        fn billing(&self) -> wisp_protocol::Billing {
            wisp_protocol::Billing::AnthropicPlan
        }
    }
    fn held_windows(cache: &PlanUsageCache, alias: &str) -> Vec<UsageWindow> {
        cache.table().entries[alias]
            .held
            .iter()
            .map(|held| held.window.clone())
            .collect()
    }

    fn snapshot(now: i64, percent: f64) -> PlanUsage {
        PlanUsage::new(
            None,
            vec![UsageWindow {
                key: "codex.primary".to_owned(),
                label: "5-hour".to_owned(),
                duration_seconds: Some(18_000),
                scope: UsageScope::All,
                used_percent: Some(Percent::try_from(percent).unwrap()),
                resets_at: Some(now + 3600),
                observed_at: now,
                source: UsageSource::UsageEndpoint,
            }],
        )
        .unwrap()
    }
    fn fake(now: i64) -> Arc<Fake> {
        Arc::new(Fake {
            calls: AtomicUsize::new(0),
            ready: tokio::sync::Notify::new(),
            result: Mutex::new(Ok(Some(snapshot(now, 50.0)))),
            generation: None,
            kind: ProviderKind::Anthropic,
        })
    }

    const FABLE: &str = "claude-fable-5-2";
    const OPUS: &str = "claude-opus-5-5";

    fn used(model: Option<&str>, resets_at: Option<i64>) -> Room {
        reason(model, resets_at, Cause::Limit)
    }

    fn reason(model: Option<&str>, resets_at: Option<i64>, cause: Cause) -> Room {
        Room::Used {
            model: model.map(str::to_owned),
            resets_at,
            cause,
        }
    }

    fn fable(now: i64, percent: f64) -> PlanUsage {
        let mut reading = snapshot(now, percent);
        let w = &mut reading.windows[0];
        w.key = "fable-weekly".into();
        w.label = "Fable weekly".into();
        w.scope = UsageScope::ModelFamily("Fable".into());
        w.duration_seconds = Some(WINDOWS[1].0);
        reading
    }

    fn named(model: &str, reset: i64) -> Room {
        reason(
            Some(model),
            Some(reset),
            Cause::Named("Fable weekly".into()),
        )
    }

    #[test]
    fn a_held_limit_holds_its_scope_until_it_resets() {
        let now = 1_800_000_000;
        let cache = PlanUsageCache::default();
        let mut generation = cache.bind("alias", fake(now)).unwrap();
        assert_eq!(cache.room("unread", "model", now), Room::Open);
        assert_eq!(cache.room("alias", "model", now), Room::Open);
        let mut limit = snapshot(now, 100.0).windows.remove(0);
        limit.scope = UsageScope::Model("model".into());
        limit.used_percent = None;
        limit.source = UsageSource::ErrorBody;
        cache.reached("alias", generation, &limit);
        cache.reached("alias", generation, &retry_guard("model", now, now + 7200));
        let sent_at = cache.guard_stamp("alias", generation, "model").unwrap();
        cache.succeeded("alias", generation, "model", sent_at);
        assert_eq!(
            cache.room("alias", "model", now),
            used(Some("model"), Some(now + 3600))
        );
        assert_eq!(cache.room("alias", "other", now), Room::Open);
        assert_eq!(
            cache.room("alias", "model", now + USAGE_MAX_AGE),
            used(Some("model"), Some(now + 3600))
        );
        assert_eq!(cache.room("alias", "model", now + 3600), Room::Open);

        let mut newer = limit.clone();
        newer.observed_at += 1;
        newer.resets_at = Some(now + 7200);
        cache.reached("alias", generation, &newer);
        cache.reached("alias", generation, &limit);
        assert_eq!(
            cache.room("alias", "model", now + 1),
            used(Some("model"), Some(now + 7200))
        );
        let mut account = limit.clone();
        account.scope = UsageScope::All;
        cache.reached("alias", generation, &account);
        assert_eq!(
            cache.room("alias", "other", now),
            used(None, Some(now + 3600))
        );
        assert_eq!(
            cache.room("alias", "model", now + 1),
            used(Some("model"), Some(now + 7200))
        );
        cache.reset_lookup("alias", now);
        assert_eq!(
            cache.room("alias", "model", now),
            used(Some("model"), Some(now + 7200))
        );
        generation = cache.rebind("alias", fake(now));
        assert_eq!(cache.room("alias", "model", now), Room::Open);

        limit.resets_at = None;
        cache.reached("alias", generation, &limit);
        assert_eq!(
            cache.room("alias", "model", now + USAGE_MAX_AGE - 1),
            used(Some("model"), None)
        );
        assert_eq!(
            cache.room("alias", "model", now + USAGE_MAX_AGE),
            Room::Open
        );
        let mut account = limit.clone();
        account.scope = UsageScope::All;
        account.resets_at = Some(now + 120);
        cache.reached("alias", generation, &account);
        assert_eq!(cache.room("alias", "model", now), used(Some("model"), None));
        assert_eq!(
            cache.room("alias", "model", now + 120),
            used(Some("model"), None)
        );
        generation = cache.rebind(
            "alias",
            Arc::new(Fake {
                kind: ProviderKind::Gemini,
                ..Arc::into_inner(fake(now)).unwrap()
            }),
        );
        limit.scope = UsageScope::ModelFamily("gemini".into());
        cache.reached("alias", generation, &limit);
        assert_eq!(
            cache.room("alias", "gemini-4-carbon", now),
            used(Some("gemini-4-carbon"), None)
        );
        assert_eq!(cache.room("alias", "gemini", now), Room::Open);
        generation = cache.rebind("alias", fake(now));

        let mut full = snapshot(now, 100.0);
        full.windows[0].scope = UsageScope::Model("model".into());
        let mut unrelated = full.windows[0].clone();
        unrelated.key = "old".into();
        unrelated.scope = UsageScope::Model("other".into());
        unrelated.observed_at -= USAGE_MAX_AGE;
        full.windows.push(unrelated);
        cache.observe("alias", generation, Ok(full), now);
        assert_eq!(
            cache.room("alias", "model", now),
            used(Some("model"), Some(now + 3600))
        );
        assert_eq!(cache.room("alias", "other", now), Room::Open);
        assert_eq!(
            cache.room("alias", "model", now + USAGE_MAX_AGE),
            Room::Open
        );
        cache.observe("alias", generation, Err(PlanUsageError::Network), now);
        assert_eq!(cache.room("alias", "model", now), Room::Open);
        cache.reset_lookup("alias", now);
        assert_eq!(cache.room("alias", "model", now), Room::Open);
        let mut full = snapshot(now, 100.0);
        full.windows[0].resets_at = None;
        cache.observe("alias", generation, Ok(full), now);
        assert_eq!(cache.room("alias", "model", now), Room::Open);
        let mut full = snapshot(now, 100.0);
        full.windows[0].resets_at = Some(now);
        cache.observe("alias", generation, Ok(full), now);
        assert_eq!(cache.room("alias", "model", now), Room::Open);
        let mut limited = snapshot(now, 50.0);
        limited.availability = Some(Availability::Limited);
        cache.observe("alias", generation, Ok(limited), now);
        assert_eq!(cache.room("alias", "any", now), used(None, None));
        let mut limited = snapshot(now, 100.0);
        limited.availability = Some(Availability::Limited);
        cache.observe("alias", generation, Ok(limited), now);
        assert_eq!(cache.room("alias", "any", now), used(None, None));
        assert_eq!(cache.room("alias", "any", now + USAGE_MAX_AGE), Room::Open);
        cache.invalidate("alias");
        assert_eq!(cache.room("alias", "any", now), Room::Open);
    }

    #[test]
    fn weekly_membership_uses_the_exact_provider_family_period_and_model() {
        let now = 1_800_000_000;
        let cache = PlanUsageCache::default();
        for held in [false, true] {
            let generation = cache.rebind("example", fake(now));
            let reading = fable(now, 100.0);
            if held {
                cache.reached("example", generation, &reading.windows[0]);
            } else {
                cache.observe("example", generation, Ok(reading), now);
            }
            for id in [FABLE, "claude-fable-5-1", "claude-fable-5"] {
                assert_eq!(cache.room("example", id, now), named(id, now + 3600));
            }
            for id in [OPUS, "claude-mythos-5-1", "claude-fable-5-3"] {
                assert_eq!(cache.room("example", id, now), Room::Open);
            }
        }
        for (family, duration) in [
            ("Fable", WINDOWS[0].0),
            ("fable", WINDOWS[1].0),
            ("claude", WINDOWS[1].0),
            ("Opus", WINDOWS[1].0),
        ] {
            let generation = cache.rebind("example", fake(now));
            let mut reading = fable(now, 100.0);
            reading.windows[0].scope = UsageScope::ModelFamily(family.into());
            reading.windows[0].duration_seconds = Some(duration);
            cache.observe("example", generation, Ok(reading), now);
            assert_eq!(
                cache.room("example", FABLE, now),
                Room::Open,
                "{family}, {duration}"
            );
        }
    }

    fn retry_guard(model: &str, now: i64, later: i64) -> UsageWindow {
        use wisp_protocol::plan_usage::RETRY_AFTER_KEY;
        UsageWindow {
            key: RETRY_AFTER_KEY.into(),
            label: "Retry after".into(),
            scope: UsageScope::Model(model.into()),
            resets_at: Some(later),
            used_percent: None,
            duration_seconds: None,
            source: UsageSource::RetryAfter,
            ..snapshot(now, 0.0).windows.remove(0)
        }
    }

    #[test]
    fn a_new_confirmed_reading_can_reopen_the_model_until_it_is_refused_again() {
        let now = 1_800_000_000;
        let later = now + 508_200;
        let cache = PlanUsageCache::default();
        let generation = cache.bind("example", fake(now)).unwrap();
        let room = |at| cache.room("example", FABLE, at);
        let observe = |reading, at| cache.observe("example", generation, Ok(reading), at);
        let retry = reason(Some(FABLE), Some(later), Cause::Retry);
        cache.reached("example", generation, &retry_guard(FABLE, now, later));
        let before = snapshot(now, 0.0);
        observe(before.clone(), now);
        assert_eq!(room(now), retry);

        let mut sparse = before;
        sparse.windows[0].observed_at = now + 1;
        sparse.windows[0].used_percent = None;
        observe(sparse, now + 1);
        assert_eq!(room(now + 1), retry, "an inherited percent is not fresh");

        cache.reset_lookup("example", now + 2);
        let mut other = fable(now + 2, 0.0);
        other.windows[0].scope = UsageScope::Model(OPUS.into());
        observe(other, now + 2);
        assert_eq!(room(now + 2), retry, "no covering row");

        let mut reading = snapshot(now + 3, 0.0);
        let mut family = fable(now + 3, 0.0).windows.remove(0);
        family.used_percent = None;
        reading.windows.push(family);
        observe(reading, now + 3);
        assert_eq!(room(now + 3), retry, "a covering row lacks a percent");
        for (source, opens) in [
            (UsageSource::UsageEndpoint, true),
            (UsageSource::ResponseHeader, true),
            (UsageSource::ResponseEvent, true),
            (UsageSource::ErrorBody, false),
            (UsageSource::RetryAfter, false),
        ] {
            let mut reading = fable(now + 3, 99.0);
            reading.windows[0].source = source;
            observe(reading, now + 3);
            let expected = if opens { Room::Open } else { retry.clone() };
            assert_eq!(room(now + 3), expected, "{source:?}");
        }
        observe(fable(now + 3, 100.0), now + 3);
        assert_eq!(room(now + 3), retry, "100% is not room");
        observe(fable(now + 3, 0.0), now + 3);
        assert_eq!(room(now + 3), Room::Open);

        let renewed = later + 60;
        cache.reached("example", generation, &retry_guard(FABLE, now + 4, renewed));
        assert_eq!(
            room(now + 4),
            reason(Some(FABLE), Some(renewed), Cause::Retry)
        );
    }

    #[test]
    fn room_reports_the_fact_that_blocks_longest() {
        use wisp_protocol::plan_usage::RETRY_AFTER_KEY;
        let now = 1_800_000_000;
        let later = now + 508_200;
        let cache = PlanUsageCache::default();
        let generation = cache.bind("example", fake(now)).unwrap();
        let mut guard = retry_guard(FABLE, now, later);
        let room = |model| cache.room("example", model, now);
        let retry = reason(Some(FABLE), Some(later), Cause::Retry);
        cache.reached("example", generation, &guard);
        assert_eq!(room(FABLE), retry);
        assert_eq!(room(OPUS), Room::Open);
        let mut full = fable(now, 100.0);
        full.windows[0].resets_at = Some(later);
        cache.observe("example", generation, Ok(full.clone()), now);
        assert_eq!(room(FABLE), named(FABLE, later));
        let mut lower = full.clone();
        lower.windows[0].used_percent = Some(Percent::try_from(20.0).unwrap());
        cache.observe("example", generation, Ok(lower), now);
        assert_eq!(room(FABLE), retry);
        assert_eq!(room("claude-fable-5-1"), Room::Open);
        assert_eq!(held_windows(&cache, "example"), [guard.clone()]);
        let mut short = guard.clone();
        short.resets_at = Some(now + 18_000);
        cache.reached("example", generation, &short);
        cache.observe("example", generation, Ok(full), now);
        assert_eq!(room(FABLE), named(FABLE, later));
        cache.reset_lookup("example", now);
        cache.reached("example", generation, &guard);
        let mut account = snapshot(now, 100.0);
        account.windows[0].key = RETRY_AFTER_KEY.into();
        cache.observe("example", generation, Ok(account.clone()), now);
        assert_eq!(room(FABLE), retry);
        assert_eq!(room(OPUS), used(None, Some(now + 3600)));
        account.availability = Some(Availability::Limited);
        cache.observe("example", generation, Ok(account), now);
        assert_eq!(room(FABLE), used(None, None));
        cache.reset_lookup("example", now);
        let mut untied = guard.clone();
        untied.scope = UsageScope::All;
        cache.reached("example", generation, &untied);
        assert_eq!(room(OPUS), Room::Open);
        assert_eq!(cache.room("example", FABLE, later), Room::Open);
        guard.scope = UsageScope::Model(OPUS.into());
        guard.observed_at = later;
        guard.resets_at = Some(later + 61);
        cache.reached("example", generation, &guard);
        assert_eq!(held_windows(&cache, "example"), [guard.clone()]);
        let limit = UsageWindow {
            key: "subscription-limit".into(),
            label: "Subscription limit".into(),
            resets_at: Some(later + 61),
            source: UsageSource::ErrorBody,
            ..guard.clone()
        };
        cache.reached("example", generation, &limit);
        assert_eq!(held_windows(&cache, "example"), [guard, limit]);
        assert_eq!(
            cache.room("example", OPUS, later),
            used(Some(OPUS), Some(later + 61))
        );

        let mut full = fable(later + 1, 100.0);
        full.windows[0].resets_at = Some(later + 3600);
        cache.reached(
            "example",
            generation,
            &retry_guard(FABLE, later, later + 3600),
        );
        cache.observe("example", generation, Ok(fable(later + 1, 0.0)), later + 1);
        assert_eq!(cache.room("example", FABLE, later + 1), Room::Open);
        cache.observe("example", generation, Ok(full), later + 1);
        assert_eq!(
            cache.room("example", FABLE, later + 1),
            named(FABLE, later + 3600)
        );
    }

    #[tokio::test]
    async fn interest_coalesces_children_and_refreshes_at_ten_minutes_without_polling() {
        let now = 1_800_000_000;
        let clock = Arc::new(ShiftedClock::at(Timestamp::from_second(now).unwrap(), None));
        let provider = fake(now);
        let cache = PlanUsageCache::default();
        let generation = cache.bind("alias", provider.clone()).unwrap();
        assert_eq!(Some(generation), cache.bind("alias", provider.clone()));
        cache.refresh("alias", generation, clock.clone(), false);
        assert_eq!(provider.calls.load(Ordering::SeqCst), 0);
        let (tx, mut rx) = mpsc::channel(32);
        cache.subscribe("alias", generation, "root", RequestId(1), &tx, "model");
        for _ in 0..5 {
            cache.refresh("alias", generation, clock.clone(), false);
        }
        assert!(!loaded_usage(&mut rx).await.stale);
        assert_eq!(provider.calls.load(Ordering::SeqCst), 1);
        clock.set(Timestamp::from_second(now + 599).unwrap());
        cache.refresh("alias", generation, clock.clone(), false);
        loaded_usage(&mut rx).await;
        assert_eq!(provider.calls.load(Ordering::SeqCst), 1);
        clock.set(Timestamp::from_second(now + 600).unwrap());
        *provider.result.lock().unwrap() = Ok(Some(snapshot(now + 600, 96.0)));
        cache.refresh("alias", generation, clock.clone(), false);
        let updated = loaded_usage(&mut rx).await;
        assert_eq!(
            updated.snapshot.unwrap().windows[0]
                .used_percent
                .unwrap()
                .value(),
            96.0
        );
        assert_eq!(provider.calls.load(Ordering::SeqCst), 2);
        cache.release("root", None);
        clock.set(Timestamp::from_second(now + 1200).unwrap());
        cache.refresh("alias", generation, clock, false);
        assert_eq!(provider.calls.load(Ordering::SeqCst), 2);
    }

    #[tokio::test]
    async fn a_hold_starts_an_unlistened_lookup_and_the_last_unhold_cancels_it() {
        let now = 1_800_000_000;
        let clock = Arc::new(ShiftedClock::at(Timestamp::from_second(now).unwrap(), None));
        let (mut answer, wait) = tokio::sync::oneshot::channel();
        let provider = Arc::new(Delayed {
            calls: AtomicUsize::new(0),
            started: tokio::sync::Notify::new(),
            answer: Mutex::new(Some(wait)),
        });
        let cache = PlanUsageCache::default();
        let generation = cache.bind("alias", provider.clone()).unwrap();
        cache.hold("section");
        cache.hold("other");
        cache.refresh("alias", generation, clock, false);
        provider.started.notified().await;
        assert!(cache.state("alias", now).unwrap().loading);
        cache.unhold("section");
        assert!(cache.state("alias", now).unwrap().loading);
        cache.unhold("other");
        assert!(!cache.awaited("alias"));
        assert!(!cache.state("alias", now).unwrap().loading);
        answer.closed().await;
    }

    #[tokio::test]
    async fn releasing_or_replacing_an_account_keeps_a_hold_and_all_change_sources_wake() {
        let now = 1_800_000_000;
        let clock = Arc::new(ShiftedClock::at(Timestamp::from_second(now).unwrap(), None));
        let provider = fake(now);
        let cache = PlanUsageCache::default();
        let mut changes = cache.changes();
        let generation = cache.bind("alias", provider.clone()).unwrap();
        assert!(changes.has_changed().unwrap());
        changes.borrow_and_update();
        cache.hold("section");
        let (tx, _) = mpsc::channel(8);
        cache.subscribe("alias", generation, "client", RequestId(1), &tx, "model");
        cache.release("client", None);
        assert!(cache.awaited("alias"));
        cache.refresh("alias", generation, clock.clone(), false);
        provider.ready.notified().await;
        assert_eq!(provider.calls.load(Ordering::SeqCst), 1);
        assert!(changes.has_changed().unwrap());
        changes.borrow_and_update();
        cache.invalidate("alias");
        assert!(changes.has_changed().unwrap());
        assert!(cache.held("section"));
        changes.borrow_and_update();
        let generation = cache.rebind("alias", provider.clone());
        assert!(changes.has_changed().unwrap());
        cache.refresh("alias", generation, clock, false);
        provider.ready.notified().await;
        assert_eq!(provider.calls.load(Ordering::SeqCst), 2);
        cache.reset_lookup("alias", now);
        assert!(cache.held("section"));
        cache.unhold("section");
        assert!(!cache.awaited("alias"));
        cache.subscribe("alias", generation, "dead", RequestId(2), &tx, "model");
        drop(tx);
        assert!(!cache.awaited("alias"));
    }

    #[tokio::test]
    async fn a_lookup_asks_once_and_leaves_nothing_subscribed() {
        let now = 1_800_000_000;
        let clock = Arc::new(ShiftedClock::at(Timestamp::from_second(now).unwrap(), None));
        let provider = fake(now);
        let cache = PlanUsageCache::default();
        let state = cache
            .lookup("alias", provider.clone(), clock.clone())
            .await
            .unwrap();
        assert!(!state.loading);
        assert_eq!(state.error, None);
        assert_eq!(
            state.snapshot.unwrap().windows[0]
                .used_percent
                .unwrap()
                .value(),
            50.0
        );
        assert_eq!(provider.calls.load(Ordering::SeqCst), 1);
        *provider.result.lock().unwrap() = Ok(None);
        let none = cache
            .lookup("none", provider.clone(), clock.clone())
            .await
            .unwrap();
        assert_eq!(none.error, Some(PlanUsageError::Unsupported));
        clock.set(Timestamp::from_second(now + 100).unwrap());
        cache.lookup("none", provider.clone(), clock).await.unwrap();
        assert_eq!(provider.calls.load(Ordering::SeqCst), 2);
        let table = cache.table();
        assert!(
            table
                .entries
                .values()
                .all(|entry| entry.listeners.is_empty())
        );
        assert!(table.entries.values().all(|entry| entry.task.is_none()));
    }

    #[tokio::test]
    async fn aliases_generations_auth_failures_and_backoff_remain_isolated() {
        let now = 1_800_000_000;
        let clock = Arc::new(ShiftedClock::at(Timestamp::from_second(now).unwrap(), None));
        let provider = fake(now);
        let cache = PlanUsageCache::default();
        let a = cache.bind("a", provider.clone()).unwrap();
        let b = cache.bind("b", provider.clone()).unwrap();
        assert_ne!(a, b);
        let (tx, mut rx) = mpsc::channel(32);
        cache.subscribe("a", a, "root", RequestId(1), &tx, "model");
        *provider.result.lock().unwrap() = Err(PlanUsageError::AuthenticationRequired);
        cache.refresh("a", a, clock.clone(), false);
        assert_eq!(
            loaded_usage(&mut rx).await.error,
            Some(PlanUsageError::AuthenticationRequired)
        );
        clock.set(Timestamp::from_second(now + 1000).unwrap());
        cache.refresh("a", a, clock.clone(), true);
        loaded_usage(&mut rx).await;
        assert_eq!(provider.calls.load(Ordering::SeqCst), 1);
        cache.invalidate("a");
        cache.observe("a", a, Ok(snapshot(now, 99.0)), now);
        assert!(cache.table().entries["a"].snapshot.is_none());
        assert!(cache.table().entries["b"].valid);
        assert_eq!(cache.bind("a", provider), None);
        let replacement = fake(now);
        let new = cache.rebind("a", replacement);
        assert_ne!(new, a);
        cache.observe("a", a, Ok(snapshot(now, 99.0)), now);
        assert!(cache.table().entries["a"].snapshot.is_none());
    }

    #[tokio::test]
    async fn partial_observations_preserve_missing_fields_and_reset_is_stale() {
        let now = 1_800_000_000;
        let cache = PlanUsageCache::default();
        let generation = cache.bind("alias", fake(now)).unwrap();
        let (tx, mut rx) = mpsc::channel(32);
        cache.subscribe("alias", generation, "root", RequestId(1), &tx, "model");
        cache.observe("alias", generation, Ok(snapshot(now, 90.0)), now);
        loaded_usage(&mut rx).await;
        let mut partial = snapshot(now + 20, 95.0);
        partial.windows[0].resets_at = None;
        partial.windows[0].duration_seconds = None;
        cache.observe("alias", generation, Ok(partial), now + 20);
        let state = loaded_usage(&mut rx).await;
        assert_eq!(
            state.snapshot.unwrap().windows[0].resets_at,
            Some(now + 3600)
        );
        cache.observe("alias", generation, Err(PlanUsageError::Network), now + 30);
        let state = loaded_usage(&mut rx).await;
        assert!(state.stale);
        assert_eq!(
            state.snapshot.unwrap().windows[0]
                .used_percent
                .unwrap()
                .value(),
            95.0
        );
    }
    type LookupReply = tokio::sync::oneshot::Receiver<Result<Option<PlanUsage>, PlanUsageError>>;

    struct Delayed {
        calls: AtomicUsize,
        started: tokio::sync::Notify,
        answer: Mutex<Option<LookupReply>>,
    }
    impl Provider for Delayed {
        fn stream(
            &self,
            _: &Request<'_>,
        ) -> BoxStream<'static, Result<ProviderEvent, ProviderError>> {
            Box::pin(futures::stream::empty())
        }
        fn cache_prefix(&self, _: &Request<'_>) -> Option<String> {
            None
        }
        fn plan_usage(
            &self,
        ) -> futures::future::BoxFuture<'static, Result<Option<PlanUsage>, PlanUsageError>>
        {
            self.calls.fetch_add(1, Ordering::SeqCst);
            let answer = self.answer.lock().unwrap().take().unwrap();
            self.started.notify_one();
            Box::pin(async move { answer.await.unwrap_or(Err(PlanUsageError::Network)) })
        }
    }

    #[tokio::test]
    async fn newer_passive_observations_win_and_release_cancels_single_flight() {
        let now = 1_800_000_000;
        let clock = Arc::new(ShiftedClock::at(Timestamp::from_second(now).unwrap(), None));
        let (answer, wait) = tokio::sync::oneshot::channel();
        let provider = Arc::new(Delayed {
            calls: AtomicUsize::new(0),
            started: tokio::sync::Notify::new(),
            answer: Mutex::new(Some(wait)),
        });
        let cache = PlanUsageCache::default();
        let generation = cache.bind("alias", provider.clone()).unwrap();
        let (tx, mut rx) = mpsc::channel(1);
        cache.subscribe("alias", generation, "root", RequestId(1), &tx, "model");
        cache.refresh("alias", generation, clock.clone(), false);
        within("the lookup's start", provider.started.notified()).await;
        for _ in 0..5 {
            cache.refresh("alias", generation, clock.clone(), true);
        }
        cache.observe("alias", generation, Ok(snapshot(now + 1, 96.0)), now + 1);
        answer.send(Ok(Some(snapshot(now, 50.0)))).unwrap();
        let mut warned = false;
        let latest = loop {
            match rx.recv().await.unwrap() {
                AgentEvent::PlanUsageWarning { threshold: 95, .. } => warned = true,
                AgentEvent::PlanUsageChanged { state, .. } if !state.loading => break state,
                _ => {}
            }
        };
        assert!(warned);
        assert_eq!(
            latest.snapshot.unwrap().windows[0]
                .used_percent
                .unwrap()
                .value(),
            96.0
        );
        assert_eq!(provider.calls.load(Ordering::SeqCst), 1);
        clock.set(Timestamp::from_second(now + 61).unwrap());
        let (mut answer, wait) = tokio::sync::oneshot::channel();
        *provider.answer.lock().unwrap() = Some(wait);
        cache.refresh("alias", generation, clock.clone(), true);
        within("the lookup's start", provider.started.notified()).await;
        cache.release("root", None);
        within("the lookup's end", answer.closed()).await;
        assert!(cache.table().entries["alias"].task.is_none());
        cache.invalidate("alias");
        cache.observe("alias", generation, Ok(snapshot(now + 62, 100.0)), now + 62);
        assert!(cache.table().entries["alias"].snapshot.is_none());
    }

    #[tokio::test]
    async fn failed_refresh_preserves_data_and_backs_off_thirty_minutes_then_an_hour() {
        let now = 1_800_000_000;
        let clock = Arc::new(ShiftedClock::at(Timestamp::from_second(now).unwrap(), None));
        let provider = fake(now);
        let calls = || provider.calls.load(Ordering::SeqCst);
        let cache = PlanUsageCache::default();
        let generation = cache.bind("alias", provider.clone()).unwrap();
        let (tx, mut rx) = mpsc::channel(32);
        cache.subscribe("alias", generation, "root", RequestId(1), &tx, "model");
        cache.observe("alias", generation, Ok(snapshot(now, 50.0)), now);
        loaded_usage(&mut rx).await;
        *provider.result.lock().unwrap() = Err(PlanUsageError::Network);
        let mut current = now + 60;
        for (attempt, delay) in [(1, 1800), (2, 3600), (3, 3600)] {
            clock.set(Timestamp::from_second(current).unwrap());
            cache.refresh("alias", generation, clock.clone(), false);
            let state = loaded_usage(&mut rx).await;
            assert!(state.stale);
            assert!(state.snapshot.is_some());
            assert_eq!(cache.table().entries["alias"].retry_at, current + delay);
            clock.set(Timestamp::from_second(current + delay - 1).unwrap());
            cache.refresh("alias", generation, clock.clone(), false);
            loaded_usage(&mut rx).await;
            assert_eq!(calls(), attempt);
            current += delay;
        }
        clock.set(Timestamp::from_second(current - 3590).unwrap());
        cache.refresh("alias", generation, clock.clone(), true);
        loaded_usage(&mut rx).await;
        assert_eq!(calls(), 4);
        current += 10;
        for (hint, wait) in [
            (Some(0), None),
            (Some(30), Some(30)),
            (None, None),
            (Some(5000), Some(5000)),
        ] {
            *provider.result.lock().unwrap() = Err(PlanUsageError::Throttled {
                retry_at: hint.map(|after| current + after),
            });
            clock.set(Timestamp::from_second(current).unwrap());
            cache.refresh("alias", generation, clock.clone(), true);
            assert_eq!(
                loaded_usage(&mut rx).await.error,
                Some(PlanUsageError::Throttled {
                    retry_at: wait.map(|after| current + after)
                })
            );
            assert_eq!(
                cache.table().entries["alias"].retry_at,
                current + wait.unwrap_or(0).max(3600)
            );
            let before = calls();
            if let Some(wait) = wait {
                clock.set(Timestamp::from_second(current + wait - 1).unwrap());
                cache.refresh("alias", generation, clock.clone(), true);
                loaded_usage(&mut rx).await;
                assert_eq!(calls(), before);
            }
            current += wait.unwrap_or(0).max(10) + 1;
        }
        *provider.result.lock().unwrap() = Ok(Some(snapshot(current, 50.0)));
        clock.set(Timestamp::from_second(current).unwrap());
        cache.refresh("alias", generation, clock.clone(), true);
        loaded_usage(&mut rx).await;
        assert_eq!(cache.table().entries["alias"].retry_at, current + 600);
        *provider.result.lock().unwrap() = Err(PlanUsageError::Throttled {
            retry_at: Some(current + 120),
        });
        clock.set(Timestamp::from_second(current + 60).unwrap());
        cache.refresh("alias", generation, clock.clone(), true);
        loaded_usage(&mut rx).await;
        assert_eq!(cache.table().entries["alias"].retry_at, current + 1860);
    }

    #[tokio::test]
    async fn two_sessions_share_one_record_one_lookup_and_its_wait() {
        let now = 1_800_000_000;
        let clock = Arc::new(ShiftedClock::at(Timestamp::from_second(now).unwrap(), None));
        let at = |second: i64| Timestamp::from_second(now + second).unwrap();
        let folder = tempfile::tempdir().unwrap();
        let session = |alias: &str| {
            let cache = PlanUsageCache::shared(folder.path().to_path_buf());
            let provider = fake(now);
            let generation = cache.bind(alias, provider.clone()).unwrap();
            let (tx, rx) = mpsc::channel(32);
            cache.subscribe(alias, generation, "root", RequestId(1), &tx, "model");
            (cache, provider, generation, tx, rx)
        };
        let (a, a_provider, a_generation, _a_tx, mut a_rx) = session("alias");
        let (b, b_provider, b_generation, _b_tx, mut b_rx) = session("alias");
        let a_calls = || a_provider.calls.load(Ordering::SeqCst);
        let b_calls = || b_provider.calls.load(Ordering::SeqCst);
        a.refresh("alias", a_generation, clock.clone(), false);
        loaded_usage(&mut a_rx).await;
        b.refresh("alias", b_generation, clock.clone(), false);
        let adopted = loaded_usage(&mut b_rx).await;
        assert!(adopted.snapshot.is_some() && !adopted.stale, "{adopted:?}");
        assert_eq!(b_calls(), 0);
        let Claim::Held(held) = record::claim(folder.path(), "alias") else {
            panic!("the lock was taken")
        };
        clock.set(at(600));
        b.refresh("alias", b_generation, clock.clone(), false);
        assert!(!loaded_usage(&mut b_rx).await.loading);
        assert_eq!(b_calls(), 0);
        drop(held);
        *a_provider.result.lock().unwrap() = Err(PlanUsageError::Throttled {
            retry_at: Some(now + 600),
        });
        a.refresh("alias", a_generation, clock.clone(), false);
        assert_eq!(
            loaded_usage(&mut a_rx).await.error,
            Some(PlanUsageError::Throttled { retry_at: None })
        );
        let mut header = snapshot(now + 650, 60.0);
        header.windows[0].source = UsageSource::ResponseHeader;
        b.observe("alias", b_generation, Ok(header.clone()), now + 650);
        loaded_usage(&mut b_rx).await;
        clock.set(at(700));
        b.refresh("alias", b_generation, clock.clone(), false);
        let spaced = loaded_usage(&mut b_rx).await;
        assert_eq!(spaced.error, None);
        assert!(spaced.snapshot.is_some());
        assert_eq!(b_calls(), 0);
        let path = folder.path().join("alias.json");
        let json: serde_json::Value =
            serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
        assert_eq!(json["failure"]["error"]["kind"], "throttled");
        assert_eq!(json["failure"]["at"], now + 600);
        assert_eq!(json["next_attempt"], now + 2400);
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = std::fs::metadata(&path).unwrap().permissions().mode();
            assert_eq!(mode & 0o077, 0, "{mode:o}");
        }
        *b_provider.result.lock().unwrap() = Err(PlanUsageError::Network);
        b.refresh("alias", b_generation, clock.clone(), true);
        loaded_usage(&mut b_rx).await;
        assert_eq!(b_calls(), 1);
        let mut newer: Record = record::read(folder.path(), "alias").unwrap();
        assert_eq!(newer.next_attempt, now + 4300);
        a.observe("alias", a_generation, Ok(header), now + 755);
        loaded_usage(&mut a_rx).await;
        clock.set(at(760));
        a.refresh("alias", a_generation, clock.clone(), true);
        newer.checked = now + 750;
        newer.observed = Some(now + 760);
        record::write(folder.path(), "alias", &newer).unwrap();
        loaded_usage(&mut a_rx).await;
        assert_eq!(a_calls(), 2);
        *a_provider.result.lock().unwrap() = Err(PlanUsageError::Disabled);
        clock.set(at(780));
        a.refresh("alias", a_generation, clock.clone(), true);
        loaded_usage(&mut a_rx).await;
        assert_eq!(a_calls(), 3);
        assert_eq!(record::read(folder.path(), "alias"), Some(newer.clone()));
        assert_eq!(
            a.table().entries["alias"].failure.map(|f| f.at),
            Some(now + 700)
        );
        let quick = fake(now);
        let looked = PlanUsageCache::shared(folder.path().to_path_buf())
            .lookup("alias", quick.clone(), clock.clone())
            .await
            .unwrap();
        assert!(looked.snapshot.is_some());
        assert_eq!(quick.calls.load(Ordering::SeqCst), 0);
        let (c, c_provider, c_generation, _c_tx, mut c_rx) = session("other");
        let other = |reading: Option<PlanUsage>, error: PlanUsageError, checked: i64| Record {
            observed: reading.as_ref().map(|_| now + checked),
            reading,
            next_attempt: now + 5000,
            failures: 1,
            failure: Some(Failure {
                error,
                at: now + checked,
            }),
            checked: now + checked,
        };
        let throttled = PlanUsageError::Throttled {
            retry_at: Some(now + 5000),
        };
        for (stored, shown) in [
            (other(None, throttled, 710), Some(throttled)),
            (other(Some(snapshot(now + 720, 10.0)), throttled, 720), None),
        ] {
            record::write(folder.path(), "other", &stored).unwrap();
            c.refresh("other", c_generation, clock.clone(), false);
            assert_eq!(loaded_usage(&mut c_rx).await.error, shown);
        }
        c.refresh("other", c_generation, clock.clone(), true);
        loaded_usage(&mut c_rx).await;
        assert_eq!(c_provider.calls.load(Ordering::SeqCst), 1);
        let (w, w_provider, w_generation, _w_tx, mut w_rx) = session("denied");
        *w_provider.result.lock().unwrap() = Err(PlanUsageError::Network);
        w.refresh("denied", w_generation, clock.clone(), false);
        loaded_usage(&mut w_rx).await;
        *w_provider.result.lock().unwrap() = Err(PlanUsageError::ScopeRequired);
        clock.set(at(790));
        w.refresh("denied", w_generation, clock.clone(), true);
        loaded_usage(&mut w_rx).await;
        let denied: Record = record::read(folder.path(), "denied").unwrap();
        assert_eq!(denied.next_attempt, now + 790);
        clock.set(at(800));
        let (y, y_provider, y_generation, _y_tx, mut y_rx) = session("denied");
        y.refresh("denied", y_generation, clock.clone(), false);
        let Some(AgentEvent::PlanUsageChanged { state, .. }) = y_rx.recv().await else {
            panic!("no usage state")
        };
        assert!(state.loading && state.error.is_none(), "{state:?}");
        assert!(loaded_usage(&mut y_rx).await.snapshot.is_some());
        assert_eq!(y_provider.calls.load(Ordering::SeqCst), 1);
        w.refresh("denied", w_generation, clock.clone(), false);
        let kept = loaded_usage(&mut w_rx).await;
        assert_eq!(kept.error, Some(PlanUsageError::ScopeRequired));
        assert!(kept.snapshot.is_some());
        let padded = [vec![b' '; 70_000], serde_json::to_vec(&newer).unwrap()].concat();
        std::fs::write(&path, padded).unwrap();
        assert_eq!(record::read::<Record>(folder.path(), "alias"), None);
        std::fs::write(&path, b"{").unwrap();
        assert_eq!(record::read::<Record>(folder.path(), "alias"), None);
        assert!(matches!(
            record::claim(folder.path(), "Bad"),
            Claim::Unshared
        ));
        let (changing, _a_change) = {
            let generation = crate::endpoints::generation::Generation::default();
            let change = generation.change().unwrap();
            (generation, change)
        };
        let refused = Arc::new(Fake {
            generation: Some(changing),
            ..Arc::into_inner(fake(now)).unwrap()
        });
        let state = within(
            "the lookup",
            PlanUsageCache::default().lookup("alias", refused, clock),
        )
        .await
        .unwrap();
        assert!(!state.loading);
    }

    #[tokio::test]
    async fn tier_follows_full_snapshots_and_survives_partial_and_failed_updates() {
        use wisp_protocol::plan_usage::PlanTier;
        let now = 1_800_000_000;
        let clock = Arc::new(ShiftedClock::at(Timestamp::from_second(now).unwrap(), None));
        let provider = fake(now);
        let cache = PlanUsageCache::default();
        let generation = cache.bind("alias", provider.clone()).unwrap();
        let (tx, mut rx) = mpsc::channel(32);
        cache.subscribe("alias", generation, "root", RequestId(1), &tx, "model");
        let mut full = snapshot(now, 50.0);
        full.plan_tier = Some(PlanTier::Pro);
        *provider.result.lock().unwrap() = Ok(Some(full));
        cache.refresh("alias", generation, clock.clone(), true);
        assert_eq!(
            loaded_usage(&mut rx).await.snapshot.unwrap().plan_tier,
            Some(PlanTier::Pro)
        );
        cache.observe("alias", generation, Ok(snapshot(now + 1, 51.0)), now + 1);
        assert_eq!(
            loaded_usage(&mut rx).await.snapshot.unwrap().plan_tier,
            Some(PlanTier::Pro)
        );
        clock.set(Timestamp::from_second(now + 60).unwrap());
        *provider.result.lock().unwrap() = Err(PlanUsageError::Network);
        cache.refresh("alias", generation, clock.clone(), true);
        let failed = loaded_usage(&mut rx).await;
        assert!(failed.error.is_some());
        assert_eq!(failed.snapshot.unwrap().plan_tier, Some(PlanTier::Pro));
        clock.set(Timestamp::from_second(now + 960).unwrap());
        *provider.result.lock().unwrap() = Ok(Some(snapshot(now + 960, 52.0)));
        cache.refresh("alias", generation, clock, true);
        assert_eq!(
            loaded_usage(&mut rx).await.snapshot.unwrap().plan_tier,
            None
        );
    }

    #[tokio::test]
    async fn full_refresh_removes_missing_windows_and_old_windows_stay_stale() {
        let now = 1_800_000_000;
        let clock = Arc::new(ShiftedClock::at(Timestamp::from_second(now).unwrap(), None));
        let provider = fake(now);
        let cache = PlanUsageCache::default();
        let generation = cache.bind("alias", provider.clone()).unwrap();
        let (tx, mut rx) = mpsc::channel(32);
        cache.subscribe("alias", generation, "root", RequestId(1), &tx, "model");
        let mut first = snapshot(now, 50.0);
        let mut extra = first.windows[0].clone();
        extra.key = "model.primary".into();
        extra.scope = UsageScope::Model("other".into());
        first.windows.push(extra);
        cache.observe("alias", generation, Ok(first), now);
        assert_eq!(
            loaded_usage(&mut rx).await.snapshot.unwrap().windows.len(),
            2
        );
        clock.set(Timestamp::from_second(now + 60).unwrap());
        cache.refresh("alias", generation, clock, false);
        assert_eq!(
            loaded_usage(&mut rx).await.snapshot.unwrap().windows.len(),
            1
        );
        cache.observe("alias", generation, Ok(snapshot(now, 96.0)), now + 900);
        assert!(loaded_usage(&mut rx).await.stale);
        let mut reset = snapshot(now + 900, 96.0);
        reset.windows[0].resets_at = Some(now + 900);
        cache.observe("alias", generation, Ok(reset), now + 900);
        assert!(loaded_usage(&mut rx).await.stale);
    }

    #[tokio::test]
    async fn warnings_use_highest_threshold_and_reset_epochs_only_for_relevant_windows() {
        let now = 1_800_000_000;
        let cache = PlanUsageCache::default();
        let generation = cache.bind("alias", fake(now)).unwrap();
        let (tx, mut rx) = mpsc::channel(32);
        cache.subscribe("alias", generation, "root", RequestId(1), &tx, "model");
        cache.observe("alias", generation, Ok(snapshot(now, 96.0)), now);
        loaded_usage(&mut rx).await;
        assert!(matches!(
            rx.recv().await,
            Some(AgentEvent::PlanUsageWarning { threshold: 95, .. })
        ));
        cache.observe("alias", generation, Ok(snapshot(now, 97.0)), now);
        loaded_usage(&mut rx).await;
        assert!(rx.try_recv().is_err());
        let mut other = snapshot(now, 100.0);
        other.windows[0].key = "other.primary".into();
        other.windows[0].scope = UsageScope::Model("other".into());
        cache.observe("alias", generation, Ok(other), now);
        loaded_usage(&mut rx).await;
        assert!(rx.try_recv().is_err());
        assert!(
            !cache.table().entries["alias"]
                .warnings
                .contains_key("other.primary")
        );
        let generation = cache.rebind("alias", fake(now));
        cache.subscribe("alias", generation, "root", RequestId(2), &tx, "model");
        for (percent, threshold) in [(96.0, Some(95)), (50.0, None), (90.0, Some(90))] {
            let mut unknown = snapshot(now, percent);
            unknown.windows[0].resets_at = None;
            cache.observe("alias", generation, Ok(unknown), now);
            loaded_usage(&mut rx).await;
            if let Some(threshold) = threshold {
                assert!(
                    matches!(rx.recv().await, Some(AgentEvent::PlanUsageWarning { threshold: got, .. }) if threshold == got)
                );
            } else {
                assert!(rx.try_recv().is_err());
            }
        }
    }
    #[tokio::test]
    async fn a_shared_reading_is_not_durable_evidence_on_a_reused_alias() {
        let now = 1_800_000_000;
        let folder = tempfile::tempdir().unwrap();
        let cache = PlanUsageCache::shared(folder.path().into());
        let clock = Arc::new(ShiftedClock::at(Timestamp::from_second(now).unwrap(), None));
        let first = cache.bind("example", fake(now)).unwrap();
        let before = fable(now, 100.0);
        record::write(
            folder.path(),
            "example",
            &Record {
                reading: Some(before.clone()),
                observed: Some(now),
                next_attempt: now + 600,
                failures: 0,
                failure: None,
                checked: now,
            },
        )
        .unwrap();
        let provider = fake(now);
        let current = cache.rebind("example", provider.clone());
        cache.hold("example-chat");
        cache.refresh("example", current, clock.clone(), false);
        assert_eq!(provider.calls.load(Ordering::SeqCst), 0);
        assert_eq!(cache.room("example", FABLE, now), named(FABLE, now + 3600));
        assert!(cache.model_limit("example", current, FABLE, now).is_none());
        let mut partial = before.clone();
        partial.windows[0].used_percent = None;
        cache.observe("example", current, Ok(partial), now);
        assert!(cache.model_limit("example", current, FABLE, now).is_none());
        cache.observe("example", current, Ok(before.clone()), now);
        assert_eq!(
            cache.model_limit("example", current, FABLE, now).as_deref(),
            before.windows.first()
        );
        assert!(cache.model_limit("example", first, FABLE, now).is_none());

        let later = now + 508_200;
        cache.reached("example", current, &retry_guard(FABLE, now, later));
        let fresh = fable(now + 1, 0.0);
        record::write(
            folder.path(),
            "example",
            &Record {
                reading: Some(fresh.clone()),
                observed: Some(now + 1),
                next_attempt: now + 600,
                failures: 0,
                failure: None,
                checked: now + 1,
            },
        )
        .unwrap();
        clock.set(Timestamp::from_second(now + 1).unwrap());
        cache.refresh("example", current, clock, false);
        assert_eq!(
            cache.state("example", now + 1).unwrap().snapshot.as_ref(),
            Some(&fresh)
        );
        assert_eq!(
            cache.room("example", FABLE, now + 1),
            reason(Some(FABLE), Some(later), Cause::Retry)
        );
        cache.observe("example", current, Ok(fresh), now + 1);
        assert_eq!(cache.room("example", FABLE, now + 1), Room::Open);
    }

    #[tokio::test]
    async fn named_warnings_and_projections_follow_the_listener_model() {
        let now = 1_800_000_000;
        let cache = PlanUsageCache::default();
        let generation = cache.bind("example", fake(now)).unwrap();
        let (fable_tx, mut fable_rx) = mpsc::channel(8);
        let (opus_tx, mut opus_rx) = mpsc::channel(8);
        cache.subscribe(
            "example",
            generation,
            "first",
            RequestId(1),
            &fable_tx,
            FABLE,
        );
        cache.subscribe(
            "example",
            generation,
            "second",
            RequestId(2),
            &opus_tx,
            OPUS,
        );
        let mut reading = fable(now, 95.0);
        let mut collision = reading.windows[0].clone();
        collision.key = "unknown".into();
        collision.scope = UsageScope::ModelFamily("claude".into());
        reading.windows.push(collision.clone());
        cache.observe("example", generation, Ok(reading.clone()), now);
        let first = loaded_usage(&mut fable_rx).await.snapshot.unwrap();
        assert_eq!(first.windows[0].scope, UsageScope::Model(FABLE.into()));
        assert_eq!(first.windows[1], collision);
        assert!(
            matches!(fable_rx.recv().await, Some(AgentEvent::PlanUsageWarning { threshold: 95, window, .. }) if window == reading.windows[0])
        );
        assert_eq!(
            loaded_usage(&mut opus_rx).await.snapshot,
            Some(reading.clone())
        );
        cache.model("example", "second", OPUS, || {
            panic!("unchanged model needs no clock")
        });
        cache.model("example", "second", "claude-fable-5-1", || now);
        let switched = loaded_usage(&mut opus_rx).await.snapshot.unwrap();
        assert_eq!(
            switched.windows[0].scope,
            UsageScope::Model("claude-fable-5-1".into())
        );
        assert_eq!(cache.state("example", now).unwrap().snapshot, Some(reading));
        assert!(opus_rx.try_recv().is_err());
    }

    #[tokio::test]
    async fn stale_child_binding_cannot_replace_reconnected_endpoint_or_listeners() {
        let now = 1_800_000_000;
        let cache = PlanUsageCache::default();
        let old = fake(now);
        let first = cache.bind("alias", old.clone()).unwrap();
        let limit = snapshot(now, 100.0).windows.remove(0);
        cache.reached("alias", first, &limit);
        assert!(matches!(
            cache.room("alias", "model", now),
            Room::Used { .. }
        ));
        let current = fake(now);
        let generation = cache.rebind("alias", current.clone());
        assert_eq!(cache.room("alias", "model", now), Room::Open);
        cache.reached("alias", first, &limit);
        assert_eq!(cache.room("alias", "model", now), Room::Open);
        assert_ne!(first, generation);
        let (tx, mut rx) = mpsc::channel(8);
        cache.subscribe("alias", generation, "root", RequestId(1), &tx, "model");
        assert_eq!(cache.bind("alias", old), None);
        assert_eq!(cache.bind("alias", current), Some(generation));
        cache.observe("alias", first, Ok(snapshot(now, 99.0)), now);
        cache.observe("alias", generation, Ok(snapshot(now, 50.0)), now);
        assert_eq!(
            loaded_usage(&mut rx).await.snapshot.unwrap().windows[0]
                .used_percent
                .unwrap()
                .value(),
            50.0
        );
        cache.reached("alias", generation, &limit);
        let mut stale = limit;
        stale.observed_at += 1;
        stale.resets_at = None;
        cache.reached("alias", first, &stale);
        assert_eq!(
            cache.room("alias", "model", now),
            Room::Used {
                model: None,
                cause: wisp_tool_text::agents::Cause::Limit,
                resets_at: Some(now + 3600),
            }
        );
    }

    #[tokio::test]
    async fn changing_lookup_permission_aborts_pending_work_and_discards_its_result() {
        let now = 1_800_000_000;
        let clock = Arc::new(ShiftedClock::at(Timestamp::from_second(now).unwrap(), None));
        let (answer, wait) = tokio::sync::oneshot::channel();
        let provider = Arc::new(Delayed {
            calls: AtomicUsize::new(0),
            started: tokio::sync::Notify::new(),
            answer: Mutex::new(Some(wait)),
        });
        let cache = PlanUsageCache::default();
        let generation = cache.bind("alias", provider.clone()).unwrap();
        let (tx, mut rx) = mpsc::channel(64);
        cache.subscribe("alias", generation, "root", RequestId(1), &tx, "model");
        cache.refresh("alias", generation, clock, true);
        within("the lookup's start", provider.started.notified()).await;
        cache.reset_lookup("alias", now);
        let state = loaded_usage(&mut rx).await;
        assert!(!state.loading);
        assert!(state.snapshot.is_none());
        tokio::task::yield_now().await;
        assert!(answer.send(Ok(Some(snapshot(now, 100.0)))).is_err());
        assert!(cache.table().entries["alias"].snapshot.is_none());
    }

    #[tokio::test]
    async fn in_flight_full_lookup_merges_unobserved_windows_and_replaces_its_inventory() {
        let now = 1_800_000_000;
        let clock = Arc::new(ShiftedClock::at(Timestamp::from_second(now).unwrap(), None));
        let (answer, wait) = tokio::sync::oneshot::channel();
        let provider = Arc::new(Delayed {
            calls: AtomicUsize::new(0),
            started: tokio::sync::Notify::new(),
            answer: Mutex::new(Some(wait)),
        });
        let cache = PlanUsageCache::default();
        let generation = cache.bind("alias", provider.clone()).unwrap();
        let (tx, mut rx) = mpsc::channel(64);
        cache.subscribe("alias", generation, "root", RequestId(1), &tx, "model");
        let mut initial = snapshot(now, 20.0);
        let mut removed = initial.windows[0].clone();
        removed.key = "model.removed".into();
        initial.windows.push(removed.clone());
        let mut retained = removed.clone();
        retained.key = "model.retained".into();
        initial.windows.push(retained.clone());
        answer.send(Ok(Some(initial))).unwrap();
        cache.refresh("alias", generation, clock.clone(), false);
        loaded_usage(&mut rx).await;
        removed.source = UsageSource::ResponseEvent;
        removed.observed_at = now + 5;
        cache.observe(
            "alias",
            generation,
            Ok(PlanUsage::new(None, vec![removed]).unwrap()),
            now + 5,
        );
        loaded_usage(&mut rx).await;
        clock.set(Timestamp::from_second(now + 60).unwrap());
        let (answer, wait) = tokio::sync::oneshot::channel();
        *provider.answer.lock().unwrap() = Some(wait);
        cache.refresh("alias", generation, clock.clone(), true);
        let mut passive = snapshot(now + 61, 80.0);
        passive.windows[0].source = UsageSource::ResponseHeader;
        let mut only_passive = passive.windows[0].clone();
        only_passive.key = "passive.primary".into();
        passive.windows.push(only_passive);
        retained.source = UsageSource::ResponseHeader;
        retained.observed_at = now + 61;
        passive.windows.push(retained);
        cache.observe("alias", generation, Ok(passive), now + 61);
        let mut full = snapshot(now + 60, 30.0);
        let mut model = full.windows[0].clone();
        model.key = "model.new".into();
        model.scope = UsageScope::Model("model".into());
        model.used_percent = Some(Percent::try_from(96.0).unwrap());
        full.windows.push(model);
        answer.send(Ok(Some(full))).unwrap();
        clock.set(Timestamp::from_second(now + 62).unwrap());
        let result = loaded_usage(&mut rx).await;
        assert!(!result.stale);
        let windows = result.snapshot.unwrap().windows;
        assert_eq!(windows.len(), 4);
        assert_eq!(windows[0].key, "codex.primary");
        assert_eq!(windows[0].used_percent.unwrap().value(), 80.0);
        assert_eq!(windows[1].key, "model.new");
        assert_eq!(windows[1].used_percent.unwrap().value(), 96.0);
        assert_eq!(windows[2].key, "model.retained");
        assert_eq!(windows[3].key, "passive.primary");
        let warning = rx.recv().await.unwrap();
        assert!(
            matches!(warning, AgentEvent::PlanUsageWarning { threshold: 95, window, .. } if window.key == "model.new")
        );
    }
}
