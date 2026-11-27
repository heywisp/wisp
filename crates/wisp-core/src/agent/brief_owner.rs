use std::collections::BTreeMap;
use std::path::PathBuf;
use std::sync::{Arc, Mutex, MutexGuard, PoisonError, Weak};

use wisp_protocol::log::{LogEvent, LoggedInput};
use wisp_protocol::{InputId, TextSpan};

use super::{Agent, Input};
use crate::secrets::scan::scrub_checklist_spans;
use crate::session::{Redactions, SessionLog};

#[derive(Clone)]
pub(super) struct SharedLog(Arc<Mutex<SessionLog>>);

impl SharedLog {
    pub(super) fn new(log: SessionLog) -> Self {
        Self(Arc::new(Mutex::new(log)))
    }

    pub(super) fn downgrade(&self) -> Weak<Mutex<SessionLog>> {
        Arc::downgrade(&self.0)
    }

    pub(super) fn lock(&self) -> MutexGuard<'_, SessionLog> {
        self.0.lock().unwrap_or_else(PoisonError::into_inner)
    }

    pub(super) fn write(
        log: &mut SessionLog,
        event: LogEvent,
        sync: bool,
        generation: Option<u64>,
        home: Option<&std::path::Path>,
    ) -> Result<(bool, Option<String>), ()> {
        if !log.is_on() {
            return Ok((false, None));
        }
        let mut failure = match generation {
            Some(generation) => log.record_if_generation_current(generation, event, home)?,
            None => log.record_with_home(event, home),
        };
        if failure.is_none() && sync {
            failure = log.sync();
        }
        Ok((failure.is_none(), failure))
    }
}

#[derive(Clone, Default)]
pub(crate) struct BriefOwner(Arc<Mutex<OwnerState>>);

#[derive(Default)]
pub(super) struct OwnerState {
    pub(super) log: Option<Weak<Mutex<SessionLog>>>,
    pub(super) configured_log: bool,
    pub(super) closed: bool,
    pub(super) ended: bool,
    pub(super) home: Option<PathBuf>,
    pub(super) waiting: BTreeMap<InputId, WaitingBrief>,
}

pub(super) struct WaitingBrief {
    pub(super) input: LoggedInput,
    pub(super) revision_unlogged: bool,
    pub(super) active_turn: Option<u64>,
    pub(super) in_flight: bool,
}

pub(super) enum Projection {
    Ready,
    Revise,
    Suppress(Option<String>),
}

impl BriefOwner {
    pub(super) fn materialized(&self, ids: Vec<InputId>) {
        if !ids.is_empty() {
            let mut owner = self.lock();
            for id in ids {
                owner.waiting.remove(&id);
            }
        }
    }

    pub(super) fn lock(&self) -> MutexGuard<'_, OwnerState> {
        self.0.lock().unwrap_or_else(PoisonError::into_inner)
    }

    pub(super) fn remember(&self, input: &Input, home: Option<PathBuf>) {
        if input.checklist_spans.is_empty() {
            return;
        }
        let mut owner = self.lock();
        owner.home = home;
        owner.remember(input);
    }

    pub(super) fn read(&self, input: &mut Input) {
        if !input.checklist_spans.is_empty() {
            self.lock().read(input);
        }
    }

    pub(super) fn project(
        &self,
        id: &InputId,
        text: &mut String,
        spans: &mut Vec<TextSpan>,
        fallback_home: Option<&std::path::Path>,
        held: &Redactions,
    ) -> Projection {
        let owner = self.lock();
        let waiting = owner.waiting.get(id);
        if let Some(waiting) = waiting {
            text.clone_from(&waiting.input.text);
            spans.clone_from(&waiting.input.checklist_spans);
        }
        let changed =
            scrub_checklist_spans(text, spans, owner.home.as_deref().or(fallback_home), held);
        if waiting.is_some_and(|entry| changed || entry.revision_unlogged) {
            Projection::Revise
        } else {
            Projection::Ready
        }
    }

    pub(super) fn persist(
        &self,
        id: &InputId,
        text: &mut String,
        spans: &mut Vec<TextSpan>,
        fallback_home: Option<&std::path::Path>,
    ) -> Projection {
        let mut owner = self.lock();
        let OwnerState {
            log,
            configured_log,
            closed,
            ended,
            home,
            waiting,
        } = &mut *owner;
        let Some(entry) = waiting.get_mut(id) else {
            return Projection::Ready;
        };
        let log = log.as_ref().and_then(Weak::upgrade).map(SharedLog);
        let mut writer = log.as_ref().map(SharedLog::lock);
        let resolved_home = home.as_deref().or(fallback_home);
        loop {
            let (held, generation) = crate::session::held_with_generation();
            let mut input = entry.input.clone();
            let changed = scrub_checklist_spans(
                &mut input.text,
                &mut input.checklist_spans,
                resolved_home,
                &held,
            );
            if changed || entry.revision_unlogged {
                if entry.in_flight || *ended {
                    return Projection::Suppress(None);
                }
                if *configured_log {
                    let Some(writer) = writer.as_deref_mut() else {
                        return Projection::Suppress(None);
                    };
                    let event = LogEvent::InputRevised {
                        turn: entry.active_turn,
                        inputs: vec![input.clone()],
                    };
                    let Ok((persisted, failure)) =
                        SharedLog::write(writer, event, !*closed, Some(generation), resolved_home)
                    else {
                        continue;
                    };
                    if !persisted {
                        entry.input = input;
                        entry.revision_unlogged = true;
                        return Projection::Suppress(failure.map(|_| {
                            "session log write failed; logging is off for this session".to_owned()
                        }));
                    }
                }
                entry.input = input;
                entry.revision_unlogged = false;
            }
            break;
        }
        text.clone_from(&entry.input.text);
        spans.clone_from(&entry.input.checklist_spans);
        Projection::Ready
    }
}

impl OwnerState {
    pub(super) fn remember(&mut self, input: &Input) {
        if !input.checklist_spans.is_empty() {
            self.waiting
                .entry(input.id.clone())
                .or_insert_with(|| WaitingBrief {
                    input: input.logged(),
                    revision_unlogged: input.revision_unlogged,
                    active_turn: None,
                    in_flight: false,
                });
        }
    }

    pub(super) fn read(&self, input: &mut Input) {
        if let Some(waiting) = self.waiting.get(&input.id) {
            input.text.clone_from(&waiting.input.text);
            input
                .checklist_spans
                .clone_from(&waiting.input.checklist_spans);
            input.revision_unlogged = waiting.revision_unlogged;
        }
    }

    pub(super) fn update(&mut self, input: LoggedInput, turn: Option<u64>, dirty: bool) {
        if let Some(entry) = self.waiting.get_mut(&input.id) {
            entry.input = input;
            entry.active_turn = turn;
            entry.revision_unlogged = dirty;
            entry.in_flight = false;
        }
    }
}

impl Agent {
    pub(crate) fn brief_owner(&self) -> BriefOwner {
        self.brief_owner.clone()
    }

    pub(super) fn set_closed(&mut self, closed: bool) {
        self.closed = closed;
        self.brief_owner.lock().closed = closed;
    }
}

#[cfg(test)]
impl BriefOwner {
    pub(crate) fn mark_in_flight_for_test(&self, id: &InputId) {
        self.lock().waiting.get_mut(id).unwrap().in_flight = true;
    }
}

#[cfg(test)]
mod tests;
