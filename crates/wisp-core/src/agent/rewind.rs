use std::collections::{BTreeMap, BTreeSet};
use std::path::PathBuf;
use std::sync::Arc;

use jiff::Timestamp;
use tokio::sync::mpsc;
use wisp_plugin_api::hooks::{Event, RewindInfo};
use wisp_plugin_api::prompt::{Prior, RewoundJob};
use wisp_protocol::agents::shown_agent;
use wisp_protocol::log::{
    BlockPosition, Closed, LogEvent, LogRecord, LoggedHead, LoggedInput, LoggedRequest,
    RebuiltPoint, Slot, input_slots, logged_head,
};
use wisp_protocol::{
    AgentEvent, AgentPath, CacheEffect, CacheTtl, CompactionNote, ContentBlock, DiscardReason,
    Dropped, ImageData, ImageSource, InputId, Message, MissingImage, NOTHING_TO_REWIND, PointInfo,
    REWIND_SESSION_ENDED, REWIND_WHILE_COMPACTING, REWIND_WHILE_RUNNING, RebuildReason, RejectKind,
    RenderingForm, RequestId, ReturnedInput, RewindPoint, RewindQuote, RewindReason, Role,
    TodoItem, TurnOrigin, Warmth, WithdrawnBy, WorkspaceChange, not_a_point, rewound_away,
};

use super::compact::{Head, Ledger};
use super::epoch::{Entry, EpochAt, PrefixKey};
use super::inbox::Input;
use super::question::{Posted, PostedFold};
use super::resume::HeadTurns;
use super::todo_provenance::{TodoFold, TodoProvenance};
use super::{Agent, LastRequest, emit};
use crate::context::{blocks_tokens, ttl_span};
use crate::plugin::hooks::points::REWOUND as HOOK_REWOUND;
use crate::pricing;
use crate::resume::opening_of;

pub(super) const REWOUND: &str = "the conversation was rewound after the failure";

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct MarkInput {
    id: InputId,
    text: String,
    images: usize,
    noted: bool,
    shas: Vec<String>,
    from: Option<AgentPath>,
}

impl MarkInput {
    fn of(input: &Input) -> Self {
        Self {
            id: input.id.clone(),
            text: input.text.clone(),
            images: input.images.len(),
            noted: input.note.is_some(),
            shas: input.refs.iter().map(|r| r.sha256.clone()).collect(),
            from: input.from.as_ref().map(|l| l.from.agent.clone()),
        }
    }

    fn logged(input: LoggedInput) -> Self {
        Self {
            id: input.id,
            text: input.text,
            images: input.images.len(),
            noted: input.note.is_some(),
            shas: input.images.into_iter().map(|r| r.sha256).collect(),
            from: None,
        }
    }
}

#[derive(Debug, Clone)]
pub(super) struct PointMark {
    pub(super) turn: u64,
    seq: Option<u64>,
    at: String,
    label: String,
    inputs: Vec<MarkInput>,
    lead: usize,
    opening: Option<BlockPosition>,
    messages: usize,
    blocks: usize,
    pub(super) head: Option<usize>,
    head_epoch: u64,
    since_seq: u64,
    pub(super) sections: BTreeMap<String, Prior>,
    pub(super) cwd: Option<PathBuf>,
    pub(super) heard: BTreeSet<u32>,
    pub(super) numbered_from: u32,
    cuts: usize,
    pub(super) epoch: EpochAt,
    pub(super) baseline: Option<LastRequest>,
    pub(super) warm_until: Option<Timestamp>,
    key: PrefixKey,
    form: RenderingForm,
    pub(super) closed: Closed,
    failed_tail: Option<BTreeSet<String>>,
    ledger: Ledger,
    pub(super) todos: Vec<TodoItem>,
    pub(super) todos_provenance: Vec<TodoProvenance>,
    pub(super) posted: Option<Posted>,
    arrival: Option<u64>,
}

#[derive(Debug, Default)]
pub(super) struct Rewinds {
    pub(super) marks: Vec<PointMark>,
    pub(super) heads: Vec<Head>,
    pub(super) left: BTreeSet<u64>,
    news: Vec<RewoundJob>,
    restored: Option<Option<LastRequest>>,
    applied: u64,
}

impl Rewinds {
    pub(super) fn compacted(&mut self) {
        self.restored = None;
    }

    pub(super) fn applied(&self) -> u64 {
        self.applied
    }
}

pub(super) struct PointStart<'a> {
    pub(super) turn: u64,
    pub(super) inputs: &'a [Input],
    pub(super) lead: usize,
    pub(super) origin: Option<TurnOrigin>,
    pub(super) retry_of: Option<u64>,
    pub(super) seq: Option<u64>,
    pub(super) at: String,
    pub(super) failed_tail: Option<BTreeSet<String>>,
    pub(super) arrival: Option<u64>,
}

fn first_line(text: &str) -> String {
    text.lines()
        .find(|line| !line.trim().is_empty())
        .unwrap_or_default()
        .trim()
        .to_owned()
}

fn label(inputs: &[MarkInput], origin: Option<TurnOrigin>, retry_of: Option<u64>) -> String {
    if let Some(of) = retry_of.filter(|_| inputs.is_empty()) {
        return format!("\u{21bb} Retried turn {of}");
    }
    if origin == Some(TurnOrigin::ProcessExit) && inputs.is_empty() {
        return "\u{21bb} a background process ended".to_owned();
    }
    let Some(first) = inputs.iter().find(|i| i.from.is_none()) else {
        return match inputs.iter().find_map(|i| i.from.as_ref()) {
            Some(agent) => format!("\u{21bb} a message from {}", shown_agent(agent)),
            None => String::new(),
        };
    };
    match first_line(&first.text) {
        line if line.is_empty() && first.images > 0 => "[image]".to_owned(),
        line => line,
    }
}

fn ends_at(started_at: Option<Timestamp>, ttl: Option<CacheTtl>) -> Option<Timestamp> {
    started_at?.checked_add(ttl_span(ttl?)).ok()
}

fn cache_of(warm: &Warmth) -> CacheEffect {
    let reason = match warm {
        Warmth::Warm => return CacheEffect::Kept,
        Warmth::OtherEndpoint { .. } => RebuildReason::EndpointSwitch,
        Warmth::OtherModel { .. } => RebuildReason::ModelSwitch,
        Warmth::OtherEffort { .. } => RebuildReason::EffortChange,
        Warmth::MayHaveExpired | Warmth::PrefixChanged => RebuildReason::Rewind,
    };
    CacheEffect::Rebuilt { reason }
}

fn image_data(block: &ContentBlock) -> Option<ImageData> {
    match block {
        ContentBlock::Image {
            source: ImageSource::Base64 { data, .. },
        } => Some(ImageData {
            data: Arc::clone(data),
        }),
        _ => None,
    }
}

impl Agent {
    pub(super) fn mark_point(&mut self, opening: PointStart<'_>) {
        self.settle_last();
        let head = self.compaction.head.as_ref().map(|head| {
            let same = self.rewind.heads.last().is_some_and(|last| last == head);
            if !same {
                self.rewind.heads.push(head.clone());
            }
            self.rewind.heads.len() - 1
        });
        let inputs: Vec<MarkInput> = opening.inputs.iter().map(MarkInput::of).collect();
        let baseline = match &self.rewind.restored {
            Some(restored) => restored.clone(),
            None => self.last.clone(),
        }
        .filter(|_| !self.epoch.bytes_changed());
        let warm_until = baseline.as_ref().and_then(|b| ends_at(b.started_at, b.ttl));
        let mark = PointMark {
            turn: opening.turn,
            seq: opening.seq,
            at: opening.at,
            label: label(&inputs, opening.origin, opening.retry_of),
            inputs,
            lead: opening.lead,
            opening: None,
            messages: self.history.len(),
            blocks: self.history.last().map_or(0, |m| m.content.len()),
            head,
            head_epoch: self.epoch_number(),
            since_seq: self.compaction.since(),
            sections: self.sections.committed().clone(),
            cwd: self.tools.as_ref().and_then(|env| env.cwd.clone()),
            heard: self.processes.heard_ids(),
            numbered_from: self.processes.next_number(),
            cuts: self.search.cuts.len(),
            epoch: self.epoch.at(),
            baseline,
            warm_until,
            key: self.prefix_key(),
            form: self.search.form,
            closed: self.search.closed.clone(),
            failed_tail: opening.failed_tail,
            ledger: self.compaction.calls_now(),
            todos: self.todos.clone(),
            todos_provenance: self.todos_provenance.clone(),
            posted: self.posted.clone(),
            arrival: opening.arrival,
        };
        self.rewind.marks.push(mark);
    }

    pub(super) fn mark_opening(&mut self, turn: u64) {
        let len = self.history.len();
        let at = match self.history.last() {
            Some(last) if last.role == Role::User && !self.at_head() => BlockPosition {
                message: len - 1,
                block: last.content.len(),
            },
            _ => BlockPosition {
                message: len,
                block: 0,
            },
        };
        if let Some(mark) = self
            .rewind
            .marks
            .last_mut()
            .filter(|m| m.turn == turn && m.opening.is_none())
        {
            mark.opening = Some(at);
        }
        if self.track.as_ref().is_some_and(|t| t.notice.is_some()) {
            self.rewind.news.clear();
        }
    }

    fn unchanged(&self, mark: &PointMark) -> bool {
        mark.messages == self.history.len()
            && mark.blocks == self.history.last().map_or(0, |l| l.content.len())
    }

    fn settle_last(&mut self) {
        if self.rewind.marks.last().is_some_and(|m| self.unchanged(m)) {
            self.rewind.marks.pop();
        }
    }

    fn open_marks(&self) -> usize {
        let len = self.rewind.marks.len();
        match self.rewind.marks.last() {
            Some(last) if self.unchanged(last) => len - 1,
            _ => len,
        }
    }

    pub(super) fn refresh_marks(&mut self) {
        self.rewind.restored = None;
        let Some(last) = &self.last else {
            return;
        };
        let Some(until) = ends_at(self.last_used_at(), last.ttl) else {
            return;
        };
        let epoch = self.epoch_number();
        for mark in &mut self.rewind.marks {
            let same = mark.head_epoch == epoch
                && mark.baseline.as_ref().is_some_and(|b| {
                    b.endpoint == last.endpoint && b.asked == last.asked && b.effort == last.effort
                });
            if same && mark.warm_until.is_none_or(|t| t < until) {
                mark.warm_until = Some(until);
            }
        }
    }

    fn point_prefix_changed(&self, mark: &PointMark) -> bool {
        let cut_inside = self
            .search
            .cuts
            .get(mark.cuts..)
            .unwrap_or_default()
            .iter()
            .any(|cut| cut.from.message < mark.messages);
        cut_inside || mark.form != self.search.form || !mark.key.same_bytes(&self.prefix_key())
    }

    fn point_tokens(&self, mark: &PointMark) -> u64 {
        let head = mark
            .head
            .and_then(|i| self.rewind.heads.get(i))
            .map_or(0, |h| {
                blocks_tokens(&h.message.content).saturating_add(blocks_tokens(&h.additions))
            });
        let from = mark
            .head
            .and_then(|i| self.rewind.heads.get(i))
            .map_or(0, |h| h.cut);
        let counted = self
            .history
            .get(from..mark.messages)
            .unwrap_or_default()
            .iter()
            .map(|m| blocks_tokens(&m.content))
            .fold(head, u64::saturating_add);
        self.evicted_tokens(from..mark.messages, counted)
    }

    pub(super) fn warmth(&self, mark: &PointMark) -> (Warmth, u64) {
        let Some(base) = &mark.baseline else {
            return (Warmth::PrefixChanged, self.point_tokens(mark));
        };
        let (endpoint, model) = self.next_endpoint_model();
        let from = (base.endpoint, base.asked.as_str(), base.effort);
        let effect = self.cache_effect_between(from, (endpoint, &model, self.next_effort()));
        let warm = match effect {
            CacheEffect::Rebuilt {
                reason: RebuildReason::EndpointSwitch,
            } => Warmth::OtherEndpoint {
                name: self.connection_name(base.endpoint).to_owned(),
            },
            CacheEffect::Rebuilt {
                reason: RebuildReason::ModelSwitch,
            } => Warmth::OtherModel {
                model: base.asked.clone(),
            },
            CacheEffect::Rebuilt { .. } => Warmth::OtherEffort {
                effort: base.effort,
            },
            CacheEffect::KeptIfPassedOn => Warmth::MayHaveExpired,
            CacheEffect::Kept if self.point_prefix_changed(mark) => Warmth::PrefixChanged,
            CacheEffect::Kept => match mark.warm_until {
                Some(until) if self.clock.now() < until => Warmth::Warm,
                _ => Warmth::MayHaveExpired,
            },
        };
        (warm, base.prompt_tokens)
    }

    fn quote(&self, mark: &PointMark) -> RewindQuote {
        let (warm, prefix_tokens) = self.warmth(mark);
        let (endpoint, model) = self.next_endpoint_model();
        let prices = self.priced_rates(endpoint, &model);
        let (read_micros, rewrite_micros) = match prices {
            Some(info) => {
                let (read, rewrite) =
                    pricing::prefix_quote(&info, prefix_tokens, self.session.cache_ttl);
                (Some(pricing::micros(read)), Some(pricing::micros(rewrite)))
            }
            None => (None, None),
        };
        RewindQuote {
            prefix_tokens,
            warm,
            read_micros,
            rewrite_micros,
        }
    }

    fn compaction_note(&self, index: usize) -> Option<CompactionNote> {
        let mark = self.rewind.marks.get(index)?;
        if mark.head_epoch == self.epoch_number() {
            return None;
        }
        let turn = self
            .rewind
            .marks
            .get(index + 1..)
            .unwrap_or_default()
            .iter()
            .find(|m| m.head_epoch != mark.head_epoch)
            .map_or(self.turns.saturating_add(1), |m| m.turn);
        let compact_at = self.window_and_point().1;
        let tokens = mark
            .baseline
            .as_ref()
            .map_or_else(|| self.point_tokens(mark), |b| b.prompt_tokens);
        Some(CompactionNote {
            turn,
            compacts_first: compact_at.is_some_and(|at| tokens >= at),
        })
    }

    fn point_info(&self, index: usize) -> Option<PointInfo> {
        let mark = self.rewind.marks.get(index)?;
        Some(PointInfo {
            point: RewindPoint::Turn { turn: mark.turn },
            label: mark.label.clone(),
            at: mark.at.clone(),
            inputs_preview: mark.inputs.iter().map(|i| first_line(&i.text)).collect(),
            quote: self.quote(mark),
            compaction: self.compaction_note(index),
            processes: self.processes.running_from(mark.numbered_from),
        })
    }

    pub(super) async fn rewind_points(
        &mut self,
        request: RequestId,
        events: &mpsc::Sender<AgentEvent>,
    ) {
        let points = (0..self.open_marks())
            .filter_map(|i| self.point_info(i))
            .collect();
        let event = AgentEvent::RewindPoints {
            answers: request,
            points,
        };
        emit(events, event).await;
    }

    pub(super) async fn refuse_rewind(
        &mut self,
        request: RequestId,
        kind: RejectKind,
        reason: impl Into<String>,
        events: &mpsc::Sender<AgentEvent>,
    ) {
        let event = AgentEvent::Rejected {
            reason: reason.into(),
            id: None,
            kind: Some(kind),
            request: Some(request),
        };
        self.emit(events, event).await;
    }

    pub(super) async fn rewind_command(
        &mut self,
        request: RequestId,
        to: RewindPoint,
        closing: bool,
        events: &mpsc::Sender<AgentEvent>,
    ) {
        if closing {
            self.refuse_rewind(
                request,
                RejectKind::NotRewindable,
                REWIND_SESSION_ENDED,
                events,
            )
            .await;
            return;
        }
        if self.track.is_some() {
            self.refuse_rewind(
                request,
                RejectKind::TurnRunning,
                REWIND_WHILE_RUNNING,
                events,
            )
            .await;
            return;
        }
        if self.compacting() {
            self.refuse_rewind(
                request,
                RejectKind::Compacting,
                REWIND_WHILE_COMPACTING,
                events,
            )
            .await;
            return;
        }
        let turn = to.turn();
        let open = self.open_marks();
        let found = self.rewind.marks[..open]
            .iter()
            .position(|m| m.turn == turn);
        let Some(index) = found else {
            let reason = if self.rewind.left.contains(&turn) {
                rewound_away(turn)
            } else if open == 0 {
                NOTHING_TO_REWIND.to_owned()
            } else {
                not_a_point(turn)
            };
            self.refuse_rewind(request, RejectKind::NotRewindable, reason, events)
                .await;
            return;
        };
        self.rewind_to(request, index, events).await;
    }

    pub(super) fn returned(&self, mark: &PointMark) -> Vec<ReturnedInput> {
        let Some(at) = mark.opening else {
            return Vec::new();
        };
        let blocks = self
            .history
            .get(at.message)
            .map_or(&[][..], |m| m.content.as_slice());
        let mut next = at.block + mark.lead;
        mark.inputs
            .iter()
            .filter_map(|input| {
                let mut images = Vec::new();
                let mut missing = Vec::new();
                let mut shas = (1..).zip(&input.shas);
                for slot in input_slots(input.images, input.noted) {
                    if slot == Slot::Image
                        && let Some((ordinal, sha)) = shas.next()
                    {
                        match blocks.get(next).and_then(image_data) {
                            Some(image) => images.push(image),
                            None => missing.push(MissingImage {
                                ordinal,
                                sha256: sha.clone(),
                            }),
                        }
                    }
                    next += 1;
                }
                input.from.is_none().then(|| ReturnedInput {
                    id: input.id.clone(),
                    text: input.text.clone(),
                    images,
                    missing,
                })
            })
            .collect()
    }

    async fn withdraw_all(&mut self, events: &mpsc::Sender<AgentEvent>) -> Vec<ReturnedInput> {
        let mut ids = self.inbox.all_ids();
        let back = ids
            .iter()
            .filter_map(|id| self.inbox.remove(id))
            .map(|input| ReturnedInput {
                id: input.id,
                text: input.text,
                images: input.images.iter().filter_map(image_data).collect(),
                missing: Vec::new(),
            })
            .collect();
        ids.extend(self.inbox.take_question_mail());
        self.withdrawn(ids, WithdrawnBy::Rewound, events).await;
        back
    }

    async fn rewind_to(
        &mut self,
        request: RequestId,
        index: usize,
        events: &mpsc::Sender<AgentEvent>,
    ) {
        let Some(mark) = self.rewind.marks.get(index).cloned() else {
            return;
        };
        let cut = mark
            .head
            .and_then(|i| self.rewind.heads.get(i))
            .map_or(0, |h| h.cut);
        let from = mark
            .opening
            .map_or(cut, |at| at.message.min(cut))
            .min(mark.messages.saturating_sub(1));
        if let Err(reason) = self.restore_media(from) {
            let reason = format!("Can't rewind: {reason}.");
            self.refuse_rewind(request, RejectKind::NotRewindable, reason, events)
                .await;
            return;
        }
        self.pause_keep_warm(events).await;
        let quote = self.quote(&mark);
        let mut returned = self.returned(&mark);
        returned.extend(self.withdraw_all(events).await);
        self.discard_background(DiscardReason::ViewChanged, None, events)
            .await;
        self.lapse_offer(REWOUND);
        let dropped = Dropped {
            turns: (mark.turn..=self.turns).collect(),
            messages: self.history.len().saturating_sub(mark.messages),
        };
        let cache = cache_of(&quote.warm);
        let todos_before = self.todos.clone();
        self.restore_point(&mark, cache == CacheEffect::Kept);
        self.rewind.applied += 1;
        self.withdraw_dropped_posted(mark.posted.as_ref(), events)
            .await;
        if let Some(seq) = mark.seq {
            let record = LogEvent::Rewound {
                to: seq.saturating_sub(1),
                point: RewindPoint::Turn { turn: mark.turn },
                reason: RewindReason::User,
                messages: mark.messages,
                blocks: mark.blocks,
                dropped: dropped.clone(),
                cache: cache.clone(),
            };
            self.log(events, record).await;
            self.sync_log(events).await;
        }
        self.rewind.left.extend(dropped.turns.iter().copied());
        self.rewind.marks.truncate(index);
        let event = AgentEvent::Rewound {
            answers: Some(request),
            point: RewindPoint::Turn { turn: mark.turn },
            reason: RewindReason::User,
            dropped,
            returned,
            cache,
        };
        emit(events, event).await;
        self.reopen_posted(mark.posted, events).await;
        self.scrub_live_todos();
        if self.todos != todos_before {
            self.emit_todos_updated(events, None).await;
        }
        self.refresh_strategy(super::hooks::Mode::Turn, None, events)
            .await;
        let changed = self.context_changed();
        emit(events, changed).await;
        self.cancel_commands(mark.arrival, events).await;
        let info = RewindInfo {
            turn: mark.turn,
            epoch: self.epoch_number(),
        };
        self.defer(HOOK_REWOUND, Event::Rewound(info));
    }

    pub(super) fn restore_point(&mut self, mark: &PointMark, warm: bool) {
        self.history.truncate(mark.messages);
        if let Some(last) = self.history.last_mut() {
            last.content.truncate(mark.blocks);
        }
        self.compaction.rewound_to(
            mark.turn,
            (
                mark.head.and_then(|i| self.rewind.heads.get(i)).cloned(),
                mark.since_seq,
            ),
            mark.ledger.clone(),
        );
        self.rewind
            .heads
            .truncate(mark.head.map_or(0, |i| i.saturating_add(1)));
        self.sections.restore(mark.sections.clone());
        self.restore_todos(&mark.todos, &mark.todos_provenance);
        if let Some(env) = &mut self.tools {
            env.cwd.clone_from(&mark.cwd);
        }
        let tail = (mark.closed.clone(), mark.failed_tail.clone());
        self.search.rewound_to(mark.messages, mark.cuts, tail);
        self.rewound_tools(mark.messages);
        self.epoch.restore_at(mark.epoch.clone());
        self.last = mark.baseline.clone().filter(|_| warm);
        self.search.drops_covered = false;
        self.rewind.restored = Some(mark.baseline.clone());
        self.accepted = self.accepted.min(super::withdraw::end_of(&self.history));
        self.sync_history();
        self.evict_media();
        let news = self.processes.rewind(&mark.heard, mark.numbered_from);
        for job in news {
            if self.rewind.news.iter().all(|known| known.id != job.id) {
                self.rewind.news.push(job);
            }
        }
    }

    pub(super) fn has_todos(&self) -> bool {
        self.toolbox.provider_of(crate::tool::TODOS).is_some()
    }

    pub(super) fn restore_todos(&mut self, items: &[TodoItem], provenance: &[TodoProvenance]) {
        if self.has_todos() {
            items.clone_into(&mut self.todos);
            let home = self.secret_check_home();
            super::scrub_todos(&mut self.todos, home.as_deref(), &crate::session::held());
            provenance.clone_into(&mut self.todos_provenance);
        } else {
            self.todos.clear();
            self.todos_provenance.clear();
        }
    }

    pub(super) fn rewound_news(&self) -> Vec<RewoundJob> {
        let running = self.processes.running_from(0);
        self.rewind
            .news
            .iter()
            .filter(|job| running.contains(&job.id))
            .cloned()
            .collect()
    }

    pub(super) fn resume_marks(
        &mut self,
        records: &[LogRecord],
        masked: &[bool],
        (points, restored): (&[RebuiltPoint], Option<Option<LoggedRequest>>),
        (turns, mut ledgers): (&[u64], BTreeMap<u64, Ledger>),
    ) {
        let mut heads: BTreeMap<usize, usize> = BTreeMap::new();
        let mut marks = Vec::new();
        let mut left = BTreeSet::new();
        for (record, _) in records.iter().zip(masked).filter(|(_, m)| !**m) {
            if let LogEvent::Rewound { dropped, .. } = &record.event {
                left.extend(dropped.turns.iter().copied());
            }
        }
        let start = self
            .sections
            .resumed_start
            .clone()
            .unwrap_or_else(|| super::sections::logged_start(records));
        let moves = super::sections::moves(
            records
                .iter()
                .enumerate()
                .zip(masked)
                .filter(|(_, m)| !**m)
                .map(|(r, _)| r),
        );
        let mut firsts: BTreeMap<u64, usize> = BTreeMap::new();
        for (at, (message, turn)) in self.history.iter().zip(turns).enumerate() {
            if message.role == Role::User {
                firsts.entry(*turn).or_insert(at);
            }
        }
        let key = self.prefix_key();
        let (mut befores, spans) = folded(records, masked, points);
        for point in points {
            let (Some(record), Some(before)) =
                (records.get(point.source), befores.remove(&point.source))
            else {
                continue;
            };
            let head = point.head.and_then(|source| {
                if let Some(index) = heads.get(&source) {
                    return Some(*index);
                }
                let span = spans.get(&source).copied().flatten();
                let head = self.resumed_head(records, (source, point.join), span)?;
                self.rewind.heads.push(head);
                let index = self.rewind.heads.len() - 1;
                heads.insert(source, index);
                Some(index)
            });
            let kept = |turn| firsts.get(&turn).is_some_and(|at| *at < point.messages);
            let sections = self.committed_at(&moves.window(point.head, point.source), kept, &start);
            let at = (before, sections, key.clone());
            if let Some(mut mark) = self.resumed_mark(records, (point, record), head, at) {
                mark.ledger = ledgers.remove(&record.seq).unwrap_or_default();
                marks.push(mark);
            }
        }
        let senders: BTreeMap<&InputId, &AgentPath> = records
            .iter()
            .filter_map(|r| match &r.event {
                LogEvent::InputAccepted {
                    id,
                    from: Some(from),
                    ..
                } => Some((id, &from.agent)),
                _ => None,
            })
            .collect();
        for input in marks.iter_mut().flat_map(|m| m.inputs.iter_mut()) {
            if let Some(agent) = senders.get(&input.id) {
                input.from = Some((*agent).clone());
            }
        }
        self.rewind.marks = marks;
        self.rewind.left = left;
        self.rewind.restored = restored.map(|b| b.and_then(|b| self.logged_last(&b, None)));
    }

    fn resumed_head(
        &self,
        records: &[LogRecord],
        (source, join): (usize, Option<usize>),
        span: Option<(u64, u64)>,
    ) -> Option<Head> {
        let record = records.get(source)?;
        let logged = logged_head(&record.event, &self.history)?;
        Head::from_logged(LoggedHead { join, ..logged }, &record.event, span)
    }

    fn resumed_mark(
        &self,
        records: &[LogRecord],
        (point, record): (&RebuiltPoint, &LogRecord),
        head: Option<usize>,
        (before, sections, key): (Before, BTreeMap<String, Prior>, PrefixKey),
    ) -> Option<PointMark> {
        let LogEvent::TurnStarted {
            turn,
            origin,
            retry_of,
            lead,
            ..
        } = &record.event
        else {
            return None;
        };
        let inputs: Vec<MarkInput> = opening_of(&record.event)
            .into_iter()
            .map(MarkInput::logged)
            .collect();
        let lead = lead.leading();
        let baseline = point
            .baseline
            .as_ref()
            .and_then(|b| self.logged_last(b, None));
        let warm_until = point
            .refreshed
            .iter()
            .filter_map(|(at, ttl)| ends_at(at.parse().ok(), Some(*ttl)))
            .max();
        let anchor = point
            .anchor
            .as_ref()
            .and_then(|a| self.logged_entry(records, a));
        let closed = before.closed;
        let failed_tail = retry_of
            .filter(|_| self.tail_live_at(point, &closed))
            .map(|_| BTreeSet::new());
        Some(PointMark {
            turn: *turn,
            seq: Some(record.seq),
            at: record.at.clone(),
            label: label(&inputs, *origin, *retry_of),
            inputs,
            lead,
            opening: point.opening,
            messages: point.messages,
            blocks: point.blocks,
            head,
            head_epoch: point.epoch,
            since_seq: point
                .head
                .and_then(|i| records.get(i))
                .map_or(0, |r| r.seq.saturating_add(1)),
            sections,
            cwd: before.cwd,
            heard: before.heard,
            numbered_from: before.top.map_or(1, |id| id.saturating_add(1)),
            cuts: point.cuts,
            epoch: EpochAt::resumed(anchor),
            baseline,
            warm_until,
            key,
            form: self.search.form,
            closed,
            failed_tail,
            ledger: Ledger::new(),
            todos: before.todos,
            todos_provenance: before.todos_provenance,
            posted: before.posted.posted(),
            arrival: None,
        })
    }

    fn tail_live_at(&self, point: &RebuiltPoint, closed: &Closed) -> bool {
        let mut kept: Vec<Message> = self
            .history
            .get(..point.messages)
            .unwrap_or_default()
            .to_vec();
        if let Some(last) = kept.last_mut() {
            last.content.truncate(point.blocks);
        }
        super::search::paused_tail(&kept, &self.search.paused, closed)
            || !super::search::open_calls(self.provider().as_ref(), &kept, closed).is_empty()
    }

    pub(super) fn logged_last(
        &self,
        logged: &LoggedRequest,
        messages_len: Option<usize>,
    ) -> Option<LastRequest> {
        let messages_len = messages_len.or(logged.view_len)?;
        Some(LastRequest {
            response_id: logged.response_id.clone(),
            endpoint: self.endpoint_index(logged.endpoint_name.as_deref()),
            model: logged
                .served
                .clone()
                .unwrap_or_else(|| logged.model.clone()),
            asked: logged.model.clone(),
            effort: logged.effort.unwrap_or(self.start_effort()),
            prompt_tokens: logged.prompt_tokens,
            turn: logged.turn,
            step: logged.step,
            used: logged.used,
            output_tokens: logged.output_tokens,
            cache_read: logged.cache_read,
            started_at: logged.started_at.parse().ok(),
            ttl: Some(logged.ttl),
            messages_len,
            first_pass: logged.first_pass,
            print: None,
            fell_back: false,
        })
    }

    pub(super) fn logged_entry(
        &self,
        records: &[LogRecord],
        logged: &LoggedRequest,
    ) -> Option<Entry> {
        let endpoint = self
            .connections
            .iter()
            .position(|c| Some(c.name.as_str()) == logged.endpoint_name.as_deref())?;
        Some(Entry {
            turn: logged.turn,
            seq: logged.reply.and_then(|r| records.get(r)).map(|r| r.seq),
            view_len: logged.view_len?,
            prompt_tokens: logged.prompt_tokens,
            used: logged.used,
            output_tokens: logged.output_tokens,
            started_at: logged.started_at.parse().ok()?,
            ttl: logged.ttl,
            model: logged.model.clone(),
            endpoint,
            prefix: logged.prefix.clone(),
            refreshed_at: None,
        })
    }
}

#[derive(Clone, Default)]
struct Before {
    closed: Closed,
    cwd: Option<PathBuf>,
    heard: BTreeSet<u32>,
    top: Option<u32>,
    todos: Vec<TodoItem>,
    todos_provenance: Vec<TodoProvenance>,
    posted: PostedFold,
}

impl Before {
    fn see(&mut self, event: &LogEvent) {
        self.posted.see(event);
        match event {
            LogEvent::ToolCallFinished {
                state: Some(WorkspaceChange::Cwd { path }),
                ..
            } => self.cwd = Some(PathBuf::from(path)),
            LogEvent::TailClosed { through, calls, .. } => {
                self.closed.calls.extend(calls.iter().cloned());
                self.closed.tails.insert(*through);
            }
            LogEvent::ProcessHeard { ids, .. } => self.heard.extend(ids.iter().copied()),
            _ => {}
        }
    }
}

type Spans = BTreeMap<usize, Option<(u64, u64)>>;

fn folded(
    records: &[LogRecord],
    masked: &[bool],
    points: &[RebuiltPoint],
) -> (BTreeMap<usize, Before>, Spans) {
    let sources: BTreeSet<usize> = points.iter().map(|p| p.source).collect();
    let mut before = Before::default();
    let mut todos = TodoFold::default();
    let mut head = HeadTurns::default();
    let (mut at_points, mut spans) = (BTreeMap::new(), BTreeMap::new());
    for (at, (record, masked)) in records.iter().zip(masked).enumerate() {
        if sources.contains(&at) {
            at_points.insert(
                at,
                Before {
                    todos: todos.todos.clone(),
                    todos_provenance: todos.provenance.clone(),
                    ..before.clone()
                },
            );
        }
        if let LogEvent::ProcessStarted { id, .. }
        | LogEvent::ToolCallFinished {
            process: Some(id), ..
        } = &record.event
        {
            before.top = before.top.max(Some(*id));
        }
        if !masked {
            before.see(&record.event);
            todos.see(&record.event);
            head.see(&record.event);
        }
        if matches!(record.event, LogEvent::ContextCompacted { .. }) {
            spans.insert(at, head.span);
        }
    }
    (at_points, spans)
}

#[cfg(test)]
mod tests;
