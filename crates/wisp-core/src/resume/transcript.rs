use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;

use serde_json::Value;
use wisp_protocol::log::{LogEvent, LogRecord, LoggedBlock, LoggedInput, LoggedMessage};
use wisp_protocol::settings::{LoggedChange, Owner};
use wisp_protocol::{
    AgentEvent, BlockKind, CacheEffect, ContentBlock, DeliveryMode, ErrorKind, Feature, InputId,
    RebuildReason, RetryOffer, Role, TextSpan, TodoItem, TurnOutcome,
};

use super::{FeatureFact, feature_state, opening_of};
use crate::agent::state_event;
use crate::provider::{forms, quiet_end};
use crate::secrets::scan::{os_detection_home, scrub_checklist_spans, scrub_todos};
use crate::session::mirror::live_event;
use crate::session::{
    REDACTED, Redactions, compaction_terminal_message, held, hook_compaction_error_kind,
};
use crate::tool::Toolbox;

pub(crate) fn replayed(event: &LogEvent) -> Option<(u64, AgentEvent)> {
    match event {
        LogEvent::ToolCallRequested { turn, .. }
        | LogEvent::ToolCallStarted { turn, .. }
        | LogEvent::QuestionAsked { turn, .. }
        | LogEvent::QuestionClosed { turn, .. }
        | LogEvent::ToolCallFinished { turn, .. } => Some((*turn, live_event(event)?)),
        _ => None,
    }
}

pub(crate) type LatestBriefs = BTreeMap<InputId, (String, Vec<TextSpan>)>;

pub(crate) fn latest_spanned_mail(records: &[LogRecord]) -> LatestBriefs {
    let mut latest = LatestBriefs::new();
    for record in records {
        let inputs = match &record.event {
            LogEvent::TurnStarted { inputs, .. }
            | LogEvent::InputRevised { inputs, .. }
            | LogEvent::InputCommitted { inputs, .. } => inputs,
            _ => continue,
        };
        for input in inputs
            .iter()
            .filter(|input| !input.checklist_spans.is_empty())
        {
            latest.insert(
                input.id.clone(),
                (input.text.clone(), input.checklist_spans.clone()),
            );
        }
    }
    latest
}

pub(crate) fn apply_latest_spanned_mail(event: &mut AgentEvent, latest: &LatestBriefs) {
    if let AgentEvent::InputAccepted {
        id,
        text,
        checklist_spans,
        ..
    } = event
        && !checklist_spans.is_empty()
        && let Some((latest_text, latest_spans)) = latest.get(id)
    {
        text.clone_from(latest_text);
        checklist_spans.clone_from(latest_spans);
    }
}

#[derive(Default)]
struct Inputs {
    known: BTreeMap<InputId, (DeliveryMode, LoggedInput)>,
    mail: BTreeMap<InputId, wisp_protocol::MailFrom>,
    delivered: BTreeSet<InputId>,
    last_attempt: BTreeMap<(u64, u32), u32>,
}

impl Inputs {
    fn read(records: &[LogRecord]) -> Inputs {
        let mut inputs = Inputs::default();
        for record in records {
            match &record.event {
                event @ LogEvent::TurnStarted { .. } => {
                    for input in opening_of(event) {
                        inputs.delivered.insert(input.id.clone());
                        inputs
                            .known
                            .entry(input.id.clone())
                            .or_insert((DeliveryMode::Queue, input));
                    }
                }
                LogEvent::InputAccepted {
                    id,
                    mode,
                    text,
                    images,
                    from,
                    checklist_spans,
                    ..
                } => {
                    let input = LoggedInput {
                        id: id.clone(),
                        text: text.clone(),
                        images: images.clone(),
                        note: None,
                        checklist_spans: checklist_spans.clone(),
                    };
                    inputs.known.insert(id.clone(), (*mode, input));
                    if let Some(from) = from {
                        inputs.mail.insert(id.clone(), from.clone());
                    }
                }
                LogEvent::InputDelivered { ids, .. } => {
                    inputs.delivered.extend(ids.iter().cloned());
                }
                LogEvent::InputCommitted { inputs: list, .. } => {
                    for input in list
                        .iter()
                        .filter(|i| !crate::agent::retry::is_call_note_id(&i.id))
                    {
                        inputs.delivered.insert(input.id.clone());
                        inputs
                            .known
                            .entry(input.id.clone())
                            .or_insert((DeliveryMode::Steer, input.clone()));
                    }
                }
                LogEvent::SearchStarted {
                    turn,
                    step,
                    attempt,
                    ..
                }
                | LogEvent::Citations {
                    turn,
                    step,
                    attempt,
                    ..
                } => inputs.attempt(*turn, *step, *attempt),
                LogEvent::SearchEnded {
                    turn: Some(turn),
                    step,
                    attempt,
                    ..
                } => inputs.attempt(*turn, *step, *attempt),
                _ => {}
            }
        }
        inputs
    }

    fn attempt(&mut self, turn: u64, step: u32, attempt: u32) {
        let last = self.last_attempt.entry((turn, step)).or_insert(attempt);
        *last = (*last).max(attempt);
    }

    fn last(&self, turn: u64, step: u32, attempt: u32) -> bool {
        self.last_attempt
            .get(&(turn, step))
            .is_none_or(|last| *last == attempt)
    }
}

struct Replay<'a> {
    inputs: Inputs,
    latest_mail: &'a LatestBriefs,
    offer: Option<(u64, RetryOffer)>,
    out: Vec<AgentEvent>,
    turn: u64,
    step: u32,
    replied: bool,
    accepted: BTreeSet<InputId>,
    delivered: BTreeSet<InputId>,
    step_end: Option<AgentEvent>,
    citations: Vec<AgentEvent>,
    model: String,
    turn_model: Option<String>,
    records: &'a [LogRecord],
    toolbox: &'a Toolbox,
    todos: Vec<TodoItem>,
    home: Option<&'a Path>,
    redactions: Redactions,
    compaction_failures: BTreeMap<u64, Vec<(ErrorKind, &'a str)>>,
}

impl<'a> Replay<'a> {
    fn flush_end(&mut self) {
        self.out.extend(self.step_end.take());
    }

    fn enter_step(&mut self, step: u32) {
        if step <= self.step {
            return;
        }
        self.flush_end();
        self.out.push(AgentEvent::StepStarted { step });
        self.step = step;
        self.replied = false;
    }

    fn accept(&mut self, id: &InputId) {
        if !self.accepted.insert(id.clone()) {
            return;
        }
        if let Some((mode, input)) = self.inputs.known.get(id) {
            let (mut text, mut checklist_spans) = self
                .latest_mail
                .get(id)
                .filter(|_| !input.checklist_spans.is_empty())
                .cloned()
                .unwrap_or_else(|| (input.text.clone(), input.checklist_spans.clone()));
            scrub_checklist_spans(&mut text, &mut checklist_spans, self.home, &self.redactions);
            self.out.push(AgentEvent::InputAccepted {
                opening: false,
                id: input.id.clone(),
                mode: *mode,
                text,
                checklist_spans,
                images: input.images.clone(),
                replaces: None,
                held: false,
                front: false,
                from: self.inputs.mail.get(id).cloned(),
            });
        }
    }

    fn deliver(&mut self, ids: &[InputId], step: u32) {
        let fresh: Vec<InputId> = ids
            .iter()
            .filter(|id| !self.delivered.contains(*id))
            .cloned()
            .collect();
        if fresh.is_empty() {
            return;
        }
        for id in &fresh {
            self.accept(id);
            self.delivered.insert(id.clone());
        }
        self.out
            .push(AgentEvent::InputDelivered { ids: fresh, step });
    }

    fn reply(&mut self, message: &LoggedMessage) {
        if self.step == 0 || self.replied {
            self.enter_step(self.step.saturating_add(1));
        }
        self.replied = true;
        if quiet_end(message.content.iter().filter_map(LoggedBlock::as_inline)) {
            return;
        }
        let indices = forms::stream_indices(&message.content);
        let mut envelope = None;
        for (position, block) in message.content.iter().enumerate() {
            let LoggedBlock::Inline(block) = block else {
                continue;
            };
            let index = indices.as_ref().and_then(|i| i.get(position).copied());
            let index = index.unwrap_or(position);
            let beside = envelope.replace(block);
            let (kind, body) = match block {
                ContentBlock::Text { text } => (BlockKind::Text, Some(text.clone())),
                ContentBlock::Thinking { thinking, .. } => {
                    (BlockKind::Thinking, Some(thinking.clone()))
                }
                ContentBlock::RedactedThinking { .. } => (BlockKind::RedactedThinking, None),
                ContentBlock::Opaque { raw, .. }
                    if raw.get("type").and_then(Value::as_str) == Some("text") =>
                {
                    let text = raw.get("text").and_then(Value::as_str).unwrap_or_default();
                    (BlockKind::Text, Some(text.to_owned()))
                }
                _ => continue,
            };
            let deltas = match (body, &kind) {
                (Some(text), BlockKind::Thinking) => {
                    vec![AgentEvent::ThinkingDelta { index, text }]
                }
                (Some(text), _) => self.cited_spans(index, text, beside),
                (None, _) => Vec::new(),
            };
            self.out.push(AgentEvent::BlockStarted { index, kind });
            self.out.extend(deltas);
            self.out.push(AgentEvent::BlockStopped { index });
        }
        self.out.append(&mut self.citations);
    }

    fn cited_spans(
        &mut self,
        index: usize,
        text: String,
        envelope: Option<&ContentBlock>,
    ) -> Vec<AgentEvent> {
        let delta = |text: &str| AgentEvent::TextDelta {
            index,
            text: text.to_owned(),
        };
        let mut out = Vec::new();
        let mut from = 0;
        for (end, url) in envelope.map(forms::cited_ends).unwrap_or_default() {
            let Some(piece) = text.get(from..end) else {
                continue;
            };
            let Some(cited) = self.take_source(index, &url) else {
                continue;
            };
            out.extend((!piece.is_empty()).then(|| delta(piece)));
            out.push(cited);
            from = end;
        }
        let rest = text.get(from..).unwrap_or_default();
        out.extend((from == 0 || !rest.is_empty()).then(|| delta(rest)));
        out
    }

    fn take_source(&mut self, at: usize, url: &str) -> Option<AgentEvent> {
        let (step, attempt, source) = self.citations.iter_mut().find_map(|event| match event {
            AgentEvent::Citations {
                index,
                sources,
                step,
                attempt,
            } if *index == at => {
                let found = sources.iter().position(|s| s.url == url)?;
                Some((*step, *attempt, sources.remove(found)))
            }
            _ => None,
        })?;
        self.citations
            .retain(|e| !matches!(e, AgentEvent::Citations { sources, .. } if sources.is_empty()));
        let sources = vec![source];
        Some(AgentEvent::Citations {
            step,
            attempt,
            index: at,
            sources,
        })
    }

    fn ended(&mut self, turn: u64, mut outcome: TurnOutcome, stats: wisp_protocol::TurnStats) {
        self.flush_end();
        self.citations.clear();
        let compaction_failures = self.compaction_failures.remove(&turn);
        if let TurnOutcome::Error {
            kind,
            offer,
            message,
            ..
        } = &mut outcome
        {
            if message != REDACTED
                && compaction_failures.is_some_and(|failures| {
                    failures.into_iter().any(|(error, earlier)| {
                        error == *kind && same_compaction_reason(earlier, message)
                    })
                })
            {
                *message = compaction_terminal_message(std::mem::take(message));
            }
            *offer = self
                .offer
                .as_ref()
                .filter(|(on, _)| *on == turn)
                .map(|(_, offer)| offer.clone());
        }
        self.out.push(AgentEvent::TurnEnded { outcome, stats });
    }

    fn rebuild_todos(&mut self, through: usize) {
        let records = self.records.get(..=through).unwrap_or(self.records);
        self.show_todos(None, wisp_protocol::todos::checklist(records));
    }

    fn show_todos(&mut self, call_id: Option<String>, mut items: Vec<TodoItem>) {
        scrub_todos(&mut items, self.home, &self.redactions);
        if call_id.is_some() || items != self.todos {
            self.todos.clone_from(&items);
            self.out.push(AgentEvent::TodosUpdated { call_id, items });
        }
    }

    fn event(&mut self, record_index: usize, event: &'a LogEvent) {
        match event {
            LogEvent::SessionStarted { model, .. } | LogEvent::SettingsChanged { model, .. } => {
                self.model.clone_from(model);
            }
            LogEvent::TurnStarted { model, .. } => self.turn_model.clone_from(model),
            LogEvent::CompactionFailed {
                turn: Some(turn),
                kind,
                message,
                ..
            } => {
                if let Some(kind) = hook_compaction_error_kind(kind) {
                    self.compaction_failures
                        .entry(*turn)
                        .or_default()
                        .push((kind, message));
                }
            }
            _ => {}
        }
        match event {
            LogEvent::TurnStarted {
                turn,
                origin,
                retry_of,
                ends,
                ..
            } => {
                self.flush_end();
                self.citations.clear();
                self.turn = *turn;
                self.step = 0;
                self.replied = false;
                self.delivered.clear();
                self.out.push(AgentEvent::TurnStarted {
                    turn: *turn,
                    origin: *origin,
                    retry_of: *retry_of,
                    answers: None,
                    ends: ends.clone(),
                });
                self.enter_step(1);
                let ids: Vec<InputId> = opening_of(event).into_iter().map(|i| i.id).collect();
                self.deliver(&ids, 1);
            }
            LogEvent::InputAccepted { id, .. } if self.inputs.delivered.contains(id) => {
                self.accept(id);
            }
            LogEvent::InputDelivered { step, ids, .. } => {
                self.enter_step(*step);
                self.deliver(ids, *step);
            }
            LogEvent::InputCommitted { step, inputs, .. } => {
                let ids: Vec<InputId> = inputs
                    .iter()
                    .filter(|i| !crate::agent::retry::is_call_note_id(&i.id))
                    .map(|i| i.id.clone())
                    .collect();
                self.deliver(&ids, *step);
            }
            LogEvent::InputWithdrawn { ids, by, .. } => {
                let ids: Vec<InputId> = ids
                    .iter()
                    .filter(|id| self.delivered.contains(*id))
                    .cloned()
                    .collect();
                if !ids.is_empty() {
                    self.out.push(AgentEvent::InputWithdrawn {
                        ids,
                        by: by.clone(),
                    });
                }
            }
            LogEvent::StepEnded {
                step,
                usage,
                cost_usd,
                lower_bound,
                ..
            } => {
                self.enter_step(*step);
                self.step_end = Some(AgentEvent::StepEnded {
                    step: *step,
                    usage: *usage,
                    cost_usd: *cost_usd,
                    lower_bound: *lower_bound,
                });
            }
            LogEvent::SearchStarted {
                turn,
                step,
                attempt,
                ..
            } if self.inputs.last(*turn, *step, *attempt) => {
                self.enter_step(*step);
                self.out.extend(live_event(event));
            }
            LogEvent::SearchEnded {
                turn,
                step,
                attempt,
                ..
            } if self.inputs.last(turn.unwrap_or(self.turn), *step, *attempt) => {
                self.out.extend(live_event(event));
            }
            LogEvent::Citations {
                turn,
                step,
                attempt,
                ..
            } if self.inputs.last(*turn, *step, *attempt) => {
                self.citations.extend(live_event(event));
            }
            LogEvent::MessageCommitted { message, .. } if message.role == Role::Assistant => {
                self.reply(message);
            }
            LogEvent::ContextCompacted {
                turn,
                trigger,
                engine,
                before,
                after,
                partial,
                usage,
                summary,
                block,
                requested_by,
                ..
            } => {
                self.flush_end();
                if requested_by.is_some() {
                    self.out.push(AgentEvent::CompactionStarted {
                        trigger: *trigger,
                        turn: *turn,
                        switch: None,
                        requested_by: requested_by.clone(),
                    });
                }
                let summary = summary.clone().or_else(|| {
                    block
                        .as_ref()
                        .and_then(|b| b.get("content"))
                        .and_then(Value::as_str)
                        .map(str::to_owned)
                });
                self.out.push(AgentEvent::Compacted {
                    trigger: *trigger,
                    engine: *engine,
                    before: *before,
                    after: *after,
                    partial: *partial,
                    converted: *trigger == wisp_protocol::CompactionTrigger::Convert,
                    summary,
                    paused: false,
                    cache_effect: CacheEffect::Rebuilt {
                        reason: RebuildReason::Compaction,
                    },
                    usage: *usage,
                    cost: None,
                    answers: None,
                    turn: *turn,
                });
            }
            LogEvent::CompactionFailed {
                turn,
                trigger,
                engine,
                kind,
                message,
                usage,
                requested_by: requested_by @ Some(_),
                ..
            } => {
                self.flush_end();
                self.out.push(AgentEvent::CompactionStarted {
                    trigger: *trigger,
                    turn: *turn,
                    switch: None,
                    requested_by: requested_by.clone(),
                });
                self.out.push(AgentEvent::CompactionFailed {
                    trigger: *trigger,
                    engine: *engine,
                    kind: kind.clone(),
                    message: message.clone(),
                    usage: *usage,
                    cost: None,
                    answers: None,
                    turn: *turn,
                    paused: false,
                    requested_by: requested_by.clone(),
                });
            }
            LogEvent::CacheMiss { .. }
            | LogEvent::CacheMissRepeated { .. }
            | LogEvent::Fallback { .. }
            | LogEvent::WindowLowered { .. } => self.out.extend(live_event(event)),
            LogEvent::Retrying {
                turn: Some(_),
                attempt,
                delay_ms,
                reason,
                step,
                kind,
                detail,
                usage,
                partial_output,
                model,
                cost_usd,
                lower_bound,
                resend,
                busy,
                ..
            } => {
                let step = step.unwrap_or(self.step);
                self.enter_step(step);
                let chars = partial_output.as_deref().map_or(0, |p| p.chars().count());
                self.out.push(AgentEvent::Retrying {
                    attempt: *attempt,
                    delay_ms: *delay_ms,
                    reason: reason.clone(),
                    step,
                    kind: kind.unwrap_or(ErrorKind::Other),
                    detail: detail.clone(),
                    max_attempts: 0,
                    discarded_blocks: 0,
                    discarded_chars: u64::try_from(chars).unwrap_or(u64::MAX),
                    usage: *usage,
                    model: model.clone(),
                    cost_usd: *cost_usd,
                    lower_bound: *lower_bound,
                    resend: *resend,
                    busy: *busy,
                });
            }
            LogEvent::SettingsChanged {
                model,
                effort,
                cache,
                endpoint_name,
                account_bound,
                withdrawn: None,
                changes,
                ..
            } if !only_client_owned(changes) => self.out.push(AgentEvent::SettingsChanged {
                answers: None,
                endpoint: endpoint_name.clone().unwrap_or_default(),
                model: model.clone(),
                effort: *effort,
                cache: cache.clone(),
                account_bound: account_bound.clone(),
                changes: Vec::new(),
                withdrawn: None,
                restored: None,
            }),
            event if let Some(fact) = feature_state(event) => match fact {
                FeatureFact::Pair {
                    feature,
                    endpoint: Some(endpoint),
                    model,
                    state,
                } => self.out.push(state_event(feature, endpoint, model, state)),
                FeatureFact::Pair { endpoint: None, .. } => {}
                FeatureFact::Search { endpoint, state } => {
                    let model = self.turn_model.as_deref().unwrap_or(&self.model);
                    let event = state_event(Feature::NativeWebSearch, endpoint, model, state);
                    self.out.push(event);
                }
            },
            LogEvent::PluginsChanged {
                added,
                updated,
                removed,
                kept,
                notes,
                ..
            } => self.out.push(AgentEvent::PluginsChanged {
                added: added.clone(),
                updated: updated.clone(),
                removed: removed.clone(),
                kept: kept.clone(),
                notes: notes.clone(),
                links: None,
            }),
            LogEvent::ToolsChanged {
                via,
                added,
                removed,
                copy,
                ..
            } => self.out.push(AgentEvent::ToolsChanged {
                added: added.clone(),
                removed: removed.clone(),
                via: *via,
                cache: copy.system_prompt.is_some().then_some(
                    wisp_protocol::CacheEffect::Rebuilt {
                        reason: wisp_protocol::RebuildReason::ToolsChange,
                    },
                ),
            }),
            LogEvent::TurnEnded {
                turn,
                outcome,
                stats,
                ..
            } => self.ended(*turn, outcome.clone(), stats.clone()),
            LogEvent::Rewound {
                point,
                reason,
                dropped,
                cache,
                ..
            } => {
                self.flush_end();
                self.out.push(AgentEvent::Rewound {
                    answers: None,
                    point: *point,
                    reason: reason.clone(),
                    dropped: dropped.clone(),
                    returned: Vec::new(),
                    cache: cache.clone(),
                });
                self.rebuild_todos(record_index);
            }
            LogEvent::TodosUpdated { call_id, items, .. } => {
                self.flush_end();
                self.show_todos(Some(call_id.clone()), items.clone());
            }
            LogEvent::AgentRenamed { agent, to, .. } => {
                self.flush_end();
                self.out.push(AgentEvent::AgentRenamed {
                    request: None,
                    agent: agent.clone(),
                    to: to.clone(),
                });
            }
            LogEvent::HookRan { .. } | LogEvent::HookApplied { .. } => {
                self.flush_end();
                self.out.extend(live_event(event));
            }
            LogEvent::ToolCallRequested { .. }
            | LogEvent::ToolCallStarted { .. }
            | LogEvent::ToolCallFinished { .. }
            | LogEvent::QuestionAsked { .. }
            | LogEvent::QuestionClosed { .. } => {
                if let Some((_, mut replayed)) = replayed(event) {
                    if let AgentEvent::ToolCallRequested { tool, display, .. } = &mut replayed
                        && let Some(declared) = self.toolbox.get(tool)
                    {
                        display.purpose = declared.meta().purpose;
                    }
                    self.flush_end();
                    self.out.push(replayed);
                }
            }
            LogEvent::SessionStarted { .. }
            | LogEvent::InputAccepted { .. }
            | LogEvent::InputRevised { .. }
            | LogEvent::InputHeld { .. }
            | LogEvent::InputUnheld { .. }
            | LogEvent::Retrying { .. }
            | LogEvent::RebuildMarked { .. }
            | LogEvent::AttachmentsWithdrawn { .. }
            | LogEvent::CacheRefresh { .. }
            | LogEvent::ModelAsked { .. }
            | LogEvent::SessionNamed { .. }
            | LogEvent::VoiceCall { .. }
            | LogEvent::UnknownProviderEvent { .. }
            | LogEvent::InputTransformations { .. }
            | LogEvent::Rejected { .. }
            | LogEvent::WakeHeld { .. }
            | LogEvent::AgentRefused { .. }
            | LogEvent::AgentSpawned { .. }
            | LogEvent::AgentEnded { .. }
            | LogEvent::AgentReported { .. }
            | LogEvent::MessageCommitted { .. }
            | LogEvent::ReturnedTaken { .. }
            | LogEvent::SessionEnded { .. }
            | LogEvent::ImageLost { .. }
            | LogEvent::ResumeFellBack { .. }
            | LogEvent::SessionResumed { .. }
            | LogEvent::SettingsChanged { .. }
            | LogEvent::PerMessageEffort { .. }
            | LogEvent::BindingControls { .. }
            | LogEvent::NativeWebSearch { .. }
            | LogEvent::NativeCompaction { .. }
            | LogEvent::InlineTools { .. }
            | LogEvent::DeferredTools { .. }
            | LogEvent::CompactionPrepared { .. }
            | LogEvent::CompactionFailed { .. }
            | LogEvent::TailClosed { .. }
            | LogEvent::RenderingForm { .. }
            | LogEvent::ThinkingCut { .. }
            | LogEvent::SearchHistoryRendered { .. }
            | LogEvent::AttachmentsAsNotes { .. }
            | LogEvent::SearchHistoryUnreadable { .. }
            | LogEvent::SystemMarkerDropped { .. }
            | LogEvent::WebSearchNotRun { .. }
            | LogEvent::SearchStarted { .. }
            | LogEvent::SearchEnded { .. }
            | LogEvent::Citations { .. }
            | LogEvent::InstructionsLoaded { .. }
            | LogEvent::InstructionsChanged { .. }
            | LogEvent::ProcessStarted { .. }
            | LogEvent::ProcessEnded { .. }
            | LogEvent::ProcessResized { .. }
            | LogEvent::ProcessAttached { .. }
            | LogEvent::ProcessDetached { .. }
            | LogEvent::ProcessHeard { .. }
            | LogEvent::ProcessNeedsInput { .. }
            | LogEvent::ProcessNeedsInputCleared { .. }
            | LogEvent::EndpointSaved { .. }
            | LogEvent::EndpointNotSaved { .. }
            | LogEvent::EndpointEdited { .. }
            | LogEvent::EndpointRemoved { .. }
            | LogEvent::EndpointsChanged { .. }
            | LogEvent::Unknown => {}
        }
    }

    fn run(mut self) -> Vec<AgentEvent> {
        let records = self.records;
        for (index, record) in records.iter().enumerate() {
            if matches!(record.event, LogEvent::SessionResumed { .. }) {
                self.flush_end();
                self.inputs = Inputs::read(run_of(records, index));
                self.accepted.clear();
                self.delivered.clear();
                self.compaction_failures.clear();
                self.out.push(AgentEvent::SessionResumed {
                    turns: self.turn,
                    moved: Vec::new(),
                    stay: Vec::new(),
                });
            }
            self.event(index, &record.event);
        }
        self.flush_end();
        if !self.todos.is_empty() && self.toolbox.provider_of(crate::tool::TODOS).is_none() {
            self.out.push(AgentEvent::TodosUpdated {
                call_id: None,
                items: Vec::new(),
            });
        }
        for event in &mut self.out {
            if let Some(audit) = crate::session::agent_hook_audit(event) {
                audit.scrub(self.home, &self.redactions);
            }
        }
        self.out
    }
}

fn same_compaction_reason(earlier: &str, terminal: &str) -> bool {
    let Some((prefix, tail)) = terminal.split_once(REDACTED) else {
        return earlier == terminal;
    };
    let (middle, suffix) = tail
        .rsplit_once(REDACTED)
        .map_or((None, tail), |(middle, suffix)| (Some(middle), suffix));
    let Some(mut available) = earlier
        .strip_prefix(prefix)
        .and_then(|earlier| earlier.strip_suffix(suffix))
    else {
        return false;
    };
    for literal in middle.into_iter().flat_map(|middle| middle.split(REDACTED)) {
        let Some((_, rest)) = available.split_once(literal) else {
            return false;
        };
        available = rest;
    }
    true
}

fn only_client_owned(changes: &[LoggedChange]) -> bool {
    let mut known = changes.iter().filter_map(|c| c.target.known()).peekable();
    known.peek().is_some() && known.all(|t| t.spec().owner == Owner::Client)
}

fn run_of(records: &[LogRecord], start: usize) -> &[LogRecord] {
    let rest = records.get(start..).unwrap_or_default();
    let end = rest
        .iter()
        .skip(1)
        .position(|r| matches!(r.event, LogEvent::SessionResumed { .. }))
        .map_or(rest.len(), |p| p + 1);
    rest.get(..end).unwrap_or_default()
}

pub fn transcript(
    records: &[LogRecord],
    toolbox: &Toolbox,
    offer: Option<(u64, RetryOffer)>,
) -> Vec<AgentEvent> {
    super::transcript_with_home(records, toolbox, offer, os_detection_home().as_deref())
}

pub(crate) fn transcript_with_home(
    records: &[LogRecord],
    toolbox: &Toolbox,
    offer: Option<(u64, RetryOffer)>,
    home: Option<&Path>,
) -> Vec<AgentEvent> {
    let latest = latest_spanned_mail(records);
    transcript_with_latest(records, &latest, toolbox, offer, home)
}

pub(crate) fn transcript_with_latest(
    records: &[LogRecord],
    latest: &LatestBriefs,
    toolbox: &Toolbox,
    offer: Option<(u64, RetryOffer)>,
    home: Option<&Path>,
) -> Vec<AgentEvent> {
    Replay {
        inputs: Inputs::read(run_of(records, 0)),
        latest_mail: latest,
        offer,
        out: Vec::new(),
        turn: 0,
        step: 0,
        replied: false,
        accepted: BTreeSet::new(),
        delivered: BTreeSet::new(),
        step_end: None,
        citations: Vec::new(),
        model: String::new(),
        turn_model: None,
        records,
        toolbox,
        todos: Vec::new(),
        home,
        redactions: held(),
        compaction_failures: BTreeMap::new(),
    }
    .run()
}

#[cfg(test)]
mod tests;
