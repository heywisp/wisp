use std::collections::{BTreeMap, BTreeSet};
use std::sync::Arc;

use futures::future::BoxFuture;
use futures::stream::FuturesUnordered;
use futures::{FutureExt, StreamExt};
use tokio::sync::mpsc;
use tokio::time::Instant;
use wisp_contain::{Contained, contain};
use wisp_plugin_api::Plugin;
use wisp_protocol::AgentEvent;
use wisp_protocol::slot::{self, SlotContent, SlotFailure, SlotInvalid, SlotView};

use super::{
    Board, CALL_LIMIT, Events, Failed, MIN_GAP, PARK_AFTER, Section, Sections, Shown, Signals,
    locked, one_line, retry_after,
};
use crate::plugin::caller::{Caller, Render};
use crate::plugin::{convert, panicked};

#[derive(Debug, Default)]
struct View {
    marked: bool,
    started: Option<Instant>,
    good: Option<Vec<u8>>,
    failures: u32,
    parks: u32,
    parked: bool,
    retry_at: Option<Instant>,
    resend: bool,
}

impl View {
    fn unpark(&mut self) {
        self.resend |= self.parked || self.failures > 0;
        self.parked = false;
        self.failures = 0;
        self.parks = 0;
        self.retry_at = None;
        self.marked = true;
    }

    fn retry(&mut self, now: Instant) {
        if self.parked && self.retry_at.is_some_and(|at| at <= now) {
            self.parked = false;
            self.retry_at = None;
            self.resend = true;
            self.marked = true;
        }
    }

    fn wake(&self, blocked: bool) -> Option<Instant> {
        if self.parked {
            return self.retry_at;
        }
        if self.marked && !blocked {
            return self.started.map(|at| at + MIN_GAP);
        }
        None
    }
}

struct Entry {
    section: Section,
    plugin: Option<Arc<dyn Plugin>>,
    panel_shown: bool,
    panel: View,
    page: Option<View>,
    next_refresh: Option<Instant>,
}

impl Entry {
    fn show(&mut self, shown: &Shown, now: Instant) {
        let panel = shown.sections.contains(&self.section.name);
        if panel && !self.panel_shown {
            self.panel.unpark();
        }
        if !panel {
            self.panel.marked = false;
        }
        self.panel_shown = panel;
        let page = shown.pages.contains(&self.section.name);
        if !page {
            self.page = None;
        } else if self.page.is_none() {
            let mut fresh = View::default();
            fresh.unpark();
            self.page = Some(fresh);
        }
        if !panel && !page {
            self.next_refresh = None;
        } else if self.next_refresh.is_none() {
            self.next_refresh = self.section.refresh.map(|every| now + every);
        }
    }

    fn shown_mut(&mut self, view: SlotView) -> Option<&mut View> {
        match view {
            SlotView::Panel if self.panel_shown => Some(&mut self.panel),
            SlotView::Page => self.page.as_mut(),
            SlotView::Panel | SlotView::Unknown => None,
        }
    }

    fn shown(&self) -> impl Iterator<Item = &View> {
        self.panel_shown
            .then_some(&self.panel)
            .into_iter()
            .chain(self.page.as_ref())
    }

    fn unpark(&mut self) {
        if self.panel_shown {
            self.panel.unpark();
        }
        if let Some(page) = &mut self.page {
            page.unpark();
        }
    }

    fn mark(&mut self) {
        for view in [SlotView::Panel, SlotView::Page] {
            if let Some(state) = self.shown_mut(view) {
                state.marked = true;
            }
        }
    }

    fn absorb(&mut self, points: &BTreeSet<String>, now: Instant) {
        if self.section.marks.iter().any(|m| points.contains(m)) {
            self.mark();
        }
        if self.next_refresh.is_some_and(|at| at <= now) {
            self.mark();
            self.next_refresh = self.section.refresh.map(|every| now + every);
        }
        if self.panel_shown {
            self.panel.retry(now);
        }
        if let Some(page) = &mut self.page {
            page.retry(now);
        }
    }
}

struct Done {
    index: usize,
    view: SlotView,
    plugin: String,
    outcome: Result<SlotContent, (SlotFailure, bool)>,
}

pub(super) struct Lane {
    entries: Vec<Entry>,
    signals: Arc<Signals>,
    board: Board,
    events: Option<Events>,
    busy: BTreeSet<String>,
    off: BTreeSet<String>,
    running: FuturesUnordered<BoxFuture<'static, Done>>,
}

impl Lane {
    pub(super) fn new(
        sections: &Sections,
        plugins: &BTreeMap<String, Arc<dyn Plugin>>,
        signals: Arc<Signals>,
        board: Board,
    ) -> Self {
        let entries = sections
            .list()
            .iter()
            .map(|section| Entry {
                section: section.clone(),
                plugin: plugins.get(&section.plugin).cloned(),
                panel_shown: false,
                panel: View::default(),
                page: None,
                next_refresh: None,
            })
            .collect();
        Self {
            entries,
            signals,
            board,
            events: None,
            busy: BTreeSet::new(),
            off: BTreeSet::new(),
            running: FuturesUnordered::new(),
        }
    }

    pub(super) async fn run(mut self) {
        loop {
            let now = Instant::now();
            self.absorb(now);
            self.start_due(now);
            let wake = self.next_wake();
            tokio::select! {
                () = self.signals.wake.notified() => {}
                Some(done) = self.running.next(), if !self.running.is_empty() => {
                    self.finish(done);
                }
                () = tokio::time::sleep_until(wake.unwrap_or(now)), if wake.is_some() => {}
            }
        }
    }

    fn absorb(&mut self, now: Instant) {
        if let Some(off) = self.signals.take_off() {
            for entry in &mut self.entries {
                let plugin = &entry.section.plugin;
                if self.off.contains(plugin) && !off.contains(plugin) {
                    entry.unpark();
                }
            }
            self.off = off;
        }
        if let Some((shown, events)) = self.signals.take_shown() {
            self.events = events;
            for entry in &mut self.entries {
                entry.show(&shown, now);
            }
        }
        let points = self.signals.take_points();
        for entry in &mut self.entries {
            entry.absorb(&points, now);
        }
    }

    fn start_due(&mut self, now: Instant) {
        let mut due: Vec<(Option<Instant>, usize, SlotView)> = Vec::new();
        for (index, entry) in self.entries.iter_mut().enumerate() {
            if entry.plugin.is_none()
                || entry.section.cycle.is_some()
                || self.off.contains(&entry.section.plugin)
            {
                continue;
            }
            for view in [SlotView::Panel, SlotView::Page] {
                let Some(state) = entry.shown_mut(view) else {
                    continue;
                };
                let resting = state.started.is_some_and(|at| now < at + MIN_GAP);
                if state.marked && !state.parked && !resting {
                    due.push((state.started, index, view));
                }
            }
        }
        due.sort_by_key(|(started, index, _)| (*started, *index));
        for (_, index, view) in due {
            let Some(entry) = self.entries.get_mut(index) else {
                continue;
            };
            let id = entry.section.plugin.clone();
            if self.busy.contains(&id) {
                continue;
            }
            let (Some(plugin), short) = (entry.plugin.clone(), entry.section.short.clone()) else {
                continue;
            };
            let Some(state) = entry.shown_mut(view) else {
                continue;
            };
            state.started = Some(now);
            let Some(held) = plugin.lock_for_render() else {
                continue;
            };
            state.marked = false;
            self.busy.insert(id.clone());
            self.running
                .push(render(index, view, plugin, short, id, held));
        }
    }

    fn next_wake(&self) -> Option<Instant> {
        let mut wake: Option<Instant> = None;
        let mut sooner = |at: Option<Instant>| {
            if let Some(at) = at {
                wake = Some(wake.map_or(at, |w| w.min(at)));
            }
        };
        for entry in &self.entries {
            sooner(entry.next_refresh);
            if self.off.contains(&entry.section.plugin) {
                continue;
            }
            let blocked = entry.plugin.is_none()
                || entry.section.cycle.is_some()
                || self.busy.contains(&entry.section.plugin);
            for view in entry.shown() {
                sooner(view.wake(blocked));
            }
        }
        wake
    }

    fn finish(&mut self, done: Done) {
        self.busy.remove(&done.plugin);
        let now = Instant::now();
        let Some(entry) = self.entries.get_mut(done.index) else {
            return;
        };
        let section = entry.section.name.clone();
        let state = match done.view {
            SlotView::Page => match entry.page.as_mut() {
                Some(page) => page,
                None => return,
            },
            SlotView::Panel | SlotView::Unknown => &mut entry.panel,
        };
        let panel = done.view == SlotView::Panel;
        let hidden = panel && !entry.panel_shown;
        let outcome = done
            .outcome
            .and_then(|content| checked(content, done.view).map_err(|failure| (failure, false)));
        let event = match outcome {
            Ok((content, bytes)) => {
                let send = state.resend || state.good.as_ref() != Some(&bytes);
                state.good = Some(bytes);
                state.failures = 0;
                state.parks = 0;
                state.resend = false;
                if panel {
                    let mut board = locked(&self.board);
                    let current = board.entry(section.clone()).or_default();
                    current.content = Some(content.clone());
                    current.failure = None;
                }
                send.then_some(AgentEvent::SlotContent {
                    section,
                    view: done.view,
                    content,
                })
            }
            Err((failure, restarted)) => {
                state.failures = state.failures.saturating_add(1);
                state.resend = true;
                let parked = state.failures >= PARK_AFTER;
                if parked {
                    state.parked = true;
                    state.parks = state.parks.saturating_add(1);
                    state.retry_at = (!restarted).then(|| now + retry_after(state.parks));
                }
                let failed = Failed {
                    failure,
                    parked,
                    restarted,
                };
                if panel {
                    locked(&self.board)
                        .entry(section.clone())
                        .or_default()
                        .failure = Some(failed.clone());
                }
                Some(failed.event(section, done.view))
            }
        };
        if hidden {
            state.resend = true;
            return;
        }
        let events = self.events.as_ref().and_then(Events::upgrade);
        let (Some(events), Some(event)) = (events, event) else {
            return;
        };
        if let Err(mpsc::error::TrySendError::Full(_)) = events.try_send(event) {
            state.resend = true;
            state.marked = true;
        }
    }
}

fn checked(content: SlotContent, view: SlotView) -> Result<(SlotContent, Vec<u8>), SlotFailure> {
    match slot::check(&content, view) {
        Err(SlotInvalid::TooLarge { bytes }) => Err(SlotFailure::TooLarge { bytes }),
        Err(SlotInvalid::Rule(rule)) => Err(SlotFailure::Invalid {
            rule: rule.to_owned(),
        }),
        Ok(()) => serde_json::to_vec(&content)
            .map(|bytes| (content, bytes))
            .map_err(|e| SlotFailure::Failed {
                message: one_line(&e.to_string()),
            }),
    }
}

fn render(
    index: usize,
    view: SlotView,
    plugin: Arc<dyn Plugin>,
    short: String,
    id: String,
    held: Box<dyn Send>,
) -> BoxFuture<'static, Done> {
    async move {
        let _held = held;
        let outcome = call(plugin.as_ref(), &short, view, &id).await;
        Done {
            index,
            view,
            plugin: id,
            outcome,
        }
    }
    .boxed()
}

pub(in crate::plugin) async fn call(
    plugin: &dyn Plugin,
    short: &str,
    view: SlotView,
    id: &str,
) -> Result<SlotContent, (SlotFailure, bool)> {
    let restarts = plugin.restarts_when_cut();
    let failed = |message: String| SlotFailure::Failed { message };
    let caller = Caller {
        deadline: Some(Instant::now() + CALL_LIMIT),
        ..Caller::rendering(Render::Slot)
    };
    let Ok(future) = caller
        .clone()
        .sync_scope(|| contain(|| plugin.render_slot(short, convert::slot_view(view))))
    else {
        return Err((failed(panicked(id)), restarts));
    };
    let run = caller.scope(Contained::new(future));
    match tokio::time::timeout(CALL_LIMIT, run).await {
        Err(_) => Err((SlotFailure::TimedOut, restarts)),
        Ok(Err(_)) => Err((failed(panicked(id)), restarts)),
        Ok(Ok(Err(message))) => Err((failed(one_line(&message)), false)),
        Ok(Ok(Ok(content))) => Ok(convert::slot_content(content)),
    }
}
