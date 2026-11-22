use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, PoisonError};

use futures::StreamExt;
use futures::stream::BoxStream;
use tokio::sync::mpsc;
use tokio::task::JoinHandle;
use wisp_protocol::AgentEvent;
use wisp_protocol::plan_usage::{PlanUsage, PlanUsageError, UsageWindow};
use wisp_protocol::{Effort, ErrorKind, Message, SessionConfig, StopReason};

use super::{Agent, Spend};
use crate::endpoints::catalog::CatalogReader;
use crate::endpoints::generation::Generation;
use crate::lanes::{Lanes, Pass};
use crate::plan_usage::GuardStamp;
use crate::provider::{
    AssembledMessage, Assembler, CompactionAsk, Features, Provider, ProviderError, ProviderEvent,
    Rendering, Request, RequestOptions, SearchSignal, ServerToolSpec, UNRENDERED,
};
use crate::warmth::Ticket;

pub(super) type Observation = Result<PlanUsage, PlanUsageError>;
pub(super) type Replies = BoxStream<'static, Result<ProviderEvent, ProviderError>>;
pub(super) type Unknowns = Arc<Mutex<Vec<(Option<u64>, String, serde_json::Value)>>>;

pub(super) struct Watch {
    unknown: Unknowns,
    turn: Option<u64>,
    cache: crate::plan_usage::PlanUsageCache,
    pub(super) alias: String,
    generation: Option<crate::plan_usage::Generation>,
    clock: Arc<dyn crate::clock::Clock>,
}

impl Watch {
    pub(super) fn sending(&self, model: &str) -> Option<GuardStamp> {
        self.cache.guard_stamp(&self.alias, self.generation?, model)
    }

    pub(super) fn succeeded(&self, model: &str, sent_at: Option<GuardStamp>) {
        if let (Some(generation), Some(sent_at)) = (self.generation, sent_at) {
            self.cache
                .succeeded(&self.alias, generation, model, sent_at);
        }
    }

    pub(super) fn note(&self, error: &mut ProviderError, model: &str) {
        if let Some(generation) = self.generation
            && error
                .detail
                .limit_reached
                .as_deref()
                .is_some_and(UsageWindow::is_retry_guard)
        {
            error.detail.model_limit = self.cache.model_limit(
                &self.alias,
                generation,
                model,
                self.clock.now().as_second(),
            );
        }
    }

    pub(super) fn failed(&self, error: &ProviderError) {
        if let Some(window) = &error.detail.limit_reached {
            self.reached(window);
        }
    }

    fn reached(&self, window: &UsageWindow) {
        if let Some(generation) = self.generation {
            self.cache.reached(&self.alias, generation, window);
        }
    }

    pub(super) fn observe(&self, observation: Observation) {
        if let Some(generation) = self.generation {
            let now = self.clock.now().as_second();
            self.cache
                .observe(&self.alias, generation, observation, now);
        }
    }
}

impl Agent {
    pub(super) fn watch(&self) -> Watch {
        let alias = self.endpoint_name();
        Watch {
            unknown: self.side_unknown.clone(),
            turn: self.running_turn(),
            cache: self.plan_usage.clone(),
            alias: alias.to_owned(),
            generation: self.usage_generations.get(alias).copied(),
            clock: self.clock.clone(),
        }
    }
}

impl Agent {
    pub(super) fn side_spend(&self, landed: &Landed, requested: &str) -> (Spend, String) {
        let served = landed.asm.model();
        let model = if served.is_empty() { requested } else { served };
        let iterations = landed.asm.iterations();
        let usage = landed
            .asm
            .usage()
            .map(|u| super::compact::summed(&u, iterations));
        let uncounted = landed.uncounted(usage.is_some());
        let spend = self.spend(usage, model, iterations, uncounted);
        (spend, model.to_owned())
    }
}

pub(super) struct Landed {
    pub(super) search: Vec<SearchSignal>,
    pub(super) asm: Assembler,
    pub(super) stop: Option<StopReason>,
    pub(super) error: Option<ProviderError>,
}

impl Landed {
    pub(super) fn failed(error: ProviderError) -> Self {
        Self {
            search: Vec::new(),
            asm: Assembler::default(),
            stop: None,
            error: Some(error),
        }
    }

    pub(super) fn failure(&self) -> Option<ProviderError> {
        self.error.clone().or_else(|| self.asm.ended().err())
    }

    pub(super) fn uncounted(&self, usage: bool) -> bool {
        self.failure().is_some_and(|error| error.uncounted(usage))
    }

    pub(super) fn finish(self) -> Result<AssembledMessage, ProviderError> {
        match self.error {
            Some(error) => Err(error),
            None => self.asm.finish(),
        }
    }
}

pub(super) async fn drain(
    mut replies: Replies,
    mut warming: Option<Ticket>,
    pass: Pass,
    watch: &Watch,
    model: &str,
    sent: Arc<AtomicBool>,
) -> Landed {
    let mut search = Vec::new();
    let mut asm = Assembler::default();
    let mut stop = None;
    let mut sent_at = None;
    let error = loop {
        match replies.next().await {
            Some(Ok(ProviderEvent::PlanUsage(observation))) => watch.observe(observation),
            Some(Ok(event)) => {
                if matches!(event, ProviderEvent::Sending) {
                    sent_at = watch.sending(model);
                    sent.store(true, Ordering::Relaxed);
                } else if let Some(ticket) = warming.as_mut() {
                    ticket.streamed();
                }
                if let ProviderEvent::MessageDelta {
                    stop_reason: Some(reason),
                    ..
                } = &event
                {
                    stop = Some(reason.clone());
                }
                if let ProviderEvent::Unknown { provider, raw } = event {
                    watch
                        .unknown
                        .lock()
                        .unwrap_or_else(PoisonError::into_inner)
                        .push((watch.turn, provider, raw));
                    continue;
                }
                if let ProviderEvent::Search { signal, .. } = event {
                    search.push(signal);
                    continue;
                }
                if let Err(error) = asm.apply(event) {
                    break Some(error);
                }
            }
            Some(Err(error)) => {
                watch.failed(&error);
                break Some(error);
            }
            None => break None,
        }
    };
    drop(replies);
    drop(pass);
    if error.is_none() && asm.ended().is_ok() {
        watch.succeeded(model, sent_at);
    }
    Landed {
        search,
        asm,
        stop,
        error,
    }
}

pub(super) async fn joined(handle: &mut JoinHandle<Landed>) -> Landed {
    handle.await.unwrap_or_else(|error| {
        Landed::failed(ProviderError::new(ErrorKind::Other, error.to_string()))
    })
}

pub(super) async fn landed(handle: Option<&mut JoinHandle<Landed>>) -> Landed {
    match handle {
        Some(handle) => joined(handle).await,
        None => std::future::pending().await,
    }
}

pub(super) async fn admit(
    lanes: &Lanes,
    endpoint: &str,
    rank: u32,
    generation: Option<Generation>,
    catalog: Option<CatalogReader>,
) -> Option<Pass> {
    if let Some(catalog) = catalog {
        catalog.check().await;
    }
    let admitted = crate::endpoints::generation::admit(generation).ok()?;
    Some(lanes.pass(endpoint, rank).await.with_generation(admitted))
}

pub(super) fn kind_name(kind: ErrorKind) -> String {
    serde_json::to_value(kind)
        .ok()
        .and_then(|v| v.as_str().map(str::to_owned))
        .unwrap_or_else(|| kind.as_str().to_owned())
}

static UNMARKED: CompactionAsk = CompactionAsk {
    native_instructions: None,
    tail: None,
};

pub(crate) fn served_side_model(provider: &dyn Provider, model: &str) -> bool {
    provider.official() && provider.capabilities(model).is_some()
}

pub(crate) fn one_shot_model(provider: &dyn Provider, chat: &str) -> String {
    crate::models::ModelTable::shared()
        .sibling(chat, Effort::Low)
        .filter(|model| served_side_model(provider, model))
        .unwrap_or(chat)
        .to_owned()
}

pub(super) fn one_shot(
    provider: &dyn Provider,
    mut session: SessionConfig,
    documented: bool,
    text: String,
    server_tools: &[ServerToolSpec],
) -> Replies {
    if provider
        .capabilities(&session.model)
        .is_none_or(|row| row.takes_effort(Effort::Low))
    {
        session.effort = Effort::Low;
    }
    let messages = [Message::user_text(text)];
    let rendering = Rendering {
        drop_base_marker: true,
        search_only: !server_tools.is_empty(),
        ..UNRENDERED.clone()
    };
    provider.stream(&Request {
        session: &session,
        tools: &[],
        deferred: &[],
        by_value: &[],
        server_tools,
        messages: &messages,
        previous_response_id: None,
        features: Features {
            documented,
            ..Features::default()
        },
        options: RequestOptions {
            debug_cache: false,
            refusal_fallback: false,
            message_threads: false,
        },
        rendering: &rendering,
        compaction: Some(&UNMARKED),
    })
}

impl Agent {
    pub(super) async fn record_side(&mut self, events: &mpsc::Sender<AgentEvent>) {
        let unknown = std::mem::take(
            &mut *self
                .side_unknown
                .lock()
                .unwrap_or_else(PoisonError::into_inner),
        );
        for (turn, provider, event) in unknown {
            Box::pin(self.emit_unboxed(
                events,
                AgentEvent::UnknownProviderEvent {
                    provider,
                    event: crate::provider::without_identity(event),
                },
                turn,
            ))
            .await;
        }
    }
}
