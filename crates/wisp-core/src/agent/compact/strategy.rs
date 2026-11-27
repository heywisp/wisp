use std::future::Future;
use std::pin::Pin;

use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;
use wisp_plugin_api::hooks::{CompactInfo, HeadInfo};
use wisp_plugin_api::strategy::{CallEntry, SaidEntry, StrategyInfo};
use wisp_protocol::settings::{Place, SettingValue, find, names};
use wisp_protocol::{AgentEvent, ClientCommand, CompactionTrigger, HookOutcome};

use super::{CompactionEngine, text_tokens};
use crate::agent::Agent;
use crate::agent::hooks::Mode;
use crate::agent::sections::{HeadTexts, StrategyHead};
use crate::plugin::caller::At;
use crate::plugin::hooks::Ran;
use crate::plugin::hooks::sections::Limits;
use crate::plugin::hooks::strategy::{
    Answer, Ask, HistoryView, STRATEGY_REFUSED, STRATEGY_TIMEOUT, strategy_hook,
};
use crate::secrets::scan::scrub_field;

pub(in crate::agent) const DEFAULT_STRATEGY: &str = "default";

pub(in crate::agent) const DEFAULT_PLUGIN: &str = "compaction";

#[derive(Debug, Clone, PartialEq, Eq)]
pub(in crate::agent) struct Run {
    pub(in crate::agent) plugin: String,
    pub(in crate::agent) live: bool,
    pub(in crate::agent) compact: CompactInfo,
    pub(in crate::agent) supplied: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(in crate::agent) struct HeadAsk {
    pub(in crate::agent) run: Run,
    pub(in crate::agent) summary: Option<String>,
    pub(in crate::agent) native: bool,
    pub(in crate::agent) before: Option<u64>,
}

impl Agent {
    pub(in crate::agent) fn pin_strategy(&mut self) {
        let chosen = find(names::CONTEXT_STRATEGY)
            .and_then(|spec| self.resolved_at(spec, &Place::Global))
            .map(|value| match value {
                SettingValue::Text(id) if id != DEFAULT_STRATEGY => id,
                _ => DEFAULT_PLUGIN.to_owned(),
            });
        self.compaction.strategy = chosen;
    }

    pub(in crate::agent) fn chosen_strategy(&self) -> Option<&str> {
        self.compaction.strategy.as_deref()
    }

    fn unavailable(&self, id: &str) -> Option<&'static str> {
        let engine = &self.hooks.engine;
        if !engine.has_plugin(id) || self.hooks.switches.removed.contains(id) {
            Some("isn't installed")
        } else if self.plugin_off(id) {
            Some("is off")
        } else if !engine.table().offers_strategy(id) {
            Some("offers no strategy")
        } else {
            None
        }
    }

    pub(in crate::agent) async fn open_strategy(
        &mut self,
        compact: CompactInfo,
        point: &str,
        events: &mpsc::Sender<AgentEvent>,
    ) -> Option<Run> {
        let plugin = self.chosen_strategy()?.to_owned();
        let live = match self.unavailable(&plugin) {
            None => true,
            Some(why) => {
                self.strategy_fallback(&plugin, point, why, events).await;
                false
            }
        };
        Some(Run {
            plugin,
            live,
            compact,
            supplied: None,
        })
    }

    async fn strategy_fallback(
        &mut self,
        plugin: &str,
        point: &str,
        why: &str,
        events: &mpsc::Sender<AgentEvent>,
    ) {
        let ran = Ran {
            hook: strategy_hook(plugin),
            point: point.to_owned(),
            at: self.at(None, None),
            outcome: HookOutcome::Failed {
                message: format!(
                    "the {plugin} plugin {why}, so Wisp used its own compaction layout"
                ),
            },
            ms: 0,
        };
        self.emit_runs(vec![ran], events).await;
    }

    fn unusable(&self, plugin: &str) -> Option<&'static str> {
        self.unavailable(plugin).or_else(|| {
            (self.chosen_strategy() != Some(plugin)).then_some("is no longer the chosen strategy")
        })
    }

    pub(in crate::agent) fn strategy_info(
        &self,
        compact: CompactInfo,
        (epoch, turns): (u64, (u64, u64)),
        native: bool,
    ) -> StrategyInfo {
        let overflow = &self.compaction.overflow;
        let cut_off = (overflow.fell_back && overflow.kept.is_none())
            .then(|| overflow.cut_off.clone())
            .flatten();
        StrategyInfo {
            compact,
            epoch,
            first_turn: turns.0,
            last_turn: turns.1,
            native,
            cut_off,
            verbatim_tokens: self.verbatim_budget(),
        }
    }

    fn head_strategy_info(&self, run: &Run, info: &HeadInfo, native: bool) -> StrategyInfo {
        let turns = (info.first_turn, info.last_turn);
        self.strategy_info(run.compact.clone(), (info.epoch, turns), native)
    }

    pub(in crate::agent) fn history_view(&self) -> HistoryView {
        HistoryView {
            said: self
                .compaction
                .said
                .iter()
                .map(|said| SaidEntry {
                    turn: said.turn,
                    text: said.text.clone(),
                    seq: said.seq,
                })
                .collect(),
            calls: self
                .compaction
                .calls
                .iter()
                .map(|call| CallEntry {
                    turn: call.turn,
                    tool: call.tool.clone(),
                    target: if call.lookup && !self.lookup_is_safe(&call.target) {
                        wisp_tool_text::index::INVALID_TARGET.to_owned()
                    } else {
                        call.target.clone()
                    },
                    status: call.status.clone(),
                    cwd: call.cwd.clone(),
                    saved: call.saved.clone(),
                    attached: call.attached.clone(),
                    withdrawn: call.withdrawn,
                })
                .collect(),
            log_pointer: self.log_pointer(),
            output_pointer: self.output_pointer(),
        }
    }

    fn ask_strategy<'a>(
        &'a mut self,
        run: &'a mut Run,
        asked: (StrategyInfo, Ask),
        commands: Option<&'a mut mpsc::Receiver<ClientCommand>>,
        events: &'a mpsc::Sender<AgentEvent>,
    ) -> Pin<Box<impl Future<Output = Option<Answer>> + 'a>> {
        Box::pin(self.ask_strategy_unboxed(run, asked, commands, events))
    }

    async fn ask_strategy_unboxed(
        &mut self,
        run: &mut Run,
        (info, ask): (StrategyInfo, Ask),
        commands: Option<&mut mpsc::Receiver<ClientCommand>>,
        events: &mpsc::Sender<AgentEvent>,
    ) -> Option<Answer> {
        if !run.live {
            return None;
        }
        let point = ask.point();
        if let Some(why) = self.unusable(&run.plugin) {
            run.live = false;
            self.strategy_fallback(&run.plugin, point, why, events)
                .await;
            return None;
        }
        let written = matches!(ask, Ask::Head(_, Some(_)));
        let cancel = CancellationToken::new();
        let at = self.at(None, None);
        let limits = Limits {
            cancel: cancel.clone(),
            deadline: Some(tokio::time::Instant::now() + STRATEGY_TIMEOUT),
            at: at.clone(),
        };
        self.publish_hook_snapshot();
        let call = self
            .hooks
            .engine
            .call_strategy(&run.plugin, info, ask, limits);
        let started = tokio::time::Instant::now();
        let done = match commands {
            Some(commands) => {
                self.await_render(call, &cancel, Mode::Turn, commands, events)
                    .await
            }
            None => Some(call.await),
        };
        let Some((answer, mut ran)) = done else {
            run.live = false;
            let ran = Ran {
                hook: strategy_hook(&run.plugin),
                point: point.to_owned(),
                at,
                outcome: HookOutcome::Cancelled,
                ms: u64::try_from(started.elapsed().as_millis()).unwrap_or(u64::MAX),
            };
            self.emit_runs(vec![ran], events).await;
            return None;
        };
        let answer = match answer {
            Some(Answer::Head(texts)) if written && texts.is_empty() => {
                ran.outcome = HookOutcome::Failed {
                    message: format!(
                        "the {} strategy gave no head texts, so Wisp used its own compaction layout",
                        run.plugin
                    ),
                };
                None
            }
            other => other,
        };
        self.emit_runs(vec![ran], events).await;
        if answer.is_none() {
            run.live = false;
        }
        answer
    }

    pub(in crate::agent) async fn strategy_instruction(
        &mut self,
        run: &mut Run,
        commands: Option<&mut mpsc::Receiver<ClientCommand>>,
        events: &mpsc::Sender<AgentEvent>,
    ) -> Option<String> {
        let native = self.summary_engine() == CompactionEngine::Native;
        let info = self.strategy_info(
            run.compact.clone(),
            (self.next_epoch(), self.next_turns()),
            native,
        );
        let answer = self
            .ask_strategy(run, (info, Ask::Instruction), commands, events)
            .await;
        let Some(Answer::Instruction(Some(text))) = answer else {
            return None;
        };
        run.supplied = Some(text.clone());
        Some(text)
    }

    pub(in crate::agent) async fn strategy_accept(
        &mut self,
        summary: &str,
        commands: &mut mpsc::Receiver<ClientCommand>,
        events: &mpsc::Sender<AgentEvent>,
    ) -> Option<String> {
        let mut run = self.compaction.strategy_run.take()?;
        let info = self.strategy_info(
            run.compact.clone(),
            (self.next_epoch(), self.next_turns()),
            false,
        );
        let answer = self
            .ask_strategy(
                &mut run,
                (info, Ask::Accept(summary.to_owned())),
                Some(commands),
                events,
            )
            .await;
        let plugin = run.plugin.clone();
        self.compaction.strategy_run = Some(run);
        let Some(Answer::Accept(Err(sentence))) = answer else {
            return None;
        };
        let home = self.secret_check_home();
        let sentence = scrub_field(&sentence, home.as_deref(), &crate::session::read_held());
        if plugin == DEFAULT_PLUGIN {
            Some(sentence)
        } else {
            Some(format!("The {plugin}{STRATEGY_REFUSED}{sentence}"))
        }
    }

    pub(in crate::agent) fn live_strategy(&self) -> bool {
        self.compaction
            .strategy_run
            .as_ref()
            .is_some_and(|r| r.live)
    }

    pub(in crate::agent) async fn strategy_head(
        &mut self,
        ask: &mut HeadAsk,
        info: &HeadInfo,
        commands: &mut mpsc::Receiver<ClientCommand>,
        events: &mpsc::Sender<AgentEvent>,
    ) -> Option<StrategyHead> {
        let strategy = self.head_strategy_info(&ask.run, info, ask.native);
        let view = self.history_view();
        let answer = self
            .ask_strategy(
                &mut ask.run,
                (strategy, Ask::Head(view, ask.summary.clone())),
                Some(commands),
                events,
            )
            .await;
        let Some(Answer::Head(texts)) = answer else {
            return None;
        };
        Some(StrategyHead {
            plugin: ask.run.plugin.clone(),
            texts,
        })
    }

    pub(in crate::agent) fn convert_compact_info(&self, trigger: CompactionTrigger) -> CompactInfo {
        let gate = self.gate(trigger);
        let plain = super::gate::Instruction::plain(None);
        self.compact_info(&gate, None, self.running_turn(), plain.text)
    }

    pub(in crate::agent) fn refresh_strategy(
        &mut self,
        mode: Mode,
        commands: Option<&mut mpsc::Receiver<ClientCommand>>,
        events: &mpsc::Sender<AgentEvent>,
    ) -> Pin<Box<impl Future<Output = ()>>> {
        Box::pin(self.refresh_strategy_unboxed(mode, commands, events))
    }

    async fn refresh_strategy_unboxed(
        &mut self,
        mode: Mode,
        mut commands: Option<&mut mpsc::Receiver<ClientCommand>>,
        events: &mpsc::Sender<AgentEvent>,
    ) {
        let Some(plugin) = self.chosen_strategy().map(str::to_owned) else {
            self.compaction.strategy_estimate = None;
            return;
        };
        let epoch = self.next_epoch();
        let failed =
            self.compaction.strategy_unestimated.as_ref() == Some(&(plugin.clone(), epoch));
        if failed
            || self.unavailable(&plugin).is_some()
            || self.open_track().is_some_and(|t| t.stopped)
        {
            self.compaction.strategy_estimate = None;
            return;
        }
        let native = self.summary_engine() == CompactionEngine::Native;
        let info = self.strategy_info(
            self.convert_compact_info(CompactionTrigger::Auto),
            (epoch, self.next_turns()),
            native,
        );
        self.compaction.strategy_estimate = None;
        self.compaction.strategy_instruction = None;
        self.compaction.strategy_converted = None;
        let view = Ask::Head(self.history_view(), None);
        let Some(head) = self
            .estimate_call(
                &plugin,
                info.clone(),
                view,
                (mode, commands.as_deref_mut(), events),
            )
            .await
        else {
            return;
        };
        let texts = match head {
            Some(Answer::Head(texts)) if native || !texts.is_empty() => texts,
            _ => {
                self.compaction.strategy_unestimated = Some((plugin, epoch));
                return;
            }
        };
        let Some(instruction) = self
            .estimate_call(
                &plugin,
                info.clone(),
                Ask::Instruction,
                (mode, commands.as_deref_mut(), events),
            )
            .await
        else {
            return;
        };
        if let Some(Answer::Instruction(text)) = instruction {
            let tokens = text.map_or_else(super::instruction_tokens, |t| text_tokens(&t));
            self.compaction.strategy_instruction = Some((plugin.clone(), tokens));
        }
        if let Some((converted, summary)) = self.converted_info(&info) {
            let ask = Ask::Head(self.history_view(), Some(summary));
            let Some(answer) = self
                .estimate_call(&plugin, converted, ask, (mode, commands, events))
                .await
            else {
                return;
            };
            if let Some(Answer::Head(texts)) = answer
                && !texts.is_empty()
            {
                self.compaction.strategy_converted = Some((plugin.clone(), epoch, texts));
            }
        }
        self.compaction.strategy_estimate = Some((plugin, epoch, texts));
    }

    fn converted_info(&self, info: &StrategyInfo) -> Option<(StrategyInfo, String)> {
        let old = self.compaction.head.as_ref().filter(|head| head.native)?;
        let turns = self.converted_turns()?;
        let converted = StrategyInfo {
            epoch: old.epoch + 1,
            first_turn: turns.0,
            last_turn: turns.1,
            native: false,
            ..info.clone()
        };
        Some((converted, old.summary.clone()))
    }

    pub(in crate::agent) fn converted_estimate(&self) -> Option<StrategyHead> {
        let (plugin, epoch, texts) = self.compaction.strategy_converted.as_ref()?;
        (*epoch == self.next_epoch() && self.chosen_strategy() == Some(plugin.as_str())).then(
            || StrategyHead {
                plugin: plugin.clone(),
                texts: texts.clone(),
            },
        )
    }

    async fn estimate_call(
        &mut self,
        plugin: &str,
        info: StrategyInfo,
        ask: Ask,
        (mode, commands, events): (
            Mode,
            Option<&mut mpsc::Receiver<ClientCommand>>,
            &mpsc::Sender<AgentEvent>,
        ),
    ) -> Option<Option<Answer>> {
        let cancel = CancellationToken::new();
        let limits = Limits {
            cancel: cancel.clone(),
            deadline: Some(tokio::time::Instant::now() + STRATEGY_TIMEOUT),
            at: At {
                turn: self.running_turn(),
                step: None,
                call_id: None,
            },
        };
        self.publish_hook_snapshot();
        let call = self.hooks.engine.call_strategy(plugin, info, ask, limits);
        let done = match commands {
            Some(commands) => {
                self.await_render(call, &cancel, mode, commands, events)
                    .await
            }
            None => Some(call.await),
        };
        if done.is_none() && mode == Mode::PreTurn && !self.closed {
            Box::pin(self.idle_cancel(events)).await;
        }
        done.map(|(answer, _)| answer)
    }

    pub(in crate::agent) fn instruction_estimate(&self) -> u64 {
        match &self.compaction.strategy_instruction {
            Some((plugin, tokens))
                if self.chosen_strategy() == Some(plugin.as_str())
                    && self.unavailable(plugin).is_none() =>
            {
                *tokens
            }
            _ => super::instruction_tokens(),
        }
    }

    pub(in crate::agent) fn strategy_estimate(&self) -> Option<&[String]> {
        let (plugin, epoch, texts) = self.compaction.strategy_estimate.as_ref()?;
        if *epoch != self.next_epoch() {
            return None;
        }
        (self.chosen_strategy() == Some(plugin.as_str())).then_some(texts.as_slice())
    }

    pub(in crate::agent) fn layout_figures(
        &self,
        texts: &HeadTexts,
        (epoch, turns): (u64, (u64, u64)),
        summary: Option<&str>,
    ) -> (u64, u64) {
        let core = || text_tokens(&self.compacted_context(epoch, turns, summary));
        let rendered = match &texts.strategy {
            Some(strategy) => strategy
                .texts
                .iter()
                .map(|text| text_tokens(text))
                .fold(0, u64::saturating_add),
            None => core(),
        };
        let estimated = match summary {
            Some(_) => core(),
            None => self.layout_tokens(epoch, turns),
        };
        (rendered, estimated)
    }

    pub(in crate::agent) fn layout_tokens(&self, epoch: u64, turns: (u64, u64)) -> u64 {
        match self.strategy_estimate() {
            Some(texts) => texts
                .iter()
                .map(|text| text_tokens(text))
                .fold(0, u64::saturating_add),
            None => text_tokens(&self.compacted_context(epoch, turns, None)),
        }
    }
}
