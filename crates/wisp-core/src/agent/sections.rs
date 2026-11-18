use std::collections::{BTreeMap, BTreeSet};
use std::future::Future;
use std::pin::Pin;

use futures::future::BoxFuture;
use tokio::sync::mpsc;
use tokio::time::Instant;
use tokio_util::sync::CancellationToken;
use wisp_plugin_api::hooks::{Event, HeadInfo};
use wisp_plugin_api::instructions::Change;
use wisp_plugin_api::manifest::SectionPlace;
use wisp_plugin_api::prompt::{At as PromptAt, Prior, Rendered};
use wisp_protocol::log::{
    LEAD_ENVIRONMENT, LeadEntry, LogEvent, LogRecord, SECTIONS_SINCE, SectionSnapshot, TurnLead,
};
use wisp_protocol::{AgentEvent, ClientCommand, ContentBlock, HookOutcome, SourceChange};

use wisp_instructions::Scope;
use wisp_instructions::delta;

use super::Agent;
use super::compact::strategy::HeadAsk;
use super::hooks::{Busy, Mode};
use crate::context::text_tokens;
use crate::plugin::Problem;
use crate::plugin::caller::{At, Caller};
use crate::plugin::hooks::points::{BEFORE_TURN, COMPACTION_HEAD};
use crate::plugin::hooks::sections::{Limits, SECTION_TIMEOUT, START_TIMEOUT, SectionCall};
use crate::plugin::hooks::{Fire, Name, Ran, What};

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct Room {
    pub(super) info: HeadInfo,
    pub(super) smaller: Option<(u64, u64)>,
    pub(super) fits: Option<(u64, u64)>,
}

struct Left {
    info: HeadInfo,
    tokens: Option<u64>,
}

impl Room {
    fn left(self, rendered: u64, estimated: u64) -> Left {
        let corrected = |figure: u64| figure.saturating_add(rendered).saturating_sub(estimated);
        let tokens = [self.smaller, self.fits]
            .into_iter()
            .flatten()
            .map(|(bound, figure)| bound.saturating_sub(corrected(figure)))
            .min();
        Left {
            info: self.info,
            tokens,
        }
    }
}

#[derive(Debug, Clone, Default)]
pub(super) struct SectionState {
    committed: BTreeMap<String, Prior>,
    start: BTreeMap<String, String>,
    start_legacy: BTreeMap<String, Option<String>>,
    pending: Option<PendingSections>,
    turn_texts: BTreeMap<String, String>,
    head_texts: BTreeMap<String, String>,
    off: BTreeSet<Name>,
    pub(super) resumed_start: Option<StartAt>,
}

#[derive(Debug, Clone)]
struct PendingSections {
    epoch: u64,
    rendered: BTreeMap<String, String>,
    opened: BTreeMap<String, Option<String>>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(super) struct HeadTexts {
    pub(super) sections: Vec<HeadSection>,
    pub(super) hooks: Vec<String>,
    pub(super) strategy: Option<StrategyHead>,
    pub(super) chosen: Option<String>,
    pub(super) instruction: Option<String>,
    pub(super) refused: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct StrategyHead {
    pub(super) plugin: String,
    pub(super) texts: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct HeadSection {
    pub(super) name: String,
    pub(super) text: String,
    pub(super) snapshot: String,
}

impl HeadTexts {
    pub(super) fn blocks(&self) -> impl Iterator<Item = ContentBlock> + '_ {
        self.sections.iter().map(|s| ContentBlock::Text {
            text: s.text.clone(),
        })
    }

    pub(super) fn strategy_blocks(&self) -> Option<Vec<ContentBlock>> {
        let strategy = self.strategy.as_ref()?;
        Some(
            strategy
                .texts
                .iter()
                .map(|text| ContentBlock::Text { text: text.clone() })
                .collect(),
        )
    }

    pub(super) fn built_by(&self) -> Option<String> {
        self.strategy.as_ref().map(|s| s.plugin.clone())
    }

    pub(super) fn hook_blocks(&self) -> impl Iterator<Item = ContentBlock> + '_ {
        self.hooks
            .iter()
            .map(|text| ContentBlock::Text { text: text.clone() })
    }

    pub(super) fn logged(&self) -> Vec<SectionSnapshot> {
        self.sections
            .iter()
            .map(|s| SectionSnapshot {
                name: s.name.clone(),
                snapshot: s.snapshot.clone(),
            })
            .collect()
    }
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct StartRendered {
    pub texts: Vec<(String, String)>,
    pub snapshots: BTreeMap<String, String>,
    pub warnings: Vec<Problem>,
}

pub(super) const INSTRUCTIONS_SECTION: &str = "instructions/instructions";

pub(super) const PRESET_SECTION: &str = "core/preset";

pub(super) const CHANGE_SECTIONS: [&str; 2] = [INSTRUCTIONS_SECTION, PRESET_SECTION];

pub(super) fn text_of(prior: &Prior) -> Option<String> {
    match prior {
        Prior::Snapshot(text) | Prior::Legacy(Some(text)) => Some(text.clone()),
        Prior::None | Prior::Legacy(None) => None,
    }
}

pub(super) fn entry_prior(entry: &LeadEntry) -> Option<Prior> {
    match &entry.snapshot {
        Some(snapshot) => Some(Prior::Snapshot(snapshot.clone())),
        None if entry.by == LEAD_ENVIRONMENT => Some(Prior::Legacy(Some(entry.text.clone()))),
        None => None,
    }
}

pub(super) fn is_section_entry(entry: &LeadEntry) -> bool {
    entry.snapshot.is_some() || entry.by == LEAD_ENVIRONMENT
}

pub(super) fn kept_for_retry(entry: &LeadEntry) -> bool {
    entry.replay || is_section_entry(entry)
}

pub(super) fn environment_in(blocks: &[ContentBlock]) -> Option<String> {
    blocks.iter().find_map(|b| match b {
        ContentBlock::Text { text } if text.starts_with(wisp_tool_text::ENVIRONMENT_TAG) => {
            Some(text.clone())
        }
        _ => None,
    })
}

fn start_warning(name: &Name) -> Problem {
    Problem {
        plugin: name.plugin.clone(),
        message: format!(
            "its prompt section `{name}` didn't render at start, so the project block leaves it out"
        ),
    }
}

impl SectionState {
    pub(super) fn committed(&self) -> &BTreeMap<String, Prior> {
        &self.committed
    }

    pub(super) fn restore(&mut self, committed: BTreeMap<String, Prior>) {
        self.committed = committed;
        self.pending = None;
    }

    pub(super) fn start(&self) -> &BTreeMap<String, String> {
        &self.start
    }

    pub(super) fn set_start(&mut self, start: &StartAt) {
        self.start = start.snapshots.clone();
        self.start_legacy = start.legacy.clone();
    }

    pub(super) fn off(&self) -> &BTreeSet<Name> {
        &self.off
    }

    pub(super) fn set_off(&mut self, off: BTreeSet<Name>) {
        self.off = off;
    }
}

impl Agent {
    pub(super) fn section_names(&self) -> Vec<String> {
        let table = self.hooks.engine.table();
        table
            .order(BEFORE_TURN)
            .iter()
            .filter(|name| table.section(name).is_some())
            .map(ToString::to_string)
            .collect()
    }

    fn plugin_section(&self, name: &str) -> Option<Name> {
        let name = Name::parse(name)?;
        self.hooks.engine.section_runnable(&name).then_some(name)
    }

    fn is_system(&self, name: &str) -> bool {
        name == PRESET_SECTION
            || Name::parse(name)
                .and_then(|n| self.hooks.engine.table().section(&n))
                .is_some_and(|spec| spec.place == SectionPlace::System)
    }

    pub(super) fn section_base(&self, name: &str) -> Prior {
        if !self.is_system(name) {
            return Prior::None;
        }
        match (
            self.sections.start.get(name),
            self.sections.start_legacy.get(name),
        ) {
            (Some(snapshot), _) => Prior::Snapshot(snapshot.clone()),
            (None, Some(text)) => Prior::Legacy(text.clone()),
            (None, None) => Prior::None,
        }
    }

    fn section_previous(&self, name: &str) -> Prior {
        self.sections
            .committed
            .get(name)
            .cloned()
            .unwrap_or(Prior::None)
    }

    pub(super) fn window_base(&self) -> BTreeMap<String, Prior> {
        self.sections
            .start
            .keys()
            .chain(self.sections.start_legacy.keys())
            .filter(|name| self.is_system(name))
            .map(|name| (name.clone(), self.section_base(name)))
            .collect()
    }

    pub(super) fn lead_previous(&self) -> BTreeMap<String, Prior> {
        self.section_names()
            .into_iter()
            .map(|name| {
                let previous = self.section_previous(&name);
                (name, previous)
            })
            .collect()
    }

    pub(super) fn note_lead(&mut self, lead: &TurnLead, rendered: &[(String, Option<Rendered>)]) {
        for (name, result) in rendered {
            match result.as_ref().and_then(|r| r.text.clone()) {
                Some(text) => self.sections.turn_texts.insert(name.clone(), text),
                None => self.sections.turn_texts.remove(name),
            };
        }
        let opened = self
            .section_names()
            .into_iter()
            .map(|name| {
                let text = text_of(&self.section_previous(&name));
                (name, text)
            })
            .collect();
        let rendered = lead
            .entries
            .iter()
            .filter(|e| is_section_entry(e))
            .map(|e| {
                (
                    e.by.clone(),
                    e.snapshot.clone().unwrap_or_else(|| e.text.clone()),
                )
            })
            .collect();
        self.sections.pending = Some(PendingSections {
            epoch: self.epoch_number(),
            rendered,
            opened,
        });
    }

    pub(super) fn commit_sections(&mut self) {
        let Some(pending) = self.sections.pending.take() else {
            return;
        };
        if pending.epoch != self.epoch_number() {
            return;
        }
        for (name, snapshot) in pending.rendered {
            self.sections
                .committed
                .insert(name, Prior::Snapshot(snapshot));
        }
    }

    pub(super) fn drop_pending_sections(&mut self) {
        self.sections.pending = None;
    }

    pub(super) fn section_target(&self, name: &str) -> Option<String> {
        let pending = self.sections.pending.as_ref()?;
        pending
            .rendered
            .get(name)
            .cloned()
            .or_else(|| pending.opened.get(name).cloned().flatten())
    }

    pub(super) fn install_sections(&mut self, texts: &HeadTexts) {
        let mut committed = self.window_base();
        for section in &texts.sections {
            committed.insert(
                section.name.clone(),
                Prior::Snapshot(section.snapshot.clone()),
            );
        }
        self.sections.committed = committed;
        self.sections.head_texts = texts
            .sections
            .iter()
            .map(|s| (s.name.clone(), s.text.clone()))
            .collect();
    }

    pub(super) fn turn_section_texts(&self) -> Vec<String> {
        self.section_names()
            .into_iter()
            .filter_map(|name| {
                self.plugin_section(&name)?;
                self.sections.turn_texts.get(&name).cloned()
            })
            .collect()
    }

    pub(super) fn estimated_head_texts(&self) -> HeadTexts {
        let sections = self
            .section_names()
            .into_iter()
            .filter_map(|name| {
                self.plugin_section(&name)?;
                let text = self.sections.head_texts.get(&name).cloned()?;
                Some(HeadSection {
                    name,
                    snapshot: String::new(),
                    text,
                })
            })
            .collect();
        HeadTexts {
            sections,
            ..HeadTexts::default()
        }
    }

    pub(super) fn head_section_tokens(&self) -> u64 {
        self.estimated_head_texts()
            .sections
            .iter()
            .map(|s| text_tokens(&s.text))
            .fold(0, u64::saturating_add)
    }

    fn render_call(&self, name: &str, at: PromptAt) -> SectionCall {
        match at {
            PromptAt::Head => SectionCall {
                at,
                previous: self.section_base(name),
                target: self.section_target(name),
            },
            PromptAt::Turn | PromptAt::Start => SectionCall {
                at,
                previous: if at == PromptAt::Start {
                    Prior::None
                } else {
                    self.section_previous(name)
                },
                target: None,
            },
        }
    }

    pub(super) async fn await_render<T>(
        &mut self,
        render: BoxFuture<'static, T>,
        cancel: &CancellationToken,
        mode: Mode,
        commands: &mut mpsc::Receiver<ClientCommand>,
        events: &mpsc::Sender<AgentEvent>,
    ) -> Option<T> {
        let processes = self.processes.clone();
        let mailbox = self.mailbox.clone();
        let mut render = render;
        let mut closed = false;
        let busy = self.hooks.busy_with.replace(Busy::Hook);
        let done = loop {
            tokio::select! {
                biased;
                done = &mut render => break Some(done),
                command = commands.recv(), if !closed => {
                    closed |= command.is_none();
                    let stop = match mode {
                        Mode::Turn => self.on_command(command, events).await,
                        Mode::PreTurn => self.pre_turn_command(command, events).await,
                    };
                    if stop {
                        cancel.cancel();
                        break None;
                    }
                }
                done = self.queue.done() => self.finish(done, events).await,
                () = processes.changed() => self.drain_processes(events).await,
                () = mailbox.arrived() => self.accept_mail(events).await,
            }
        };
        self.hooks.busy_with = busy;
        if busy.is_none() && mode == Mode::Turn && !self.stopped_now() {
            self.start_deferred(events).await;
        }
        done
    }

    async fn render_one(
        &mut self,
        name: &Name,
        at: PromptAt,
        mode: Mode,
        commands: &mut mpsc::Receiver<ClientCommand>,
        events: &mpsc::Sender<AgentEvent>,
    ) -> (Option<Option<Rendered>>, Ran) {
        let call = self.render_call(&name.to_string(), at);
        let cancel = CancellationToken::new();
        let at_turn = At {
            turn: self.running_turn(),
            step: None,
            call_id: None,
        };
        let limits = Limits {
            cancel: cancel.clone(),
            deadline: Some(Instant::now() + SECTION_TIMEOUT),
            at: at_turn.clone(),
        };
        let render = self.hooks.engine.render_section(name, call, limits);
        let started = Instant::now();
        match self
            .await_render(render, &cancel, mode, commands, events)
            .await
        {
            Some((rendered, ran)) => (Some(rendered), ran),
            None => (
                None,
                Ran {
                    hook: name.to_string(),
                    point: crate::plugin::hooks::sections::point_of(at).to_owned(),
                    at: at_turn,
                    outcome: HookOutcome::Cancelled,
                    ms: u64::try_from(started.elapsed().as_millis()).unwrap_or(u64::MAX),
                },
            ),
        }
    }

    pub(super) fn has_plugin_sections(&self) -> bool {
        let names = self.section_names();
        names.iter().any(|name| self.plugin_section(name).is_some())
    }

    fn publish_for_sections(&mut self) {
        if self.has_plugin_sections() {
            self.publish_hook_snapshot();
        }
    }

    fn stopped_now(&self) -> bool {
        self.open_track().is_some_and(|t| t.stopped)
    }

    pub(super) fn head_step<'a>(
        &'a mut self,
        room: Room,
        ask: Option<HeadAsk>,
        commands: &'a mut mpsc::Receiver<ClientCommand>,
        events: &'a mpsc::Sender<AgentEvent>,
    ) -> Pin<Box<impl Future<Output = HeadTexts> + 'a>> {
        Box::pin(self.head_step_unboxed(room, ask, commands, events))
    }

    async fn head_step_unboxed(
        &mut self,
        room: Room,
        ask: Option<HeadAsk>,
        commands: &mut mpsc::Receiver<ClientCommand>,
        events: &mpsc::Sender<AgentEvent>,
    ) -> HeadTexts {
        self.publish_for_sections();
        let mut texts = HeadTexts::default();
        for name in self.section_names() {
            let Some(plugin) = self.plugin_section(&name) else {
                continue;
            };
            if self.stopped_now() {
                continue;
            }
            let (rendered, ran) = self
                .render_one(&plugin, PromptAt::Head, Mode::Turn, commands, events)
                .await;
            self.emit_runs(vec![ran], events).await;
            match rendered {
                Some(Some(Rendered {
                    snapshot,
                    text: Some(text),
                })) => texts.sections.push(HeadSection {
                    name,
                    text,
                    snapshot,
                }),
                Some(_) => {}
                None => break,
            }
        }
        if let Some((update, snapshot)) = self.preset_update(&self.section_base(PRESET_SECTION)) {
            texts.sections.push(HeadSection {
                name: PRESET_SECTION.to_owned(),
                text: update.text,
                snapshot,
            });
        }
        let mut layout = (0, 0);
        if let Some(mut ask) = ask {
            texts.chosen = Some(ask.run.plugin.clone());
            texts.instruction.clone_from(&ask.run.supplied);
            if !self.stopped_now() {
                texts.strategy =
                    Box::pin(self.strategy_head(&mut ask, &room.info, commands, events)).await;
            }
            let summary = ask.summary.as_deref();
            let epoch_turns = (room.info.epoch, (room.info.first_turn, room.info.last_turn));
            layout = self.layout_figures(&texts, epoch_turns, summary);
            if let (Some(before), Some(summary)) = (ask.before, summary) {
                let head = self.build_head(None, summary, epoch_turns, &texts);
                let after = self
                    .fixed_tokens()
                    .saturating_add(crate::context::blocks_tokens(&head.content));
                if after >= before {
                    texts.refused = Some(super::compact::not_smaller(after, before));
                    return texts;
                }
            }
        }
        if !self.stopped_now() {
            let rendered = texts
                .sections
                .iter()
                .map(|s| text_tokens(&s.text))
                .fold(layout.0, u64::saturating_add);
            let estimated = self.head_section_tokens().saturating_add(layout.1);
            let room = room.left(rendered, estimated);
            let at = self.at(None, None);
            let event = Event::CompactionHead(room.info);
            let fire = Fire::new(COMPACTION_HEAD, What::Event(event), at).within(room.tokens);
            texts.hooks = self.await_hooks(fire, commands, events).await.head;
        }
        texts
    }

    pub(super) fn refresh_sections<'a>(
        &'a mut self,
        commands: &'a mut mpsc::Receiver<ClientCommand>,
        events: &'a mpsc::Sender<AgentEvent>,
    ) -> Pin<Box<impl Future<Output = ()> + 'a>> {
        Box::pin(self.refresh_sections_unboxed(Mode::Turn, commands, events))
    }

    pub(super) fn refresh_sections_idle<'a>(
        &'a mut self,
        commands: &'a mut mpsc::Receiver<ClientCommand>,
        events: &'a mpsc::Sender<AgentEvent>,
    ) -> Pin<Box<impl Future<Output = ()> + 'a>> {
        Box::pin(self.refresh_sections_unboxed(Mode::PreTurn, commands, events))
    }

    async fn refresh_sections_unboxed(
        &mut self,
        mode: Mode,
        commands: &mut mpsc::Receiver<ClientCommand>,
        events: &mpsc::Sender<AgentEvent>,
    ) {
        self.publish_for_sections();
        for name in self.section_names() {
            let Some(plugin) = self.plugin_section(&name) else {
                continue;
            };
            for at in [PromptAt::Turn, PromptAt::Head] {
                if self.stopped_now() {
                    return;
                }
                let (rendered, ran) = self.render_one(&plugin, at, mode, commands, events).await;
                if !matches!(ran.outcome, HookOutcome::Applied | HookOutcome::None) {
                    self.emit_runs(vec![ran], events).await;
                }
                let Some(rendered) = rendered else {
                    if mode == Mode::PreTurn && !self.closed {
                        Box::pin(self.idle_cancel(events)).await;
                    }
                    return;
                };
                let texts = match at {
                    PromptAt::Head => &mut self.sections.head_texts,
                    PromptAt::Turn | PromptAt::Start => &mut self.sections.turn_texts,
                };
                match rendered.and_then(|r| r.text) {
                    Some(text) => texts.insert(name.clone(), text),
                    None => texts.remove(&name),
                };
            }
        }
        if !self.stopped_now() {
            self.refresh_strategy(mode, Some(commands), events).await;
        }
    }

    pub async fn render_start(&mut self) -> StartRendered {
        self.start_switches();
        self.publish_hook_snapshot();
        let cancel = Caller::current().cancel.unwrap_or_default();
        let mut out = StartRendered::default();
        for name in self.section_names() {
            if !self.is_system(&name) {
                continue;
            }
            let Some(plugin) = self.plugin_section(&name) else {
                continue;
            };
            let bundled = self
                .hooks
                .engine
                .table()
                .section(&plugin)
                .is_some_and(|spec| spec.bundled);
            let limits = Limits {
                cancel: cancel.clone(),
                deadline: (!bundled).then(|| Instant::now() + START_TIMEOUT),
                at: At::default(),
            };
            let call = self.render_call(&name, PromptAt::Start);
            let (rendered, ran) = self
                .hooks
                .engine
                .render_section(&plugin, call, limits)
                .await;
            self.hooks.engine.record_ran(ran);
            match rendered {
                Some(Rendered {
                    snapshot,
                    text: Some(text),
                }) => {
                    out.texts.push((name.clone(), text));
                    out.snapshots.insert(name, snapshot);
                }
                Some(_) => {}
                None => out.warnings.push(start_warning(&plugin)),
            }
        }
        out
    }

    pub fn with_start_sections(mut self) -> Pin<Box<impl Future<Output = (Self, Vec<Problem>)>>> {
        Box::pin(async move {
            let rendered = Box::pin(self.render_start()).await;
            self.apply_start(&rendered);
            (self, rendered.warnings)
        })
    }

    pub fn with_config_dir(self, dir: Option<&std::path::Path>) -> Self {
        if let Some(host) = &self.hooks.host {
            host.set_config_dir(dir.map(|d| d.display().to_string()));
        }
        self
    }

    pub(super) fn apply_start(&mut self, rendered: &StartRendered) {
        let (own, others): (Vec<_>, Vec<_>) = rendered
            .texts
            .iter()
            .cloned()
            .partition(|(name, _)| name == INSTRUCTIONS_SECTION);
        let own = own.into_iter().next().map(|(_, text)| text);
        let texts: Vec<String> = others.into_iter().map(|(_, text)| text).collect();
        let instructed = self
            .hooks
            .host
            .as_ref()
            .map(|host| host.take_instructed())
            .unwrap_or_default();
        let model = self.session.model.clone();
        let info = self.priced_rates(self.endpoint, &model);
        if let Some(state) = &mut self.instructions {
            state.take_report(instructed, info.as_ref().map(|info| (model.as_str(), info)));
            self.session.instructions = state.parts_block(own, texts);
        } else if own.is_some() || !texts.is_empty() {
            let all: Vec<String> = self
                .session
                .instructions
                .take()
                .into_iter()
                .chain(own)
                .chain(texts)
                .collect();
            self.session.instructions = Some(all.join("\n"));
        }
        self.sections.start = rendered.snapshots.clone();
        if let Some(preset) = self.main_preset() {
            let snapshot = delta::snapshot(&preset.files);
            self.sections
                .start
                .insert(PRESET_SECTION.to_owned(), snapshot);
        }
        self.sections.start_legacy.clear();
        let base = self.window_base();
        for (name, prior) in base {
            self.sections.committed.insert(name, prior);
        }
    }

    pub(super) async fn log_instructions_change(
        &mut self,
        (name, text): (&str, &str),
        (turn, epoch, ahead): (Option<u64>, Option<u64>, bool),
        baseline: Option<Vec<delta::Entry>>,
        events: &mpsc::Sender<AgentEvent>,
    ) {
        let change = if name == PRESET_SECTION {
            let update = self.preset_update(&self.section_previous(PRESET_SECTION));
            Some(Change {
                changed: update.map(|(u, _)| u.changed).unwrap_or_default(),
                ..Change::default()
            })
        } else {
            self.hooks.host.as_ref().and_then(|h| h.take_change(text))
        };
        let Some(mut change) = change else {
            return;
        };
        if let Some(baseline) = baseline {
            change.changed = delta::since(change.changed, &baseline);
            if change.to.as_ref().is_some_and(|to| {
                baseline
                    .iter()
                    .any(|e| e.scope == to.scope && e.name == to.name)
            }) {
                change.from.clone_from(&change.to);
            }
        }
        let info = self.priced_rates(self.endpoint, &self.session.model);
        let priced = info
            .as_ref()
            .map(|info| (self.session.model.as_str(), info));
        let summary = crate::instructions::summary(change.summary, priced);
        let from = change
            .from
            .map(|i| Box::new(crate::instructions::source_info(i)));
        let to = change
            .to
            .map(|i| Box::new(crate::instructions::source_info(i)));
        let changed: Vec<SourceChange> = change
            .changed
            .into_iter()
            .map(|c| SourceChange {
                display_path: c.display_path,
                old_sha256: c.old_sha256,
                new_sha256: c.new_sha256,
            })
            .collect();
        let chosen = summary.chosen.map(Box::new);
        self.log(
            events,
            LogEvent::InstructionsChanged {
                turn,
                epoch,
                retry_of: None,
                ahead,
                from: from.clone(),
                to: to.clone(),
                changed: changed.clone(),
                chosen: chosen.clone(),
                imports: summary.imports.clone(),
                candidates: summary.candidates.clone(),
                sent: true,
                diagnostics: summary.diagnostics.clone(),
                text: text.to_owned(),
            },
        )
        .await;
        let event = AgentEvent::InstructionsChanged {
            turn,
            epoch,
            retry_of: None,
            ahead,
            from,
            to,
            changed,
            chosen,
            imports: summary.imports,
            candidates: summary.candidates,
            sent: true,
            diagnostics: summary.diagnostics,
        };
        super::emit(events, event).await;
    }

    pub(super) async fn log_head_instructions(
        &mut self,
        texts: &HeadTexts,
        (turn, epoch): (Option<u64>, u64),
        events: &mpsc::Sender<AgentEvent>,
    ) {
        for name in CHANGE_SECTIONS {
            let Some(section) = texts.sections.iter().find(|s| s.name == name) else {
                continue;
            };
            let ahead = self.section_target(name).is_some();
            let text = section.text.clone();
            let baseline = delta::prior_entries(&self.section_previous(name));
            let at = (turn, Some(epoch), ahead);
            self.log_instructions_change((name, &text), at, baseline, events)
                .await;
        }
    }

    pub(super) fn preset_lead(&self) -> Option<LeadEntry> {
        let (update, snapshot) = self.preset_update(&self.section_previous(PRESET_SECTION))?;
        Some(LeadEntry::new(PRESET_SECTION, update.text).with_snapshot(snapshot))
    }

    fn preset_update(&self, previous: &Prior) -> Option<(delta::Update, String)> {
        let preset = self.main_preset()?;
        let Prior::Snapshot(old) = previous else {
            return None;
        };
        let dir = preset.path.parent()?;
        let home = self
            .tools
            .as_ref()
            .and_then(|t| t.workspace.home.as_deref());
        let roots = delta::Roots {
            project: dir,
            config_dir: Some(dir),
            home: home.filter(|h| h.is_absolute()),
        };
        let left = |entry: &delta::Entry| Some((roots.shown(entry.scope, &entry.name), false));
        let update = delta::update(&delta::entries(old), &delta::items_of(&preset.files), left)?;
        Some((update, delta::snapshot(&preset.files)))
    }

    pub(super) fn start_sections(&self) -> Vec<SectionSnapshot> {
        self.section_names()
            .into_iter()
            .chain([PRESET_SECTION.to_owned()])
            .filter_map(|name| {
                let snapshot = self.sections.start.get(&name)?.clone();
                Some(SectionSnapshot { name, snapshot })
            })
            .collect()
    }
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(super) struct StartAt {
    pub(super) at: usize,
    pub(super) snapshots: BTreeMap<String, String>,
    pub(super) legacy: BTreeMap<String, Option<String>>,
}

#[derive(Debug, Clone, Default)]
pub(super) struct Moves {
    heads: BTreeMap<usize, BTreeMap<String, Prior>>,
    turns: Vec<TurnMove>,
}

pub(super) struct Window<'a> {
    head: Option<(usize, &'a BTreeMap<String, Prior>)>,
    turns: &'a [TurnMove],
}

impl Moves {
    pub(super) fn window(&self, head: Option<usize>, end: usize) -> Window<'_> {
        let from = head.map_or(0, |h| self.turns.partition_point(|m| m.at <= h));
        let to = self.turns.partition_point(|m| m.at < end);
        Window {
            head: head
                .filter(|h| *h < end)
                .and_then(|h| Some((h, self.heads.get(&h)?))),
            turns: self.turns.get(from..to).unwrap_or_default(),
        }
    }
}

#[derive(Debug, Clone)]
struct TurnMove {
    at: usize,
    turn: u64,
    entries: Vec<(String, Prior)>,
}

pub(super) fn logged_start(records: &[LogRecord]) -> StartAt {
    let mut start = StartAt::default();
    let mut block: Option<String> = None;
    let mut block_sha: Option<String> = None;
    for (at, record) in records.iter().enumerate() {
        let sections = match &record.event {
            LogEvent::SessionStarted {
                sections,
                instructions,
                instructions_sha256,
                ..
            } => {
                block.clone_from(instructions);
                block_sha.clone_from(instructions_sha256);
                Some(sections)
            }
            LogEvent::SessionResumed {
                sections,
                instructions,
                instructions_sha256,
                ..
            } => {
                if instructions.is_some() || *instructions_sha256 != block_sha {
                    block.clone_from(instructions);
                }
                block_sha.clone_from(instructions_sha256);
                sections.as_ref()
            }
            _ => None,
        };
        let Some(sections) = sections else {
            continue;
        };
        let mut snapshots: BTreeMap<String, String> = sections
            .iter()
            .map(|s| (s.name.clone(), s.snapshot.clone()))
            .collect();
        if let Some(snapshot) = snapshots.get_mut(INSTRUCTIONS_SECTION) {
            let scope = block
                .as_deref()
                .and_then(delta::block_scope)
                .unwrap_or(Scope::Project);
            *snapshot = delta::with_scope(snapshot, scope);
        }
        if let Some(text) = block.as_deref()
            && !snapshots.contains_key(PRESET_SECTION)
        {
            let entries = delta::entries_in(text, &[Scope::Preset]);
            if !entries.is_empty() {
                snapshots.insert(PRESET_SECTION.to_owned(), delta::snapshot_of(&entries));
            }
        }
        let legacy = block
            .clone()
            .filter(|text| !snapshots.contains_key(INSTRUCTIONS_SECTION) && delta::has_files(text))
            .map(|text| (INSTRUCTIONS_SECTION.to_owned(), Some(text)))
            .into_iter()
            .collect();
        start = StartAt {
            at,
            snapshots,
            legacy,
        };
    }
    start
}

fn head_sections(record: &LogRecord) -> Option<BTreeMap<String, Prior>> {
    let LogEvent::ContextCompacted {
        head,
        additions,
        sections,
        ..
    } = &record.event
    else {
        return None;
    };
    if record.schema_version >= SECTIONS_SINCE {
        return Some(
            sections
                .iter()
                .map(|s| (s.name.clone(), Prior::Snapshot(s.snapshot.clone())))
                .collect(),
        );
    }
    let blocks: Vec<ContentBlock> = head.content.iter().chain(additions).cloned().collect();
    Some(BTreeMap::from([(
        LEAD_ENVIRONMENT.to_owned(),
        Prior::Legacy(environment_in(&blocks)),
    )]))
}

pub(super) fn moves<'a>(records: impl IntoIterator<Item = (usize, &'a LogRecord)>) -> Moves {
    let mut moves = Moves::default();
    for (at, record) in records {
        if let Some(head) = head_sections(record) {
            moves.heads.insert(at, head);
        }
        if let LogEvent::TurnStarted { turn, lead, .. } = &record.event {
            let entries: Vec<(String, Prior)> = lead
                .entries
                .iter()
                .filter_map(|e| Some((e.by.clone(), entry_prior(e)?)))
                .collect();
            if !entries.is_empty() {
                moves.turns.push(TurnMove {
                    at,
                    turn: *turn,
                    entries,
                });
            }
        }
    }
    moves
}

impl Agent {
    pub(super) fn committed_at(
        &self,
        moves: &Window<'_>,
        kept: impl Fn(u64) -> bool,
        start: &StartAt,
    ) -> BTreeMap<String, Prior> {
        let system = |name: &str| self.is_system(name);
        let mut committed: BTreeMap<String, Prior> = start
            .snapshots
            .iter()
            .filter(|(name, _)| system(name))
            .map(|(name, snapshot)| (name.clone(), Prior::Snapshot(snapshot.clone())))
            .chain(
                start
                    .legacy
                    .iter()
                    .filter(|(name, _)| system(name) && !start.snapshots.contains_key(*name))
                    .map(|(name, text)| (name.clone(), Prior::Legacy(text.clone()))),
            )
            .collect();
        let moves_it = |name: &str, at: usize| !system(name) || at > start.at;
        if let Some((at, head)) = moves.head {
            for (name, prior) in head {
                if moves_it(name, at) {
                    committed.insert(name.clone(), prior.clone());
                }
            }
        }
        for TurnMove { at, turn, entries } in moves.turns {
            if !kept(*turn) {
                continue;
            }
            for (name, prior) in entries {
                if moves_it(name, *at) {
                    committed.insert(name.clone(), prior.clone());
                }
            }
        }
        committed
    }
}

#[cfg(test)]
mod tests;
