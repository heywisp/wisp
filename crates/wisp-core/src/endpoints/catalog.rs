use std::collections::{BTreeMap, BTreeSet};
use std::path::PathBuf;
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};

use tokio::sync::watch;

use super::generation::{Change, Guarded, Unavailable};
use super::host::Reconnected;
use crate::kit::{Connect, Connected};
use crate::models::list::{LIST_WAIT, ListWait};

#[derive(Clone)]
pub struct LiveCatalog {
    shared: Arc<Mutex<State>>,
    changed: watch::Sender<u64>,
    watch: Option<Arc<super::watch::Watch>>,
    lists: Option<PathBuf>,
}

#[derive(Clone)]
pub struct CatalogReader(LiveCatalog);

impl CatalogReader {
    pub fn providers_off(&self) -> crate::plugin::registry::ProvidersOff {
        self.0.providers_off()
    }
    pub fn set_providers_off(&self, off: crate::plugin::registry::ProvidersOff) {
        self.0.set_providers_off(off);
    }
    pub fn revision(&self) -> u64 {
        self.0.revision()
    }
    pub fn changes(&self) -> watch::Receiver<u64> {
        self.0.changes()
    }
    pub fn view(&self) -> Reconnected {
        self.0.view()
    }
    pub async fn check(&self) {
        self.0.check().await;
    }
    pub async fn refetch(&self, alias: &str, always: bool) {
        self.0.refetch(alias, always).await;
    }
    pub(crate) fn needs_refetch(&self, alias: &str, model: &str) -> bool {
        self.0.state().needs_refetch(alias, model)
    }
    pub async fn refetch_unlisted(&self, alias: &str, model: &str) -> bool {
        let mut finished = {
            let mut state = self.0.state();
            if !state.needs_refetch(alias, model) {
                return false;
            }
            state
                .refetched
                .entry((alias.to_owned(), model.to_owned()))
                .or_insert_with(|| {
                    let (running, finished) = watch::channel(());
                    let catalog = self.clone();
                    let alias = alias.to_owned();
                    let deadline = tokio::time::Instant::now() + LIST_WAIT;
                    tokio::spawn(async move {
                        let wait = ListWait::default();
                        wait.until(deadline, wait.fetch(catalog.refetch(&alias, true)))
                            .await;
                        drop(running);
                    });
                    finished
                })
                .clone()
        };
        let _ = finished.changed().await;
        true
    }
    pub fn removing(&self, alias: &str) -> bool {
        self.0.state().removed.contains(alias)
    }
    pub fn take_removed(&self) -> Vec<String> {
        std::mem::take(&mut self.0.state().removed)
            .into_iter()
            .collect()
    }
}

struct State {
    revision: u64,
    connected: Connected,
    endpoints: Vec<wisp_protocol::settings::EndpointSummary>,
    changing: BTreeSet<String>,
    generations: BTreeMap<String, super::generation::Generation>,
    usage: crate::plan_usage::PlanUsageCache,
    own: BTreeSet<String>,
    removed: BTreeSet<String>,
    refetched: BTreeMap<(String, String), watch::Receiver<()>>,
    off: crate::plugin::registry::ProvidersOff,
}

impl State {
    fn needs_refetch(&self, alias: &str, model: &str) -> bool {
        if let Some(finished) = self.refetched.get(&(alias.to_owned(), model.to_owned())) {
            return finished.has_changed().is_ok();
        }
        self.connected.ready.iter().any(|connection| {
            connection.name == alias
                && connection.provider.list_key().is_some()
                && crate::agents::endpoints::provider_mismatch(connection.provider.as_ref(), model)
                    .is_ok()
                && connection
                    .provider
                    .model_list()
                    .is_some_and(|list| list.find(model).is_none())
        })
    }
}

fn active_provider(
    connected: &Connected,
    active: &str,
    fallback: &Arc<dyn crate::provider::Provider>,
) -> Arc<dyn crate::provider::Provider> {
    connected
        .ready
        .iter()
        .find(|connection| connection.name == active)
        .map(|connection| connection.provider.clone())
        .unwrap_or_else(|| Guarded::wrap(fallback.clone()))
}

impl LiveCatalog {
    pub fn providers_off(&self) -> crate::plugin::registry::ProvidersOff {
        self.state().off.clone()
    }

    pub fn set_providers_off(&self, off: crate::plugin::registry::ProvidersOff) {
        self.state().off = off;
    }

    pub fn reader(&self) -> CatalogReader {
        CatalogReader(self.clone())
    }

    pub fn new(mut connected: Connected) -> Self {
        for connection in &mut connected.ready {
            connection.provider = Guarded::wrap(connection.provider.clone());
        }
        connected.provider = active_provider(&connected, &connected.active, &connected.provider);
        let mut generations: BTreeMap<_, _> = connected
            .ready
            .iter()
            .filter_map(|connection| {
                Some((connection.name.clone(), connection.provider.generation()?))
            })
            .collect();
        for (alias, _) in &connected.unavailable {
            generations.entry(alias.clone()).or_default();
        }
        Self {
            shared: Arc::new(Mutex::new(State {
                revision: 0,
                connected,
                endpoints: Vec::new(),
                changing: BTreeSet::new(),
                generations,
                usage: crate::plan_usage::PlanUsageCache::default(),
                own: BTreeSet::new(),
                removed: BTreeSet::new(),
                refetched: BTreeMap::new(),
                off: crate::plugin::registry::ProvidersOff::default(),
            })),
            changed: watch::channel(0).0,
            watch: None,
            lists: None,
        }
    }

    pub fn with_start(self, active: String, provider: &Arc<dyn crate::provider::Provider>) -> Self {
        {
            let mut state = self.state();
            state.connected.provider = active_provider(&state.connected, &active, provider);
            state.connected.active = active;
        }
        self
    }

    pub fn with_lists(mut self, folder: Option<PathBuf>) -> Self {
        self.lists = folder;
        self
    }

    pub fn with_watch(
        mut self,
        source: Arc<dyn super::watch::AccountsWatch>,
        prints: super::watch::Prints,
    ) -> Self {
        self.watch = Some(Arc::new(super::watch::Watch::new(source, prints)));
        self
    }

    pub async fn check(&self) {
        if let Some(watch) = &self.watch {
            watch.check(self).await;
        }
    }

    pub(super) fn taken_here(&self) -> BTreeSet<String> {
        let state = &mut *self.state();
        let mut taken: BTreeSet<String> = state
            .own
            .extract_if(.., |name| !state.changing.contains(name))
            .collect();
        taken.extend(state.changing.iter().cloned());
        taken
    }

    pub(super) fn removed_elsewhere(&self, alias: &str, view: Reconnected) {
        if !self.view().lists(alias) {
            return;
        }
        let Ok(change) = self.begin_inner(alias, Mode::Close) else {
            return;
        };
        change.publish(view);
        self.state().removed.insert(alias.to_owned());
    }

    pub(super) fn provider_off(&self, alias: &str) -> bool {
        let state = self.state();
        state.connected.ready.iter().any(|connection| {
            connection.name == alias && state.off.contains_key(connection.provider.kind().as_str())
        })
    }

    pub(super) fn changed_elsewhere(
        &self,
        alias: &str,
        view: Reconnected,
    ) -> Result<(), Unavailable> {
        let mode = if self.provider_off(alias) {
            Mode::Close
        } else {
            Mode::Watch
        };
        let change = self.begin_inner(alias, mode)?;
        self.state().removed.remove(alias);
        change.publish(view);
        Ok(())
    }

    pub(super) fn remarked(&self, endpoints: &[wisp_protocol::settings::EndpointSummary]) {
        let mut state = self.state();
        let mut changed = false;
        for summary in &mut state.endpoints {
            let mark = endpoints
                .iter()
                .find(|e| e.name == summary.name)
                .and_then(|e| e.mark);
            changed |= std::mem::replace(&mut summary.mark, mark) != mark;
        }
        if changed {
            state.revision += 1;
            self.changed.send_replace(state.revision);
        }
    }

    pub async fn refetch(&self, alias: &str, always: bool) {
        let Some(folder) = self.lists.clone() else {
            return;
        };
        let provider = {
            let state = self.state();
            let expired = state.endpoints.iter().any(|e| e.name == alias && e.expired);
            state
                .connected
                .ready
                .iter()
                .find(|connection| connection.name == alias)
                .map(|connection| connection.provider.clone())
                .filter(|provider| provider.list_key().is_some() && (always || !expired))
        };
        let Some(provider) = provider else {
            return;
        };
        provider.refresh_list(folder, always).await;
        let state = &mut *self.state();
        let listing = crate::agent::account_listing(&state.connected.ready, alias);
        let summary = state.endpoints.iter_mut().find(|e| e.name == alias);
        if let Some(summary) = summary.filter(|summary| {
            summary.models != listing.models
                || summary.complete != listing.complete
                || summary.default_model != listing.default_model
        }) {
            summary.models = listing.models;
            summary.complete = listing.complete;
            summary.default_model = listing.default_model;
            state.revision += 1;
            self.changed.send_replace(state.revision);
        }
    }

    pub fn with_summaries(self, endpoints: Vec<wisp_protocol::settings::EndpointSummary>) -> Self {
        self.state().endpoints = endpoints;
        self
    }

    pub fn with_usage(self, usage: crate::plan_usage::PlanUsageCache) -> Self {
        self.state().usage = usage;
        self
    }

    fn state(&self) -> MutexGuard<'_, State> {
        self.shared.lock().unwrap_or_else(PoisonError::into_inner)
    }

    pub fn revision(&self) -> u64 {
        self.state().revision
    }
    pub fn changes(&self) -> watch::Receiver<u64> {
        self.changed.subscribe()
    }

    pub fn view(&self) -> Reconnected {
        let state = self.state();
        Reconnected {
            ready: state.connected.ready.clone(),
            unavailable: state.connected.unavailable.clone(),
            endpoints: state.endpoints.clone(),
        }
    }

    pub fn begin(&self, alias: &str) -> Result<CatalogChange, Unavailable> {
        self.begin_inner(alias, Mode::Change)
    }
    pub fn begin_logout(&self, alias: &str) -> Result<CatalogChange, Unavailable> {
        self.begin_inner(alias, Mode::Logout)
    }

    fn begin_inner(&self, alias: &str, mode: Mode) -> Result<CatalogChange, Unavailable> {
        let mut state = self.state();
        if state.changing.contains(alias) {
            return Err(Unavailable::Changing);
        }
        let generation = state
            .generations
            .get(alias)
            .map(|generation| match mode {
                Mode::Change | Mode::Watch => generation.change(),
                Mode::Logout => generation.remove(),
                Mode::Close => generation.close(),
            })
            .transpose()?;
        if matches!(mode, Mode::Change | Mode::Logout) {
            state.own.insert(alias.to_owned());
        }
        state.changing.insert(alias.to_owned());
        Ok(CatalogChange {
            catalog: self.clone(),
            alias: alias.to_owned(),
            generation,
            released: false,
        })
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Mode {
    Change,
    Logout,
    Watch,
    Close,
}

impl Connect for LiveCatalog {
    fn connect(&self, _path: &str) -> Result<Connected, String> {
        Ok(self.state().connected.clone())
    }
}

pub struct CatalogChange {
    catalog: LiveCatalog,
    alias: String,
    generation: Option<Change>,
    released: bool,
}

impl CatalogChange {
    pub fn publish(mut self, mut refreshed: Reconnected) -> u64 {
        let mut state = self.catalog.state();
        refreshed
            .ready
            .retain(|connection| connection.name == self.alias);
        let generation = self
            .generation
            .as_ref()
            .map(Change::replacement)
            .unwrap_or_default();
        if refreshed.lists(&self.alias) {
            state
                .generations
                .insert(self.alias.clone(), generation.clone());
        } else {
            state.generations.remove(&self.alias);
        }
        for connection in &mut refreshed.ready {
            connection.provider =
                Guarded::replacement(connection.provider.clone(), generation.clone());
        }
        refreshed.ready.extend(
            state
                .connected
                .ready
                .iter()
                .filter(|connection| connection.name != self.alias)
                .cloned(),
        );
        refreshed
            .unavailable
            .retain(|(name, _)| name == &self.alias);
        refreshed.unavailable.extend(
            state
                .connected
                .unavailable
                .iter()
                .filter(|(name, _)| name != &self.alias)
                .cloned(),
        );
        let mut summaries = state.endpoints.clone();
        summaries.retain(|summary| summary.name != self.alias);
        for summary in &mut summaries {
            if let Some(current) = refreshed
                .endpoints
                .iter()
                .find(|current| current.name == summary.name)
            {
                summary.default = current.default;
                summary.mark = current.mark;
            }
        }
        summaries.extend(
            refreshed
                .endpoints
                .into_iter()
                .filter(|summary| summary.name == self.alias),
        );
        refreshed.endpoints = summaries;
        refreshed
            .ready
            .sort_by(|left, right| left.name.cmp(&right.name));
        refreshed
            .unavailable
            .sort_by(|left, right| left.0.cmp(&right.0));
        refreshed
            .endpoints
            .sort_by(|left, right| left.name.cmp(&right.name));
        state.usage.invalidate(&self.alias);
        if let Some(connection) = refreshed
            .ready
            .iter()
            .find(|connection| connection.name == self.alias)
        {
            state.usage.rebind(&self.alias, connection.provider.clone());
        }
        if let Some(generation) = self.generation.take() {
            generation.retire();
        }
        if let Some(active) = refreshed
            .ready
            .iter()
            .find(|connection| connection.name == state.connected.active)
        {
            state.connected.provider = active.provider.clone();
        }
        state.connected.ready = refreshed.ready;
        state.connected.unavailable = refreshed.unavailable;
        state.endpoints = refreshed.endpoints;
        state.revision += 1;
        let revision = state.revision;
        state.changing.remove(&self.alias);
        self.released = true;
        self.catalog.changed.send_replace(revision);
        revision
    }
}

impl Drop for CatalogChange {
    fn drop(&mut self) {
        if !self.released {
            drop(self.generation.take());
            let revision = {
                let mut state = self.catalog.state();
                state.changing.remove(&self.alias);
                state.own.remove(&self.alias);
                state.revision
            };
            self.catalog.changed.send_replace(revision);
        }
    }
}
