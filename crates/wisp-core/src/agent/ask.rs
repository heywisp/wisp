use std::collections::BTreeSet;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, PoisonError};

use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;
use wisp_plugin_api::model::{AskError, Prompt, Reply, Source};
use wisp_protocol::log::LogEvent;
use wisp_protocol::{AgentEvent, ErrorKind, SessionConfig, StopReason};

use super::side::{self, Landed, Watch, kind_name};
use super::{Agent, emit};
use crate::endpoints::catalog::CatalogReader;
use crate::lanes::Lanes;
use crate::provider::{Provider, ProviderError, Refused, SearchSignal, ServerToolSpec};

pub(crate) struct Asker {
    provider: Arc<dyn Provider>,
    session: SessionConfig,
    documented: bool,
    native: Vec<ServerToolSpec>,
    chat_native: Vec<ServerToolSpec>,
    side_model: String,
    lanes: Lanes,
    depth: u32,
    catalog: Option<CatalogReader>,
    watch: Watch,
}

pub(crate) struct ModelAsk {
    landed: Landed,
    model: String,
    detail: Option<String>,
}

struct Pending<'a> {
    records: &'a Mutex<Vec<ModelAsk>>,
    sent: Arc<AtomicBool>,
    record: Option<ModelAsk>,
}

impl Drop for Pending<'_> {
    fn drop(&mut self) {
        if let Some(mut record) = self.record.take() {
            if self.sent.load(Ordering::Relaxed) {
                record.landed.error = record.landed.error.map(ProviderError::possibly_billed);
            }
            self.records
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .push(record);
        }
    }
}

impl Asker {
    pub(crate) async fn ask(
        &self,
        req: Prompt,
        cancel: &CancellationToken,
        records: &Mutex<Vec<ModelAsk>>,
    ) -> Result<Reply, AskError> {
        let native = match req.native.as_deref() {
            None => &[][..],
            Some(family) => {
                let tool = self
                    .native
                    .iter()
                    .find(|tool| tool.name == family)
                    .ok_or_else(|| {
                        AskError::NotAllowed(format!("{family} is not on for this chat"))
                    })?;
                std::slice::from_ref(tool)
            }
        };
        let model = if native.is_empty() {
            &self.session.model
        } else {
            &self.side_model
        };
        let reply = self.once(&req, model, native, cancel, records).await;
        if matches!(reply, Err(AskError::Refused(_))) && model != &self.session.model {
            return self
                .once(
                    &req,
                    &self.session.model,
                    &self.chat_native,
                    cancel,
                    records,
                )
                .await;
        }
        reply
    }

    async fn once(
        &self,
        req: &Prompt,
        model: &str,
        native: &[ServerToolSpec],
        cancel: &CancellationToken,
        records: &Mutex<Vec<ModelAsk>>,
    ) -> Result<Reply, AskError> {
        let mut session = self.session.clone();
        session.model = model.to_owned();
        session.system = Some(req.system.clone());
        session.max_tokens = req.max_tokens.min(session.max_tokens);
        let sent = Arc::new(AtomicBool::new(false));
        let mut pending = Pending {
            records,
            sent: sent.clone(),
            record: Some(ModelAsk {
                landed: Landed::failed(ProviderError::new(ErrorKind::Interrupted, "cancelled")),
                model: session.model.clone(),
                detail: Some("cancelled".to_owned()),
            }),
        };
        let request = async {
            let pass = side::admit(
                &self.lanes,
                &self.watch.alias,
                self.depth,
                self.provider.generation(),
                self.catalog.clone(),
            )
            .await
            .ok_or_else(|| ProviderError::new(ErrorKind::Other, "not admitted"));
            match pass {
                Ok(pass) => {
                    side::drain(
                        side::one_shot(
                            self.provider.as_ref(),
                            session,
                            self.documented,
                            req.text.clone(),
                            native,
                        ),
                        None,
                        pass,
                        &self.watch,
                        model,
                        sent,
                    )
                    .await
                }
                Err(error) => Landed::failed(error),
            }
        };
        let landed = tokio::select! {
            biased;
            () = cancel.cancelled() => return Err(AskError::Cancelled),
            landed = request => landed,
        };
        let reply = reply_of(self.provider.as_ref(), &landed);
        if let Some(mut record) = pending.record.take() {
            record.detail = reply.as_ref().err().map(|error| match error {
                AskError::NotAllowed(reason)
                | AskError::Refused(reason)
                | AskError::Failed(reason) => reason.clone(),
                AskError::Cancelled => "cancelled".to_owned(),
            });
            record.landed = landed;
            records
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .push(record);
        }
        reply
    }
}

fn reply_of(provider: &dyn Provider, landed: &Landed) -> Result<Reply, AskError> {
    if let Some(error) = landed.failure() {
        return Err(if error.refused == Some(Refused::NativeSearch) {
            AskError::Refused("native_search".to_owned())
        } else {
            AskError::Failed(kind_name(error.kind))
        });
    }
    if let Some(stop) = landed
        .stop
        .as_ref()
        .filter(|stop| !matches!(stop, StopReason::EndTurn | StopReason::MaxTokens))
    {
        return Err(AskError::Failed(stop.as_str().to_owned()));
    }
    let mut sources = Vec::new();
    for signal in signals_of(provider, landed) {
        if let SearchSignal::Cited { sources: cited } = signal {
            for source in cited {
                let source = Source {
                    title: source.title.unwrap_or_default(),
                    url: source.url,
                };
                if !sources.contains(&source) {
                    sources.push(source);
                }
            }
        }
    }
    Ok(Reply {
        text: landed.asm.partial_text(),
        sources,
    })
}

fn signals_of<'a>(
    provider: &'a dyn Provider,
    landed: &'a Landed,
) -> impl Iterator<Item = SearchSignal> + 'a {
    landed.search.iter().cloned().chain(
        landed
            .asm
            .blocks()
            .filter_map(|block| provider.search_signal(block)),
    )
}

impl Agent {
    pub(crate) fn asker(&self) -> Asker {
        let provider = self.provider().clone();
        let side_model = crate::search::side_model(provider.as_ref(), &self.session.model);
        let native = |model: &str| {
            if self.server_tools().is_empty() {
                Vec::new()
            } else {
                self.search_facts(self.endpoint_name(), model)
                    .native()
                    .cloned()
                    .into_iter()
                    .collect()
            }
        };
        let chat_native = native(&self.session.model);
        let native = native(&side_model);
        let (session, features) = self.bare_request_parts();
        Asker {
            provider,
            session,
            documented: features.documented,
            native,
            chat_native,
            side_model,
            lanes: self.lanes.clone(),
            depth: self.path().depth(),
            catalog: self.endpoint_catalog.clone(),
            watch: self.watch(),
        }
    }

    pub(super) async fn record_asks(
        &mut self,
        call_id: &str,
        plugin: &str,
        records: Vec<ModelAsk>,
        events: &mpsc::Sender<AgentEvent>,
    ) {
        if records.is_empty() {
            return;
        }
        let mut records = records.into_iter().peekable();
        while let Some(record) = records.next() {
            self.record_side(events).await;
            let signals: Vec<_> = signals_of(self.provider().as_ref(), &record.landed).collect();
            let (mut first, mut started, mut here) = (None, 0, BTreeSet::new());
            for mut signal in signals {
                match &mut signal {
                    SearchSignal::Started { call_id: id, .. } if first.is_none() => {
                        first = Some(id.clone());
                        *id = call_id.to_owned();
                    }
                    SearchSignal::Ended { call_id: id, .. }
                        if first.as_deref() == Some(id.as_str()) =>
                    {
                        *id = call_id.to_owned()
                    }
                    SearchSignal::Cited { sources } => {
                        if record.detail.is_none() {
                            for source in sources.drain(..) {
                                if !self
                                    .track()
                                    .model_sources
                                    .iter()
                                    .any(|seen| seen.url == source.url)
                                {
                                    self.track().model_sources.push(source);
                                }
                            }
                        }
                        continue;
                    }
                    _ => {}
                }
                self.on_search(
                    signal,
                    0,
                    (&mut started, &mut here, &BTreeSet::new()),
                    events,
                )
                .await;
            }
            if !here.is_empty() {
                self.end_unanswered(wisp_protocol::UnfinishedReason::Failed, events)
                    .await;
            }
            let served = record.landed.asm.model();
            let model = if served.is_empty() {
                &record.model
            } else {
                served
            };
            let usage = record.landed.asm.usage();
            let uncounted = record.landed.uncounted(usage.is_some());
            let spend = self.spend(usage, model, record.landed.asm.iterations(), uncounted);
            let cost = self.add_side(spend, model);
            if let Some(track) = self.track.as_mut() {
                track.add(spend);
            }
            self.log(
                events,
                LogEvent::ModelAsked {
                    call_id: call_id.to_owned(),
                    plugin: plugin.to_owned(),
                    turn: self.track.as_ref().map(|track| track.index),
                    detail: record.detail,
                    usage: spend.usage,
                    cost_usd: spend.usd,
                    list_usd: spend.list_usd,
                    plan_usd: spend.plan_usd(),
                    model: Some(model.to_owned()),
                    lower_bound: spend.lower_bound,
                },
            )
            .await;
            emit(
                events,
                AgentEvent::ModelAsked {
                    call_id: call_id.to_owned(),
                    cost,
                },
            )
            .await;
            if records.peek().is_none()
                && record
                    .landed
                    .failure()
                    .is_some_and(|error| error.refused == Some(Refused::NativeSearch))
            {
                self.mark_search_refused(events).await;
            }
        }
        self.refresh_plan_usage();
    }
}
