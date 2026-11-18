use tokio::sync::mpsc;
use wisp_plugin_api::hooks::{Event, FillInfo};
use wisp_protocol::{AgentEvent, ClientCommand};

use super::auto::{Due, Early, Figures};
use super::gate::{Asked, Gate};
use crate::agent::Agent;
use crate::plugin::hooks::points::CONTEXT_THRESHOLD;
use crate::plugin::hooks::{Fire, What};

pub(super) enum Ask {
    Due(Due, Option<String>),
    Request(Due, Asked),
}

impl Ask {
    pub(super) fn settle(&self) -> (Due, bool) {
        match self {
            Self::Due(due, _) => (*due, false),
            Self::Request(due, _) => (*due, true),
        }
    }

    pub(super) fn gate(self, estimate: u64) -> Gate {
        match self {
            Self::Due(due, focus) => Gate::auto(estimate, due).focused(focus),
            Self::Request(due, asked) => Gate::auto(estimate, due).requested(asked),
        }
    }
}

impl Agent {
    pub(super) async fn ask_threshold(
        &mut self,
        estimate: u64,
        due: Option<Due>,
        before_turn: bool,
        commands: &mut mpsc::Receiver<ClientCommand>,
        events: &mpsc::Sender<AgentEvent>,
    ) -> Result<Option<Ask>, Early> {
        let Some(figures) = self.figures(before_turn) else {
            return Ok(due.map(|due| Ask::Due(due, None)));
        };
        if !self.has_new_content() || self.track().stopped {
            return Ok(due.map(|due| Ask::Due(due, None)));
        }
        let info = self.fill_info(estimate, figures, before_turn);
        let step = (!before_turn).then_some(info.step);
        let at = self.at(step, None);
        let refusal = if due.is_some() { None } else { self.refusal() };
        let fire = Fire::new(
            CONTEXT_THRESHOLD,
            What::Event(Event::ContextThreshold(info)),
            at,
        )
        .refusing(refusal);
        let progress = self.await_hooks(fire, commands, events).await;
        if self.track().stopped {
            return Err(Early::Cancelled);
        }
        let asked = progress.compact.map(|(hook, focus)| Asked { hook, focus });
        Ok(match (due, asked) {
            (Some(due), asked) => Some(Ask::Due(due, asked.and_then(|a| a.focus))),
            (None, Some(asked)) => Some(Ask::Request(figures.requested(), asked)),
            (None, None) => None,
        })
    }

    fn fill_info(&mut self, estimate: u64, figures: Figures, before_turn: bool) -> FillInfo {
        let auto_compact = if before_turn {
            self.next_auto_compact()
        } else {
            self.auto_compact()
        };
        let track = self.track();
        let (turn, step) = (track.index, track.step);
        FillInfo {
            turn,
            step,
            before_turn,
            estimate,
            measured: self.last.as_ref().map(|last| last.used),
            window: Some(figures.window),
            point: figures.point,
            ceiling: Some(figures.ceiling),
            auto_compact,
            epoch: self.epoch_number(),
        }
    }
}
