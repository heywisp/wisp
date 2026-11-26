use std::collections::VecDeque;
use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;

use tokio::sync::mpsc;
use tokio::time::Instant;
use tokio_util::sync::CancellationToken;
use wisp_contain::{Contained, contain};
use wisp_plugin_api::commands::CommandReply;
use wisp_plugin_api::hooks::{CommandInfo, Event};
use wisp_protocol::log::{LogEvent, LogRecord};
use wisp_protocol::{
    AgentEvent, ClientCommand, CommandOutcome, CommandRefusal, CommandWait, DeliveryMode, InputId,
    RejectKind, RequestId, Secret,
};
use wisp_tool_text::STEER_NOTE;

use super::super::Agent;
use super::super::choice::SENT_AFTER;
use super::super::hooks::{Busy, Deferred, Held};
use super::super::inbox::{Input, MAX_WAITING, reason};
use super::Done;
use crate::plugin::caller::Caller;
use crate::plugin::commands::PluginCommand;
use crate::plugin::hooks::points::COMMAND;
use crate::plugin::panicked;
use crate::plugin::registry::off_hint;

pub(in crate::agent) const REPLY_PREFIX: &str = "wisp-command-";

#[derive(Debug, Clone)]
pub(in crate::agent) struct Asked {
    request: RequestId,
    command: Arc<PluginCommand>,
    args: Secret,
    pub(in crate::agent) arrival: u64,
}

impl Asked {
    pub(in crate::agent) fn plugin(&self) -> &str {
        &self.command.plugin
    }
}

#[derive(Debug)]
pub(in crate::agent) enum Ran {
    Reply(CommandReply),
    Failed(String),
    TimedOut,
    Cancelled,
}

#[derive(Debug, Clone)]
pub(in crate::agent) struct Reply {
    asked: Asked,
    text: String,
}

struct Running {
    asked: Asked,
    generation: u64,
    cancel: CancellationToken,
}

#[derive(Default)]
pub(in crate::agent) struct PluginCommands {
    waiting: VecDeque<Asked>,
    running: Option<Running>,
    generation: u64,
    replies: u64,
}

impl PluginCommands {
    pub(in crate::agent) fn is_running(&self) -> bool {
        self.running.is_some()
    }

    fn busy(&self) -> bool {
        self.running.is_some() || !self.waiting.is_empty()
    }

    fn take_running(&mut self, generation: u64) -> Option<Asked> {
        self.running
            .take_if(|r| r.generation == generation)
            .map(|r| r.asked)
    }

    pub(in crate::agent) fn seed(&mut self, records: &[LogRecord]) {
        let accepted = records.iter().filter_map(|r| match &r.event {
            LogEvent::InputAccepted { id, .. } => Some(id.as_str()),
            _ => None,
        });
        self.replies = self
            .replies
            .max(InputId::highest_numbered(REPLY_PREFIX, accepted));
    }

    fn next_reply(&mut self) -> InputId {
        self.replies += 1;
        InputId(format!("{REPLY_PREFIX}{}", self.replies))
    }
}

fn wait_of(busy: Busy) -> CommandWait {
    match busy {
        Busy::Hook => CommandWait::Hook,
        Busy::Call => CommandWait::Call,
    }
}

fn refused(kind: CommandRefusal, hint: String) -> CommandOutcome {
    CommandOutcome::Refused { kind, hint }
}

fn spawn_run(
    asked: &Asked,
    generation: u64,
    cancel: CancellationToken,
    tx: mpsc::UnboundedSender<Done>,
) {
    let deadline = Instant::now().checked_add(asked.command.timeout);
    let command = asked.command.clone();
    let args = asked.args.clone();
    let caller = Caller::call(cancel.clone(), deadline).allow_private_write();
    tokio::spawn(caller.scope(async move {
        let ran = run(&command, args.expose(), &cancel, deadline).await;
        drop(tx.send(Done::Plugin { generation, ran }));
    }));
}

async fn run(
    command: &PluginCommand,
    args: &str,
    cancel: &CancellationToken,
    deadline: Option<Instant>,
) -> Ran {
    let Ok(future) = contain(|| command.instance.run_command(&command.name, args)) else {
        return Ran::Failed(panicked(&command.plugin));
    };
    let late = || deadline.is_some_and(|at| Instant::now() >= at);
    tokio::select! {
        biased;
        done = Contained::new(future) => match done {
            Ok(_) if late() => Ran::TimedOut,
            Ok(reply) => Ran::Reply(reply),
            Err(_) => Ran::Failed(panicked(&command.plugin)),
        },
        () = cancel.cancelled() => Ran::Cancelled,
        () = tokio::time::sleep_until(deadline.unwrap_or_else(Instant::now)), if deadline.is_some() => Ran::TimedOut,
    }
}

impl Agent {
    fn off_refusal(&self, plugin: &str) -> Option<String> {
        self.plugin_off(plugin)
            .then(|| off_hint(plugin, self.hooks.switches.removed.contains(plugin)))
    }

    fn turn_running(&self) -> bool {
        self.track.is_some() || self.compacting() || self.hooks.pre_turn || self.hooks.holding
    }

    pub(in crate::agent) async fn plugin_command(
        &mut self,
        (name, args, request): (String, Secret, RequestId),
        arrival: u64,
        events: &mpsc::Sender<AgentEvent>,
    ) {
        let Some(command) = self.hooks.commands.find(&name) else {
            let removed = self.catalog.plugins.iter().find(|p| {
                !p.installed
                    && p.commands
                        .iter()
                        .flat_map(|c| std::iter::once(&c.name).chain(&c.aliases))
                        .any(|n| format!("{}:{n}", p.id) == name)
            });
            let outcome = match removed {
                Some(plugin) => refused(CommandRefusal::PluginOff, off_hint(&plugin.id, true)),
                None => refused(
                    CommandRefusal::Unknown,
                    format!("there is no command /{name}"),
                ),
            };
            self.emit(
                events,
                AgentEvent::CommandDone {
                    answers: request,
                    outcome,
                },
            )
            .await;
            return;
        };
        if let Some(hint) = self.off_refusal(&command.plugin) {
            let outcome = refused(CommandRefusal::PluginOff, hint);
            self.emit(
                events,
                AgentEvent::CommandDone {
                    answers: request,
                    outcome,
                },
            )
            .await;
            return;
        }
        if let Some(hint) = command
            .between_turns
            .clone()
            .filter(|_| self.turn_running())
        {
            let outcome = refused(CommandRefusal::Busy, hint);
            self.emit(
                events,
                AgentEvent::CommandDone {
                    answers: request,
                    outcome,
                },
            )
            .await;
            return;
        }
        let asked = Asked {
            request,
            command,
            args,
            arrival,
        };
        match self.hooks.busy_with {
            Some(busy) => {
                self.hooks.deferred.push_back(Deferred::Command(asked));
                let until = wait_of(busy);
                self.emit(events, AgentEvent::CommandDeferred { request, until })
                    .await;
            }
            None => {
                self.start_deferred(events).await;
                self.enter_lane(asked, events).await;
            }
        }
    }

    pub(in crate::agent) async fn enter_lane(
        &mut self,
        asked: Asked,
        events: &mpsc::Sender<AgentEvent>,
    ) {
        let waits = self.queue.lane.busy();
        let request = asked.request;
        self.queue.lane.waiting.push_back(asked);
        if waits {
            let until = CommandWait::Command;
            self.emit(events, AgentEvent::CommandDeferred { request, until })
                .await;
        } else {
            self.start_lane(events).await;
        }
    }

    pub(in crate::agent) fn start_deferred<'a>(
        &'a mut self,
        events: &'a mpsc::Sender<AgentEvent>,
    ) -> Pin<Box<impl Future<Output = ()> + 'a>> {
        Box::pin(async move {
            if self.hooks.busy_with.is_some() {
                return;
            }
            let mut asked = Vec::new();
            self.hooks.deferred.retain(|d| match d {
                Deferred::Command(a) => {
                    asked.push(a.clone());
                    false
                }
                Deferred::Point { .. } => true,
            });
            for a in asked {
                self.enter_lane(a, events).await;
            }
            self.start_lane(events).await;
        })
    }

    pub(in crate::agent) fn start_lane<'a>(
        &'a mut self,
        events: &'a mpsc::Sender<AgentEvent>,
    ) -> Pin<Box<impl Future<Output = ()> + 'a>> {
        Box::pin(async move {
            if self.hooks.busy_with.is_some() || self.queue.lane.running.is_some() {
                return;
            }
            while let Some(asked) = self.queue.lane.waiting.pop_front() {
                if let Some(hint) = self.off_refusal(&asked.command.plugin) {
                    let outcome = refused(CommandRefusal::PluginOff, hint);
                    self.command_done(&asked, outcome, false, events).await;
                    continue;
                }
                if let Some(hint) = asked
                    .command
                    .between_turns
                    .clone()
                    .filter(|_| self.turn_running())
                {
                    let outcome = refused(CommandRefusal::Busy, hint);
                    self.command_done(&asked, outcome, false, events).await;
                    continue;
                }
                self.publish_hook_snapshot();
                let lane = &mut self.queue.lane;
                lane.generation += 1;
                let cancel = CancellationToken::new();
                spawn_run(
                    &asked,
                    lane.generation,
                    cancel.clone(),
                    self.queue.tx.clone(),
                );
                self.queue.lane.running = Some(Running {
                    generation: self.queue.lane.generation,
                    asked,
                    cancel,
                });
                return;
            }
        })
    }

    pub(in crate::agent) fn command_finished<'a>(
        &'a mut self,
        generation: u64,
        ran: Ran,
        events: &'a mpsc::Sender<AgentEvent>,
    ) -> Pin<Box<impl Future<Output = ()> + 'a>> {
        Box::pin(async move {
            self.renamed(events).await;
            self.show_links(events).await;
            self.sign_ins_ended(events).await;
            let Some(asked) = self.queue.lane.take_running(generation) else {
                return;
            };
            self.emit_nested(events).await;
            let outcome = match ran {
                Ran::Reply(CommandReply::Send(text)) if self.hooks.holding => {
                    self.hooks
                        .held
                        .push_back(Held::Reply(Reply { asked, text }));
                    return;
                }
                Ran::Reply(CommandReply::Send(text)) => {
                    self.place_reply(asked.arrival, text, events).await
                }
                Ran::Reply(CommandReply::None) => CommandOutcome::None,
                Ran::Reply(CommandReply::Note(text)) => CommandOutcome::Note { text },
                Ran::Reply(CommandReply::Lines(lines)) => CommandOutcome::Lines {
                    lines: crate::plugin::convert::command_lines(lines),
                },
                Ran::Failed(message) => CommandOutcome::Failed { message },
                Ran::TimedOut => CommandOutcome::TimedOut,
                Ran::Cancelled => CommandOutcome::Cancelled,
            };
            self.command_done(&asked, outcome, true, events).await;
            self.start_lane(events).await;
        })
    }

    pub(in crate::agent) async fn place_held(
        &mut self,
        reply: Reply,
        events: &mpsc::Sender<AgentEvent>,
    ) {
        let outcome = self
            .place_reply(reply.asked.arrival, reply.text, events)
            .await;
        self.command_done(&reply.asked, outcome, true, events).await;
    }

    pub(in crate::agent) async fn cancel_held(
        &mut self,
        reply: Reply,
        events: &mpsc::Sender<AgentEvent>,
    ) {
        self.command_done(&reply.asked, CommandOutcome::Cancelled, true, events)
            .await;
    }

    async fn place_reply(
        &mut self,
        arrival: u64,
        text: String,
        events: &mpsc::Sender<AgentEvent>,
    ) -> CommandOutcome {
        if text.trim().is_empty() {
            return refused(CommandRefusal::Empty, reason(&RejectKind::Empty, ""));
        }
        if self.inbox.len() >= MAX_WAITING {
            return refused(CommandRefusal::TooMany, reason(&RejectKind::TooMany, ""));
        }
        self.start_log(events).await;
        let id = self.queue.lane.next_reply();
        let steers = self.track.is_some() || self.hooks.pre_turn;
        let mode = if steers {
            DeliveryMode::Steer
        } else {
            DeliveryMode::Queue
        };
        if !steers || self.compacting() {
            self.lapse_offer(SENT_AFTER);
            self.cancel_rewinds(wisp_protocol::SENT_BEFORE_REWIND, events)
                .await;
        }
        self.inbox.clear_returned();
        let input = Input {
            id: id.clone(),
            mode,
            text,
            images: Vec::new(),
            refs: Vec::new(),
            held: false,
            from: None,
            arrival: Some(arrival),
            note: self.track.is_some().then(|| STEER_NOTE.to_owned()),
        };
        let front = self.inbox.has_front();
        self.announce(&input, None, front, events).await;
        self.inbox.place(input);
        CommandOutcome::Sent { input: id }
    }

    async fn command_done(
        &mut self,
        asked: &Asked,
        outcome: CommandOutcome,
        started: bool,
        events: &mpsc::Sender<AgentEvent>,
    ) {
        let done = AgentEvent::CommandDone {
            answers: asked.request,
            outcome,
        };
        self.emit(events, done).await;
        if started {
            self.defer_command(asked.command.full_name(), asked.args.expose().to_owned());
        }
    }

    pub(in crate::agent) fn defer_command(&mut self, name: String, args: String) {
        let Some(args) = wisp_protocol::masked_command(&args) else {
            return;
        };
        self.defer(COMMAND, Event::Command(CommandInfo { name, args }));
    }

    pub(in crate::agent) async fn cancel_asked(
        &mut self,
        hit: impl Fn(&Asked) -> bool,
        events: &mpsc::Sender<AgentEvent>,
    ) {
        let mut cancelled: Vec<(Asked, bool)> = Vec::new();
        if let Some(running) = self.queue.lane.running.take_if(|r| hit(&r.asked)) {
            running.cancel.cancel();
            cancelled.push((running.asked, true));
        }
        let lane = &mut self.queue.lane;
        let (gone, kept): (VecDeque<Asked>, VecDeque<Asked>) =
            lane.waiting.drain(..).partition(&hit);
        lane.waiting = kept;
        cancelled.extend(gone.into_iter().map(|a| (a, false)));
        let mut points = VecDeque::new();
        for deferred in std::mem::take(&mut self.hooks.deferred) {
            match deferred {
                Deferred::Command(a) if hit(&a) => cancelled.push((a, false)),
                other => points.push_back(other),
            }
        }
        self.hooks.deferred = points;
        cancelled.sort_by_key(|(a, _)| a.arrival);
        for (asked, started) in cancelled {
            self.command_done(&asked, CommandOutcome::Cancelled, started, events)
                .await;
        }
    }

    pub(in crate::agent) async fn cancel_commands(
        &mut self,
        from: Option<u64>,
        events: &mpsc::Sender<AgentEvent>,
    ) {
        let hit = |arrival: u64| from.is_none_or(|from| arrival >= from);
        self.cancel_asked(|a| hit(a.arrival), events).await;
        let mut held = VecDeque::new();
        let mut requests = Vec::new();
        for entry in std::mem::take(&mut self.hooks.held) {
            match entry {
                Held::Command(arrival, ClientCommand::RunPluginCommand { request, .. })
                    if hit(arrival) =>
                {
                    requests.push((arrival, request));
                }
                other => held.push_back(other),
            }
        }
        self.hooks.held = held;
        for (_, request) in requests {
            let done = AgentEvent::CommandDone {
                answers: request,
                outcome: CommandOutcome::Cancelled,
            };
            self.emit(events, done).await;
        }
        self.start_lane(events).await;
    }
}

#[cfg(test)]
mod tests;
