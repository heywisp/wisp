use tokio::sync::mpsc;
use wisp_protocol::log::{Cut, LogEvent};
use wisp_protocol::{AgentEvent, ContentBlock, Message, RenderingForm};

use super::epoch::MarkReason;
use super::search::holds_thinking_from;
use super::{Agent, Failure};
use crate::provider::forms::Layout;
use crate::provider::{Provider, Refused, ServerCalls};

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct Trial {
    pub(super) form: RenderingForm,
    pub(super) cut: Option<Cut>,
}

pub(super) fn echoed_positions(
    provider: &dyn Provider,
    content: &[ContentBlock],
    earlier: &ServerCalls,
) -> Vec<usize> {
    let echoed = provider.echoed(content, earlier);
    content
        .iter()
        .enumerate()
        .filter(|(_, b)| echoed.iter().any(|e| std::ptr::eq(*e, *b)))
        .map(|(i, _)| i)
        .collect()
}

impl Agent {
    pub(super) fn carries_rendered(&self, messages: &[Message]) -> bool {
        let rendering = self.rendering(messages.len());
        let provider = self.provider().as_ref();
        Layout::new(messages, &rendering, |content, earlier| {
            echoed_positions(provider, content, earlier)
        })
        .carries_rendered()
    }

    pub(super) fn recover(
        &mut self,
        failure: Box<Failure>,
        original: &mut Option<Box<Failure>>,
        messages: &[Message],
    ) -> Option<Box<Failure>> {
        if failure.error.refused != Some(Refused::Rendering) {
            return Some(failure);
        }
        if self.escalate(messages) {
            if original.is_none() {
                *original = Some(failure);
            }
            return None;
        }
        self.search.trial = None;
        Some(original.take().unwrap_or(failure))
    }

    fn escalate(&mut self, messages: &[Message]) -> bool {
        let current = self
            .search
            .trial
            .as_ref()
            .map_or(self.search.form, |t| t.form);
        let Some(form) = current.next() else {
            return false;
        };
        let keeps = self.keeps_thinking();
        let kept = self.kept_rendering(messages.len());
        let provider = self.provider().as_ref();
        let layout = Layout::new(messages, &kept, |content, earlier| {
            echoed_positions(provider, content, earlier)
        });
        let cut = layout
            .first_change(messages, &kept, form)
            .map(|from| self.to_history(from))
            .zip(self.history.len().checked_sub(1))
            .filter(|(from, through)| !keeps && holds_thinking_from(&self.history, *from, *through))
            .map(|(from, through)| Cut {
                from,
                through,
                tools: false,
            });
        self.search.trial = Some(Trial { form, cut });
        true
    }

    pub(super) async fn keep_form(&mut self, events: &mpsc::Sender<AgentEvent>) {
        let Some(trial) = self.search.trial.take() else {
            return;
        };
        self.epoch.rebuilt(MarkReason::RenderingForm);
        self.log(events, LogEvent::RenderingForm { form: trial.form })
            .await;
        if let Some(cut) = trial.cut {
            self.record_cut(cut, None, events).await;
        }
        self.search.form = trial.form;
        self.log_mark(events).await;
    }
}
