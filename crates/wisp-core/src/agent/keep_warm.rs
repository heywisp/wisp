use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use jiff::{SignedDuration, Timestamp};
use tokio::sync::mpsc;
use tokio::task::JoinHandle;
use tokio::time::Instant;
use wisp_protocol::log::LogEvent;
use wisp_protocol::{
    AgentEvent, CacheTtl, RefreshOutcome, RetryClass, Role, StopReason, TurnOutcome, Usage,
};

use super::retry::{backoff, is_busy};
use super::side::{Landed, Replies, kind_name};
use super::{Agent, Spend, emit};
use crate::lanes::{Lanes, Pass};
use crate::provider::{ProviderError, Request, RequestOptions};

pub(super) const DUE_AFTER: SignedDuration = SignedDuration::from_mins(55);
pub(super) const LATE_AFTER: SignedDuration = SignedDuration::from_mins(58);
const LATE: &str = "late";
const CHANGED: &str = "changed";

type Admission = Pin<Box<dyn Future<Output = Option<Pass>> + Send>>;

#[derive(Default)]
pub(super) struct KeepWarm {
    forced: Option<u32>,
    pub(super) resumed: Option<crate::resume::WarmSpell>,
    spell: Option<Spell>,
    flight: Option<Flight>,
}

struct Spell {
    anchor: Anchor,
    count: u32,
    tries: u32,
    late: Instant,
    admission: Option<Admission>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Anchor {
    turn: u64,
    view_len: usize,
    used_at: Timestamp,
}

struct Flight {
    anchor: Anchor,
    sound: bool,
    count: u32,
    tries: u32,
    late: Instant,
    started_at: Timestamp,
    detached: bool,
    sent: Arc<AtomicBool>,
    handle: JoinHandle<Landed>,
}

struct Judged {
    outcome: RefreshOutcome,
    detail: Option<String>,
    spend: Spend,
    model: Option<String>,
}

fn admission(
    lanes: Lanes,
    endpoint: String,
    due: Instant,
    late: Instant,
    generation: Option<crate::endpoints::generation::Generation>,
    catalog: Option<crate::endpoints::catalog::CatalogReader>,
) -> Admission {
    Box::pin(async move {
        tokio::time::timeout_at(late, async move {
            tokio::time::sleep_until(due).await;
            super::side::admit(
                &lanes,
                &endpoint,
                crate::lanes::REFRESH_RANK,
                generation,
                catalog,
            )
            .await
        })
        .await
        .ok()
        .flatten()
    })
}

pub(super) enum Woke {
    Due(Option<Pass>),
    Landed(Box<Landed>),
}

pub(super) async fn woke(warm: &mut KeepWarm) -> Woke {
    let KeepWarm { spell, flight, .. } = warm;
    tokio::select! {
        biased;
        landed = landed(flight) => Woke::Landed(Box::new(landed)),
        pass = admitted(spell) => Woke::Due(pass),
    }
}

async fn admitted(spell: &mut Option<Spell>) -> Option<Pass> {
    match spell.as_mut().and_then(|s| s.admission.as_mut()) {
        Some(admission) => admission.await,
        None => std::future::pending().await,
    }
}

async fn landed(flight: &mut Option<Flight>) -> Landed {
    super::side::landed(flight.as_mut().map(|f| &mut f.handle)).await
}

fn after(at: Timestamp, span: SignedDuration) -> Timestamp {
    at.checked_add(span).unwrap_or(Timestamp::MAX)
}

fn read_enough(usage: &Usage) -> bool {
    let read = usage.cache_read_input_tokens;
    read > 0 && read.saturating_mul(2) >= usage.prompt_tokens()
}

fn retry_after(error: &ProviderError) -> Option<Duration> {
    match error.detail.retry {
        RetryClass::After { ms } => Some(Duration::from_millis(ms)),
        _ => None,
    }
}

impl Agent {
    #[cfg(test)]
    #[must_use]
    pub(crate) fn with_keep_warm(mut self, limit: u32) -> Self {
        self.keep_warm.forced = Some(limit);
        self
    }

    fn warm_limit(&self) -> Option<u32> {
        if !self.provider().keeps_warm() {
            return None;
        }
        self.keep_warm
            .forced
            .or_else(|| self.agents.keep_warm())
            .filter(|limit| *limit > 0)
    }

    fn instant_at(&self, at: Timestamp) -> Instant {
        let ahead = at.duration_since(self.clock.now());
        Instant::now() + Duration::try_from(ahead).unwrap_or(Duration::ZERO)
    }

    fn spell(&self, anchor: Anchor, count: u32, tries: u32, due: Instant, late: Instant) -> Spell {
        let lanes = self.lanes.clone();
        let endpoint = self.endpoint_name().to_owned();
        Spell {
            anchor,
            count,
            tries,
            late,
            admission: Some(admission(
                lanes,
                endpoint,
                due,
                late,
                self.provider().generation(),
                self.endpoint_catalog.clone(),
            )),
        }
    }

    fn schedule(&mut self, anchor: Anchor, count: u32) {
        let due = self.instant_at(after(anchor.used_at, DUE_AFTER));
        let late = self.instant_at(after(anchor.used_at, LATE_AFTER));
        self.keep_warm.spell = Some(self.spell(anchor, count, 0, due, late));
    }

    pub(super) fn disarm_keep_warm(&mut self) {
        self.keep_warm.spell = None;
        if let Some(flight) = self.keep_warm.flight.as_mut() {
            flight.detached = true;
        }
    }

    pub(super) async fn start_keep_warm_turn(&mut self, events: &mpsc::Sender<AgentEvent>) {
        self.disarm_keep_warm();
        self.announce_warm(Some(0), None, events).await;
    }

    pub(super) async fn pause_keep_warm(&mut self, events: &mpsc::Sender<AgentEvent>) {
        self.disarm_keep_warm();
        self.announce_warm(None, Some(true), events).await;
    }

    pub(super) async fn arm_keep_warm(
        &mut self,
        outcome: &TurnOutcome,
        events: &mpsc::Sender<AgentEvent>,
    ) {
        self.disarm_keep_warm();
        if self.warm_limit().is_none() {
            self.announce_warm(None, Some(true), events).await;
            return;
        }
        let anchor = matches!(outcome, TurnOutcome::Completed { .. })
            .then(|| self.warm_anchor())
            .flatten();
        match anchor {
            Some(anchor) => {
                self.schedule(anchor, 0);
                self.announce_warm(Some(0), Some(false), events).await;
            }
            None => self.announce_warm(None, Some(true), events).await,
        }
    }

    pub(super) async fn restart_keep_warm(&mut self, events: &mpsc::Sender<AgentEvent>) {
        let Some(spell) = self.keep_warm.resumed.take() else {
            return;
        };
        let Some(limit) = self.warm_limit() else {
            return;
        };
        let anchor = self.warm_anchor();
        if !spell.ended
            && spell.count < limit
            && let Some(anchor) = anchor
        {
            self.schedule(anchor, spell.count);
        }
        self.announce_warm(
            Some(spell.count),
            Some(spell.ended || anchor.is_none()),
            events,
        )
        .await;
    }

    async fn announce_warm(
        &mut self,
        refreshes: Option<u32>,
        paused: Option<bool>,
        events: &mpsc::Sender<AgentEvent>,
    ) {
        if let Some(event) = self.agents.warm_changed(refreshes, paused) {
            emit(events, event).await;
        }
    }

    fn warm_anchor(&self) -> Option<Anchor> {
        self.bound_endpoint()?;
        if self.session.cache_ttl != CacheTtl::OneHour {
            return None;
        }
        let last = self.last.as_ref()?;
        if last.fell_back || last.endpoint != self.endpoint {
            return None;
        }
        let entry = self.epoch.anchor(self.endpoint, &self.session.model)?;
        let same = entry.turn == last.turn && entry.view_len == last.messages_len;
        if !same || entry.ttl != CacheTtl::OneHour || entry.prefix.is_none() {
            return None;
        }
        let view = self.view_messages();
        let reply = view.get(entry.view_len)?;
        if reply.role != Role::Assistant
            || crate::provider::anthropic::holds_fallback(std::slice::from_ref(reply))
        {
            return None;
        }
        Some(Anchor {
            turn: entry.turn,
            view_len: entry.view_len,
            used_at: entry.used_at(),
        })
    }

    fn still_anchored(&self, anchor: &Anchor) -> bool {
        let pending = self.settings.pending.is_some();
        let compacting = self.compaction.background.running() || self.compaction.background.ready();
        let sent = self
            .epoch
            .sent()
            .is_none_or(|sent| *sent == self.prefix_key());
        !pending
            && !compacting
            && sent
            && !self.rebuild_due()
            && self
                .warm_anchor()
                .is_some_and(|a| a.turn == anchor.turn && a.view_len == anchor.view_len)
    }

    pub(super) async fn warm_woke(&mut self, woke: Woke, events: &mpsc::Sender<AgentEvent>) {
        match woke {
            Woke::Due(pass) => self.warm_due(pass, events).await,
            Woke::Landed(landed) => self.warm_landed(*landed, events).await,
        }
    }

    async fn warm_due(&mut self, pass: Option<Pass>, events: &mpsc::Sender<AgentEvent>) {
        let Some(spell) = self.keep_warm.spell.take() else {
            return;
        };
        if self.warm_limit().is_none() {
            self.announce_warm(None, Some(true), events).await;
            return;
        }
        let now = self.clock.now();
        let late = now >= after(spell.anchor.used_at, LATE_AFTER);
        let Some(pass) = pass.filter(|_| !late) else {
            self.skipped(spell, LATE, now, events).await;
            return;
        };
        if !self.still_anchored(&spell.anchor) {
            self.skipped(spell, CHANGED, now, events).await;
            return;
        }
        let Some((replies, model, sound)) = self.refresh_stream(&spell.anchor) else {
            self.skipped(spell, CHANGED, now, events).await;
            return;
        };
        let sent = Arc::new(AtomicBool::new(false));
        let watch = self.watch();
        let flag = sent.clone();
        let handle = tokio::spawn(async move {
            super::side::drain(replies, None, pass, &watch, &model, flag).await
        });
        self.keep_warm.flight = Some(Flight {
            anchor: spell.anchor,
            sound,
            count: spell.count,
            tries: spell.tries,
            late: spell.late,
            started_at: now,
            detached: false,
            sent,
            handle,
        });
    }

    fn refresh_stream(&self, anchor: &Anchor) -> Option<(Replies, String, bool)> {
        let (mut session, features) = self.request_parts();
        session.max_tokens = 1;
        let view = self.view_messages();
        let messages = view.get(..anchor.view_len)?;
        let rendering = self.rendering(anchor.view_len);
        let (tools, server_tools) = self.wire(&self.sent, self.step_server_tools());
        let request = Request {
            session: &session,
            tools: &tools,
            deferred: super::tool_changes::deferred_for(&self.deferred, features),
            by_value: super::tool_changes::by_value_for(&self.deferred, features),
            server_tools,
            messages,
            previous_response_id: self
                .last
                .as_ref()
                .filter(|l| l.endpoint == self.endpoint)
                .and_then(|l| l.response_id.as_deref()),
            features,
            options: RequestOptions {
                refusal_fallback: false,
                ..self.request_options()
            },
            rendering: &rendering,
            compaction: None,
        };
        let digest = self.provider().cache_prefix(&request)?;
        let entry = self.epoch.anchor(self.endpoint, &self.session.model)?;
        let replies = (entry.prefix.as_deref() == Some(digest.as_str()))
            .then(|| self.provider().stream(&request))?;
        Some((
            replies,
            session.model,
            self.sent_sound(messages, &rendering),
        ))
    }

    async fn skipped(
        &mut self,
        spell: Spell,
        why: &str,
        now: Timestamp,
        events: &mpsc::Sender<AgentEvent>,
    ) {
        let record = LogEvent::CacheRefresh {
            turn: spell.anchor.turn,
            outcome: RefreshOutcome::Skipped,
            detail: Some(why.to_owned()),
            started_at: Some(crate::session::rfc3339(now)),
            count: spell.count.saturating_add(1),
            usage: None,
            cost_usd: None,
            list_usd: None,
            model: None,
            lower_bound: false,
        };
        self.log(events, record).await;
        self.announce_warm(None, Some(true), events).await;
    }

    async fn warm_landed(&mut self, landed: Landed, events: &mpsc::Sender<AgentEvent>) {
        let Some(flight) = self.keep_warm.flight.take() else {
            return;
        };
        self.record_side(events).await;
        self.refresh_plan_usage();
        if !flight.detached
            && landed.asm.usage().is_none()
            && let Some(error) = &landed.error
            && is_busy(&self.retry, error, false)
        {
            let tries = flight.tries.saturating_add(1);
            let due = Instant::now() + backoff(&self.retry, tries, retry_after(error));
            if due < flight.late {
                let spell = self.spell(flight.anchor, flight.count, tries, due, flight.late);
                self.keep_warm.spell = Some(spell);
                return;
            }
        }
        let judged = self.judge(&landed, flight.sound);
        let count = flight.count.saturating_add(1);
        if judged.outcome == RefreshOutcome::Read {
            self.epoch.refreshed(
                flight.anchor.turn,
                flight.anchor.view_len,
                flight.started_at,
            );
            self.refresh_marks();
        }
        self.record_refresh(
            (flight.anchor.turn, flight.started_at),
            count,
            judged,
            events,
        )
        .await;
        if flight.detached {
            return;
        }
        let limit = self.warm_limit();
        let again = limit.is_some_and(|limit| count < limit) && self.keep_warm.spell.is_none();
        let read = self.read_anchor(&flight);
        if again && let Some(read) = read {
            self.schedule(read, count);
        }
        let done = read.is_some() && limit.is_some_and(|limit| count >= limit);
        let paused = self.keep_warm.spell.is_none() && !done;
        self.announce_warm(Some(count), Some(paused), events).await;
    }

    fn read_anchor(&self, flight: &Flight) -> Option<Anchor> {
        let entry = self
            .epoch
            .entry_of(flight.anchor.turn, flight.anchor.view_len)?;
        (entry.refreshed_at == Some(flight.started_at)).then_some(Anchor {
            used_at: entry.used_at(),
            ..flight.anchor
        })
    }

    fn judge(&self, landed: &Landed, sound: bool) -> Judged {
        let (spend, model) = self.side_spend(landed, &self.session.model, sound);
        let other_stop = landed
            .stop
            .as_ref()
            .filter(|s| **s != StopReason::MaxTokens)
            .map(|s| s.as_str().to_owned());
        let error_kind = landed.error.as_ref().map(|e| kind_name(e.kind));
        let (outcome, detail) = match &spend.usage {
            Some(u) if read_enough(u) => (RefreshOutcome::Read, error_kind.or(other_stop)),
            Some(_) => (RefreshOutcome::Missed, error_kind.or(other_stop)),
            None => (RefreshOutcome::Failed, error_kind),
        };
        Judged {
            outcome,
            detail,
            spend,
            model: Some(model),
        }
    }

    async fn record_refresh(
        &mut self,
        (turn, started_at): (u64, Timestamp),
        count: u32,
        judged: Judged,
        events: &mpsc::Sender<AgentEvent>,
    ) {
        let spend = judged.spend;
        let cost = self.add_side(spend, judged.model.as_deref().unwrap_or_default());
        let record = LogEvent::CacheRefresh {
            turn,
            outcome: judged.outcome.clone(),
            detail: judged.detail.clone(),
            started_at: Some(crate::session::rfc3339(started_at)),
            count,
            usage: spend.usage,
            cost_usd: spend.usd,
            list_usd: spend.list_usd,
            model: judged.model,
            lower_bound: spend.lower_bound,
        };
        self.log(events, record).await;
        let event = AgentEvent::CacheRefreshed {
            outcome: judged.outcome,
            detail: judged.detail,
            usage: spend.usage,
            cost,
        };
        emit(events, event).await;
    }

    pub(super) async fn end_keep_warm(&mut self, events: &mpsc::Sender<AgentEvent>) {
        self.keep_warm.spell = None;
        let Some(finished) = self
            .keep_warm
            .flight
            .as_ref()
            .map(|f| f.handle.is_finished())
        else {
            return;
        };
        if finished {
            let landed = landed(&mut self.keep_warm.flight).await;
            if let Some(flight) = self.keep_warm.flight.as_mut() {
                flight.detached = true;
            }
            self.warm_landed(landed, events).await;
            return;
        }
        let Some(flight) = self.keep_warm.flight.take() else {
            return;
        };
        flight.handle.abort();
        let _ = flight.handle.await;
        let judged = Judged {
            outcome: RefreshOutcome::Stopped,
            detail: None,
            spend: Spend {
                lower_bound: flight.sent.load(Ordering::Relaxed),
                ..Spend::default()
            },
            model: None,
        };
        let count = flight.count.saturating_add(1);
        self.record_refresh(
            (flight.anchor.turn, flight.started_at),
            count,
            judged,
            events,
        )
        .await;
    }
}

#[cfg(test)]
pub(super) mod tests;
