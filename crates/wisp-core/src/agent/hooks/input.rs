use tokio::sync::mpsc;
use wisp_plugin_api::hooks::{Event, UserInput};
use wisp_protocol::{
    AgentEvent, ClientCommand, HookChange, InputId, RejectKind, TurnOrigin, WithdrawnBy,
};

use super::super::Agent;
use super::super::inbox::{Input, TurnEnd};
use super::Held;
use super::calls::blocked_text;
use crate::plugin::caller::At;
use crate::plugin::hooks::points::USER_INPUT;
use crate::plugin::hooks::{Change, Fire, What};

pub(in crate::agent) const STOPPED_BEFORE_TURN: &str = "stopped before the turn started";

fn event(input: &Input, steer: bool) -> Event {
    Event::UserInput(UserInput {
        text: input.text.clone(),
        images: u32::try_from(input.refs.len()).unwrap_or(u32::MAX),
        steer,
    })
}

impl Agent {
    pub(in crate::agent) fn user_input_attached(&self) -> bool {
        self.hooks.engine.attached(USER_INPUT)
    }

    async fn applied_text(
        &mut self,
        at: &At,
        applied: Vec<(InputId, Change)>,
        events: &mpsc::Sender<AgentEvent>,
    ) {
        for (id, change) in applied {
            let event = AgentEvent::HookApplied {
                hook: change.hook,
                point: USER_INPUT.to_owned(),
                turn: at.turn,
                step: None,
                call_id: None,
                input_id: Some(id),
                change: HookChange::Text {
                    before: change.before,
                    after: change.after,
                },
            };
            self.emit(events, event).await;
        }
    }

    async fn block_input(
        &mut self,
        id: InputId,
        (hook, reason): (String, String),
        events: &mpsc::Sender<AgentEvent>,
    ) -> String {
        let text = blocked_text(&hook, &reason);
        self.withdrawn(vec![id], WithdrawnBy::Hook { hook, reason }, events)
            .await;
        text
    }

    pub(in crate::agent) async fn take_inputs(
        &mut self,
        inputs: Vec<Input>,
        origin: Option<TurnOrigin>,
        commands: &mut mpsc::Receiver<ClientCommand>,
        events: &mpsc::Sender<AgentEvent>,
    ) -> Option<Vec<Input>> {
        if !self.user_input_attached() || inputs.iter().all(Input::is_mail) {
            return Some(inputs);
        }
        let mut kept = Vec::with_capacity(inputs.len());
        let mut applied = Vec::new();
        let mut blocked: Option<String> = None;
        let mut rest = inputs.into_iter();
        let mut stopped = false;
        for mut input in rest.by_ref() {
            if input.is_mail() {
                kept.push(input);
                continue;
            }
            let fire = Fire::new(USER_INPUT, What::Event(event(&input, false)), At::default());
            let (progress, stop) = self.await_pre_turn(fire, commands, events).await;
            if stop {
                kept.push(input);
                stopped = true;
                break;
            }
            if let Some(block) = progress.blocked {
                let text = self.block_input(input.id, block, events).await;
                blocked.get_or_insert(text);
                continue;
            }
            if let Some(value) = progress.value.filter(|_| !progress.changes.is_empty()) {
                applied.extend(progress.changes.into_iter().map(|c| (input.id.clone(), c)));
                input.text = value;
            }
            kept.push(input);
        }
        if stopped {
            kept.extend(rest);
            self.stop_before_turn(kept, events).await;
            return None;
        }
        self.applied_text(&At::default(), applied, events).await;
        let users = kept.iter().any(|i| !i.is_mail());
        match blocked {
            Some(reason) if !users && origin.is_none() => {
                self.inbox.restore_mail(kept);
                let event = AgentEvent::Rejected {
                    reason,
                    id: None,
                    kind: Some(RejectKind::InputBlocked),
                    request: None,
                };
                self.emit(events, event).await;
                None
            }
            _ => Some(kept),
        }
    }

    async fn stop_before_turn(&mut self, inputs: Vec<Input>, events: &mpsc::Sender<AgentEvent>) {
        self.cancel_commands(None, events).await;
        let held = std::mem::take(&mut self.hooks.held);
        self.hooks.pre_turn = true;
        for entry in held {
            match entry {
                Held::Command(
                    arrival,
                    command @ (ClientCommand::Send { .. } | ClientCommand::Release),
                ) => {
                    self.inbox.replay_as(arrival);
                    self.on_command(Some(command), events).await;
                }
                Held::Reply(reply) if self.closed => self.cancel_held(reply, events).await,
                Held::Reply(reply) => self.place_held(reply, events).await,
                other => self.hooks.held.push_back(other),
            }
        }
        self.hooks.pre_turn = false;
        let (mail, users): (Vec<Input>, Vec<Input>) = inputs.into_iter().partition(Input::is_mail);
        self.inbox.requeue_mail(mail);
        let ids = users.into_iter().map(|i| i.id).collect();
        self.settle_inputs(TurnEnd::Stopped, ids, events).await;
        self.end_wakes(TurnEnd::Stopped);
        let event = AgentEvent::Rejected {
            reason: STOPPED_BEFORE_TURN.to_owned(),
            id: None,
            kind: Some(RejectKind::InputCancelled),
            request: None,
        };
        self.emit(events, event).await;
    }

    pub(in crate::agent) async fn replay_held(&mut self, events: &mpsc::Sender<AgentEvent>) {
        while let Some(entry) = self.hooks.held.pop_front() {
            match entry {
                Held::Command(arrival, command) => {
                    self.inbox.replay_as(arrival);
                    self.on_command(Some(command), events).await;
                }
                Held::Reply(reply) => self.place_held(reply, events).await,
            }
        }
    }

    pub(in crate::agent) async fn take_steers(
        &mut self,
        steers: Vec<Input>,
        commands: &mut mpsc::Receiver<ClientCommand>,
        events: &mpsc::Sender<AgentEvent>,
    ) -> (Vec<Input>, Vec<(InputId, Change)>) {
        if !self.user_input_attached() || steers.iter().all(Input::is_mail) {
            return (steers, Vec::new());
        }
        let mut kept = Vec::with_capacity(steers.len());
        let mut applied = Vec::new();
        let mut rest = steers.into_iter();
        for mut input in rest.by_ref() {
            if input.is_mail() {
                kept.push(input);
                continue;
            }
            let at = self.at(None, None);
            let fire = Fire::new(USER_INPUT, What::Event(event(&input, true)), at);
            let progress = self.await_hooks(fire, commands, events).await;
            if self.track().stopped {
                kept.push(input);
                break;
            }
            if let Some(block) = progress.blocked {
                self.block_input(input.id, block, events).await;
                continue;
            }
            if let Some(value) = progress.value.filter(|_| !progress.changes.is_empty()) {
                applied.extend(progress.changes.into_iter().map(|c| (input.id.clone(), c)));
                input.text = value;
            }
            kept.push(input);
        }
        kept.extend(rest);
        (kept, applied)
    }

    pub(in crate::agent) async fn steers_applied(
        &mut self,
        applied: Vec<(InputId, Change)>,
        events: &mpsc::Sender<AgentEvent>,
    ) {
        let at = self.at(None, None);
        self.applied_text(&at, applied, events).await;
    }
}
