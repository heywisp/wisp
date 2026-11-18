use tokio::sync::mpsc;
use wisp_plugin_api::hooks::{BeforeEnd, Event};
use wisp_protocol::{
    AgentEvent, ClientCommand, ContentBlock, HookChange, Message, Role, StopReason,
};

use super::super::{Agent, Ending};
use crate::plugin::hooks::points::BEFORE_END;
use crate::plugin::hooks::{Fire, What};
use crate::provider::reply_text;

impl Agent {
    fn last_reply_text(&self) -> String {
        self.history
            .last()
            .filter(|m| m.role == Role::Assistant)
            .and_then(|m| reply_text(&m.content))
            .unwrap_or_default()
    }

    pub(in crate::agent) async fn before_end(
        &mut self,
        end: &Ending,
        commands: &mut mpsc::Receiver<ClientCommand>,
        events: &mpsc::Sender<AgentEvent>,
    ) -> bool {
        let ends_turn = matches!(
            end.stop_reason,
            StopReason::EndTurn | StopReason::StopSequence
        );
        if !end.final_reply
            || !end.committed
            || !ends_turn
            || self.track().stopped
            || !self.hooks.engine.attached(BEFORE_END)
        {
            return false;
        }
        let (turn, step, continues) = {
            let track = self.track();
            (track.index, track.step, track.continued)
        };
        let event = Event::BeforeEnd(BeforeEnd {
            turn,
            step,
            reply: self.last_reply_text(),
            continues,
        });
        let at = self.at(Some(step), None);
        let fire = Fire::new(BEFORE_END, What::Event(event), at.clone());
        let progress = self.await_hooks(fire, commands, events).await;
        if self.track().stopped {
            return false;
        }
        let Some((hook, text)) = progress.continued else {
            return false;
        };
        self.track().continued += 1;
        let message = Message {
            role: Role::User,
            content: vec![ContentBlock::Text { text: text.clone() }],
        };
        self.history.push(message.clone());
        self.sync_history();
        self.commit_record(message, None, None, events).await;
        let applied = AgentEvent::HookApplied {
            hook,
            point: BEFORE_END.to_owned(),
            turn: at.turn,
            step: at.step,
            call_id: None,
            input_id: None,
            change: HookChange::Continue { text },
        };
        self.emit(events, applied).await;
        true
    }
}
