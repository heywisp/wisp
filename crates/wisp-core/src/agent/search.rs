use std::borrow::Borrow;
use std::collections::{BTreeMap, BTreeSet};

use tokio::sync::mpsc;
use wisp_protocol::log::{BlockPosition, Closed, Cut, LogEvent};
use wisp_protocol::{
    AgentEvent, ContentBlock, Message, RenderingForm, Role, SearchEnd, UnfinishedReason,
};

use super::Agent;
use super::epoch::MarkReason;
use super::forms::Trial;
use crate::provider::forms::thinking_shaped;
use crate::provider::forms::{server_call, server_result};
use crate::provider::{Provider, Rendering, ServerCalls};

pub const MAX_CONTINUATIONS: u32 = 5;

pub(super) const WHOLE: BlockPosition = BlockPosition {
    message: 0,
    block: 0,
};

#[derive(Debug, Clone, Default)]
pub(super) struct SearchState {
    pub(super) paused: BTreeSet<usize>,
    pub(super) closed: Closed,
    pub(super) cuts: Vec<Cut>,
    pub(super) endpoints: Vec<String>,
    pub(super) rendered_on: BTreeSet<String>,
    pub(super) rendered_through: BTreeMap<String, usize>,
    pub(super) unreadable: BTreeMap<String, usize>,
    pub(super) marker_dropped: BTreeSet<String>,
    pub(super) failed_tail: Option<BTreeSet<String>>,
    pub(super) form: RenderingForm,
    pub(super) trial: Option<Trial>,
    pub(super) refused: BTreeSet<String>,
    pub(super) kept: BTreeMap<(String, String), bool>,
    pub(super) probe: Option<Probe>,
    pub(super) built_with: Option<String>,
    pub(super) logged_tools: Option<String>,
    pub(super) sent_tools: Option<Vec<String>>,
    pub(super) not_run: BTreeSet<String>,
    pub(super) kept_break: Option<usize>,
    pub(super) drops_covered: bool,
    pub(super) notes: BTreeSet<(String, String)>,
    pub(super) notes_trial: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct Probe {
    pub(super) endpoint: String,
    pub(super) built_with: Option<String>,
    pub(super) epoch: super::epoch::Epoch,
}

impl SearchState {
    pub(super) fn sync(&mut self, history: &[Message], endpoint: &str) {
        while self.endpoints.len() < history.len() {
            self.endpoints.push(endpoint.to_owned());
        }
    }

    pub(super) fn rewound_to(
        &mut self,
        messages: usize,
        cuts: usize,
        (closed, failed_tail): (Closed, Option<BTreeSet<String>>),
    ) {
        self.paused.retain(|i| *i < messages);
        self.closed = closed;
        let later = self.cuts.split_off(cuts.min(self.cuts.len()));
        let last = messages.saturating_sub(1);
        self.cuts.extend(
            later
                .into_iter()
                .filter(|cut| (cut.tools || cut.from == WHOLE) && cut.from.message < messages)
                .map(|cut| Cut {
                    through: cut.through.min(last),
                    ..cut
                }),
        );
        self.endpoints.truncate(messages);
        for through in self
            .rendered_through
            .values_mut()
            .chain(self.unreadable.values_mut())
        {
            *through = (*through).min(messages);
        }
        self.failed_tail = failed_tail;
        self.trial = None;
    }
}

pub(super) fn earlier_calls(
    provider: &dyn Provider,
    messages: &[Message],
    upto: usize,
) -> ServerCalls {
    let mut earlier = ServerCalls::new();
    for message in messages.iter().take(upto) {
        if message.role != Role::Assistant {
            continue;
        }
        let sent: Vec<String> = provider
            .echoed(&message.content, &earlier)
            .into_iter()
            .filter_map(server_call)
            .map(str::to_owned)
            .collect();
        earlier.extend(sent);
    }
    earlier
}

pub(super) fn live_tail<M: Borrow<Message>>(history: &[M]) -> Option<usize> {
    let last = history.len().checked_sub(1)?;
    let message = history.get(last)?.borrow();
    match message.role {
        Role::Assistant => Some(last),
        Role::User
            if !message.content.is_empty()
                && message
                    .content
                    .iter()
                    .all(|b| matches!(b, ContentBlock::ToolResult { .. })) =>
        {
            let before = last.checked_sub(1)?;
            (history.get(before)?.borrow().role == Role::Assistant).then_some(before)
        }
        _ => None,
    }
}

pub(super) fn tail_run<M: Borrow<Message>>(
    history: &[M],
) -> Option<std::ops::RangeInclusive<usize>> {
    let last = live_tail(history)?;
    let mut first = last;
    while first
        .checked_sub(1)
        .and_then(|i| history.get(i))
        .is_some_and(|m| m.borrow().role == Role::Assistant)
    {
        first -= 1;
    }
    Some(first..=last)
}

pub(super) fn open_calls(
    provider: &dyn Provider,
    history: &[Message],
    closed: &Closed,
) -> Vec<(BlockPosition, String)> {
    let Some(run) = tail_run(history) else {
        return Vec::new();
    };
    let (first, tail) = (*run.start(), *run.end());
    let mut earlier = earlier_calls(provider, history, first);
    let mut echoes = Vec::new();
    for (index, message) in history.iter().enumerate().take(tail + 1).skip(first) {
        let echoed = provider.echoed(&message.content, &earlier);
        earlier.extend(
            echoed
                .iter()
                .filter_map(|b| server_call(b))
                .map(str::to_owned),
        );
        echoes.push((index, message, echoed));
    }
    let answered: BTreeSet<&str> = echoes
        .iter()
        .flat_map(|(_, _, echoed)| echoed.iter().filter_map(|b| server_result(b)))
        .collect();
    echoes
        .iter()
        .flat_map(|(index, message, echoed)| {
            echoed.iter().filter_map(|block| {
                let id = server_call(block)?;
                if answered.contains(id) || closed.calls.contains(id) {
                    return None;
                }
                let position = message
                    .content
                    .iter()
                    .position(|b| std::ptr::eq(b, *block))?;
                let at = BlockPosition {
                    message: *index,
                    block: position,
                };
                Some((at, id.to_owned()))
            })
        })
        .collect()
}

pub(super) fn paused_tail(history: &[Message], paused: &BTreeSet<usize>, closed: &Closed) -> bool {
    let Some(last) = history.len().checked_sub(1) else {
        return false;
    };
    history.get(last).is_some_and(|m| m.role == Role::Assistant)
        && paused.contains(&last)
        && !closed.tails.contains(&last)
}

pub(super) fn holds_thinking_from(
    history: &[Message],
    from: BlockPosition,
    through: usize,
) -> bool {
    history
        .iter()
        .enumerate()
        .skip(from.message)
        .take_while(|(i, _)| *i <= through)
        .any(|(i, m)| {
            m.content.iter().enumerate().any(|(b, block)| {
                BlockPosition {
                    message: i,
                    block: b,
                } >= from
                    && thinking_shaped(block)
            })
        })
}

impl Agent {
    pub(super) async fn record_cut(
        &mut self,
        cut: Cut,
        tools_sha256: Option<String>,
        events: &mpsc::Sender<AgentEvent>,
    ) {
        let record = LogEvent::ThinkingCut {
            from: cut.from,
            through: cut.through,
            tools_sha256,
        };
        self.log(events, record).await;
        self.search.cuts.push(cut);
    }

    pub(super) fn open_calls(&self) -> Vec<String> {
        open_calls(self.provider().as_ref(), &self.history, &self.search.closed)
            .into_iter()
            .map(|(_, id)| id)
            .collect()
    }

    pub(super) fn paused_tail(&self) -> bool {
        paused_tail(&self.history, &self.search.paused, &self.search.closed)
    }

    pub(super) fn tail_live(&self) -> bool {
        self.paused_tail() || !self.open_calls().is_empty()
    }

    pub(super) fn sync_history(&mut self) {
        let endpoint = self.endpoint_name().to_owned();
        self.search.sync(&self.history, &endpoint);
        self.sync_fill();
    }

    pub(super) fn paused_flags(&self, len: usize) -> Vec<bool> {
        (0..len)
            .map(|i| {
                self.full_index(i)
                    .is_some_and(|i| self.search.paused.contains(&i))
            })
            .collect()
    }

    pub(super) fn rendering(&self, len: usize) -> Rendering {
        let mut rendering = self.kept_rendering(len);
        rendering.attachments_as_notes |= self.search.notes_trial;
        if let Some(trial) = &self.search.trial {
            rendering.form = trial.form;
            rendering
                .cuts
                .extend(trial.cut.and_then(|c| self.view_cut(c)));
        }
        if let Some((cut, _)) = self.whole_cut() {
            rendering.cuts.extend(self.view_cut(cut));
        }
        rendering
            .cuts
            .extend(self.rendering_cut().and_then(|c| self.view_cut(c)));
        rendering
    }

    pub(super) fn kept_rendering(&self, len: usize) -> Rendering {
        let current = self.endpoint_name();
        let as_text = self.search.rendered_on.contains(current);
        let unreadable = if crate::search::by_call(self.step_server_tools()) {
            self.history.len()
        } else {
            self.search.unreadable.get(current).copied().unwrap_or(0)
        };
        Rendering {
            closed: self.search.closed.calls.clone(),
            cuts: self
                .search
                .cuts
                .iter()
                .filter_map(|c| self.view_cut(*c))
                .collect(),
            native_as_text: (0..len)
                .map(|i| {
                    let full = self.full_index(i);
                    full.is_some_and(|i| i < unreadable)
                        || as_text
                            && full
                                .and_then(|i| self.search.endpoints.get(i))
                                .is_some_and(|e| e != current)
                })
                .collect(),
            paused: self.paused_flags(len),
            drop_base_marker: self.search.marker_dropped.contains(current),
            search_only: false,
            form: self.search.form,
            provisional: false,
            sends_media: false,
            attachments_as_notes: self.kept_notes(),
            tools_point: self.view_index(self.tools_point()),
            array: self.sent.iter().map(|s| s.name.clone()).collect(),
        }
    }

    pub(super) async fn close_tail(
        &mut self,
        reason: UnfinishedReason,
        billed: &BTreeSet<String>,
        events: &mpsc::Sender<AgentEvent>,
    ) {
        self.close_tail_records(reason, billed, events).await;
        self.log_mark(events).await;
    }

    pub(super) async fn close_tail_records(
        &mut self,
        reason: UnfinishedReason,
        billed: &BTreeSet<String>,
        events: &mpsc::Sender<AgentEvent>,
    ) {
        let open = open_calls(self.provider().as_ref(), &self.history, &self.search.closed);
        let paused = self.paused_tail();
        if open.is_empty() && !paused {
            return;
        }
        let Some(through) = self.history.len().checked_sub(1) else {
            return;
        };
        let first = open
            .iter()
            .map(|(at, _)| at.message)
            .min()
            .unwrap_or(through);
        let cut_from = open
            .iter()
            .map(|(at, _)| *at)
            .min()
            .filter(|from| holds_thinking_from(&self.history, *from, through))
            .filter(|_| !self.keeps_thinking());
        let mark = match cut_from {
            Some(_) => MarkReason::ThinkingCut,
            None => MarkReason::TailClosed,
        };
        self.epoch.changed_from(self.view_index(first), mark);
        let (step, attempt) = self
            .track
            .as_ref()
            .map_or((0, 0), |t| (t.step, t.step_retries));
        let calls: Vec<String> = open.iter().map(|(_, id)| id.clone()).collect();
        for call_id in &calls {
            let event = AgentEvent::SearchEnded {
                step,
                attempt,
                call_id: call_id.clone(),
                outcome: SearchEnd::Unfinished {
                    reason,
                    possibly_billed: billed.contains(call_id),
                },
            };
            self.emit(events, event).await;
        }
        let record = LogEvent::TailClosed {
            through,
            calls: calls.clone(),
            reason,
        };
        self.log(events, record).await;
        if let Some(from) = cut_from {
            let cut = Cut {
                from,
                through,
                tools: false,
            };
            self.record_cut(cut, None, events).await;
        }
        self.search.closed.calls.extend(calls);
        self.search.closed.tails.insert(through);
    }

    pub(super) async fn close_failed_tail(&mut self, events: &mpsc::Sender<AgentEvent>) {
        let Some(billed) = self.search.failed_tail.take() else {
            return;
        };
        self.close_tail(UnfinishedReason::Failed, &billed, events)
            .await;
    }

    pub(super) fn ends_in_closed_pause(&self) -> bool {
        let Some(last) = self.history.len().checked_sub(1) else {
            return false;
        };
        self.history
            .get(last)
            .is_some_and(|m| m.role == Role::Assistant)
            && self.search.paused.contains(&last)
            && self.search.closed.tails.contains(&last)
    }

    pub(super) fn closed_tail_flags(&self, len: usize) -> Vec<bool> {
        (0..len)
            .map(|i| {
                self.full_index(i)
                    .is_some_and(|i| self.search.closed.tails.contains(&i))
            })
            .collect()
    }
}

#[cfg(test)]
mod tests;
