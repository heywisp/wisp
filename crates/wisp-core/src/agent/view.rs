use std::collections::{BTreeMap, BTreeSet};

use super::{Agent, PairState, emit};
use crate::provider::pair_features;
use tokio::sync::mpsc;
use wisp_protocol::log::{LogEvent, LogRecord};
use wisp_protocol::{
    AgentEvent, NextTurn, Now, Pair, RequestId, Returned, ReturnedImage, SessionView,
};

impl Agent {
    pub fn view(&self, records: &[LogRecord]) -> SessionView {
        self.view_builder()(records)
    }

    fn view_builder(&self) -> impl FnOnce(&[LogRecord]) -> SessionView + Send + 'static {
        let returned = self
            .returned_inputs()
            .iter()
            .map(|input| Returned {
                id: Some(input.id.clone()),
                text: input.text.clone(),
                images: input
                    .images
                    .iter()
                    .map(|image| ReturnedImage {
                        data: image.data.data.clone(),
                        width: image.image.width,
                        height: image.image.height,
                    })
                    .collect(),
                missing: input.missing.len(),
            })
            .collect();
        let toolbox = self.toolbox().clone();
        let offer = self.retry_offer();
        let pairs = self.restored_pairs();
        let mut suffix = self.restored_processes();
        suffix.push(self.context_now());
        let endpoint = self.endpoint_name().to_owned();
        move |records| {
            let mut events = crate::resume::transcript(records, &toolbox, offer);
            events.retain(|event| {
                let AgentEvent::FeatureStateChanged {
                    endpoint,
                    model,
                    feature,
                    state,
                } = event
                else {
                    return true;
                };
                !pair_features().any(|pair| pair == *feature)
                    || pairs.get(&(endpoint.clone(), model.clone(), *feature))
                        == Some(&PairState::from(*state))
            });
            events.extend(suffix);
            let started = records
                .iter()
                .find(|record| !crate::resume::carried(&record.event))
                .and_then(|record| {
                    let LogEvent::SessionStarted {
                        endpoint_name,
                        model,
                        effort,
                        ..
                    } = &record.event
                    else {
                        return None;
                    };
                    Some(Pair {
                        endpoint: endpoint_name.clone().unwrap_or(endpoint),
                        model: model.clone(),
                        effort: *effort,
                    })
                });
            SessionView {
                events,
                returned,
                input_ids: input_ids(records),
                retried_on: retried_on(records, started.as_ref()),
                started,
            }
        }
    }
}

fn retried_on(records: &[LogRecord], started: Option<&Pair>) -> BTreeMap<u64, Pair> {
    records
        .iter()
        .filter_map(|record| match &record.event {
            LogEvent::TurnStarted {
                turn,
                retry_of: Some(_),
                model,
                effort,
                endpoint_name,
                ..
            } => {
                let endpoint = endpoint_name
                    .clone()
                    .or_else(|| started.map(|p| p.endpoint.clone()))?;
                let model = model.clone().or_else(|| started.map(|p| p.model.clone()))?;
                let effort = effort.or_else(|| started.map(|p| p.effort))?;
                Some((
                    *turn,
                    Pair {
                        endpoint,
                        model,
                        effort,
                    },
                ))
            }
            _ => None,
        })
        .collect()
}

pub fn input_ids(records: &[LogRecord]) -> Vec<String> {
    let mut ids = Vec::new();
    for record in records {
        match &record.event {
            LogEvent::TurnStarted {
                input_id, inputs, ..
            } => {
                ids.extend(input_id.iter().map(|id| id.0.clone()));
                ids.extend(inputs.iter().map(|i| i.id.0.clone()));
            }
            LogEvent::InputAccepted { id, .. } => ids.push(id.0.clone()),
            LogEvent::InputCommitted { inputs, .. } => {
                ids.extend(inputs.iter().map(|i| i.id.0.clone()));
            }
            _ => {}
        }
    }
    ids
}

#[derive(Default)]
pub(super) struct Cut {
    pub(super) seq: Option<u64>,
    waiting: Vec<AgentEvent>,
}

impl Agent {
    pub fn now(&self) -> Now {
        Now {
            endpoint: self.endpoint_name().to_owned(),
            model: self.session.model.clone(),
            effort: self.session.effort,
            cache_ttl: self.session.cache_ttl,
            next: self.next_turn(),
            endpoints: self.settings.layers.endpoints.clone(),
        }
    }

    pub fn next_turn(&self) -> Option<NextTurn> {
        let pending = self.pending_settings();
        let cache_ttl = self.next_cache_ttl();
        if pending.is_none() && cache_ttl.is_none() {
            return None;
        }
        let (endpoint, model, effort) = match pending {
            Some((endpoint, model, effort)) => (
                (endpoint != self.endpoint_name()).then(|| endpoint.to_owned()),
                (model != self.session.model).then(|| model.to_owned()),
                (effort != self.session.effort).then_some(effort),
            ),
            None => (None, None, None),
        };
        Some(NextTurn {
            endpoint,
            model,
            effort,
            cache_ttl,
        })
    }

    pub(super) fn cut_view(&mut self) {
        self.view_cut.seq = self.log.as_ref().map(|(log, _)| log.next_seq());
        self.view_cut.waiting = self.inbox.accepted_at_cut();
    }

    pub(super) async fn describe(&mut self, answers: RequestId, events: &mpsc::Sender<AgentEvent>) {
        let cut = self.view_cut.seq.unwrap_or(0);
        let reader = self
            .log
            .as_ref()
            .and_then(|(log, _)| log.store().reader(log.id(), log.next_seq()));
        let build = self.view_builder();
        let waiting = self.view_cut.waiting.clone();
        let held = self.inbox.ids().map(|id| id.0.clone()).collect::<Vec<_>>();
        let now = self.now();
        let mut state = vec![
            self.spend_snapshot(),
            self.context_now(),
            self.agents_snapshot(None),
            self.catalog_now(),
        ];
        state.extend(self.agents.warm_now());
        state.extend(self.agents.live_processes());
        if !self.agents.in_tree() {
            state.extend(self.processes.started_events());
        }
        state.extend(self.posted_event());
        let snapshot = move |mut records: Vec<LogRecord>| {
            let mut ids = input_ids(&records);
            let mut seen = ids.iter().cloned().collect::<BTreeSet<_>>();
            ids.extend(held.into_iter().filter(|id| seen.insert(id.clone())));
            records.retain(|record| record.seq < cut);
            let mut view = build(&records);
            view.input_ids = ids;
            view.events.extend(waiting);
            AgentEvent::SessionSnapshot {
                answers,
                view,
                now,
                state,
            }
        };
        if let Some(read) = reader {
            self.queue.background(async move {
                let result = tokio::task::spawn_blocking(move || {
                    read().map(|records| Box::new(snapshot(records)))
                })
                .await;
                let snapshot = result.ok().and_then(Result::ok).ok_or(());
                super::queue::Done::Session {
                    request: answers,
                    snapshot,
                }
            });
        } else {
            let records = match &self.log {
                Some((log, _)) => log.store().read(log.id()),
                None => Ok(Vec::new()),
            };
            let event = match records {
                Ok(records) => snapshot(records),
                Err(_) => AgentEvent::refused(answers, "couldn't read the session log"),
            };
            emit(events, event).await;
        }
    }

    pub(super) fn catalog_now(&self) -> AgentEvent {
        AgentEvent::CatalogChanged {
            catalog: self.catalog.clone(),
        }
    }

    pub(super) fn agents_snapshot(&self, request: Option<RequestId>) -> AgentEvent {
        AgentEvent::AgentsSnapshot {
            request,
            agents: self.agents.snapshot(),
        }
    }

    pub(super) async fn window_gone(&mut self, events: &mpsc::Sender<AgentEvent>, closing: bool) {
        self.slots_detached();
        if !closing {
            self.agents.window_gone().await;
        }
        self.plan_usage.release(&self.session_id, None);
        self.detach(wisp_protocol::DetachReason::Disconnected, events)
            .await;
    }

    pub(crate) fn processes(&self) -> crate::jobs::Processes {
        self.processes.clone()
    }
}
