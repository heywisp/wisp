use std::pin::Pin;

use tokio::sync::mpsc;
use wisp_protocol::{AgentEvent, ClientCommand, QueueReason};

use super::retry::millis;
use super::{Agent, emit};
use crate::lanes::Pass;

impl Agent {
    pub(super) fn enter_endpoint_lane<'a>(
        &'a mut self,
        step: u32,
        commands: &'a mut mpsc::Receiver<ClientCommand>,
        events: &'a mpsc::Sender<AgentEvent>,
    ) -> Pin<Box<impl Future<Output = Option<Pass>> + 'a>> {
        Box::pin(async move {
            loop {
                if !self.park_for_restart(commands, events).await {
                    return None;
                }
                let pass = self.wait_endpoint_lane(step, commands, events).await;
                if !self.agents.restarting() || self.track().stopped {
                    return pass;
                }
            }
        })
    }

    async fn wait_endpoint_lane(
        &mut self,
        step: u32,
        commands: &mut mpsc::Receiver<ClientCommand>,
        events: &mpsc::Sender<AgentEvent>,
    ) -> Option<Pass> {
        let endpoint = self.endpoint_name().to_owned();
        let mut waiter = match self.lanes.enter(&endpoint, self.path().depth()) {
            Ok(pass) => return Some(pass),
            Err(waiter) => waiter,
        };
        let began = tokio::time::Instant::now();
        let mut shown = waiter.ahead();
        let queued = |ahead| AgentEvent::RequestQueued {
            step,
            endpoint: endpoint.clone(),
            ahead,
        };
        emit(events, queued(shown)).await;
        self.waiting_as(Some(QueueReason::Endpoint), events).await;
        let pass = loop {
            let agents = self.agents.clone();
            let admitted = async {
                tokio::select! {
                    biased;
                    () = agents.starting() => None,
                    pass = waiter.admitted(shown) => Some(pass),
                }
            };
            match self.wait_for(admitted, commands, events).await.flatten() {
                None => break None,
                Some(Ok(pass)) => break Some(pass),
                Some(Err(ahead)) => {
                    shown = ahead;
                    emit(events, queued(ahead)).await;
                }
            }
        };
        let waited = began.elapsed();
        self.track().step_queued += waited;
        let admitted = AgentEvent::RequestAdmitted {
            step,
            waited_ms: millis(waited),
        };
        if !self.track().stopped {
            emit(events, admitted).await;
        }
        self.waiting_as(None, events).await;
        pass
    }

    pub(super) fn park_for_restart<'a>(
        &'a mut self,
        commands: &'a mut mpsc::Receiver<ClientCommand>,
        events: &'a mpsc::Sender<AgentEvent>,
    ) -> Pin<Box<impl Future<Output = bool> + 'a>> {
        Box::pin(async move {
            if self.agents.restarting() {
                let parked = self.agents.parking();
                emit(events, AgentEvent::ReadyToRestart).await;
                let agents = self.agents.clone();
                let resumed = async move {
                    if let Some(parked) = parked {
                        let _ = parked.await;
                    }
                    agents.resumed().await;
                };
                if self.wait_for(resumed, commands, events).await.is_none() {
                    return false;
                }
                self.agents.requesting();
            }
            !self.track().stopped
        })
    }

    pub(super) fn wait_for<'a, T: 'a>(
        &'a mut self,
        ready: impl Future<Output = T> + 'a,
        commands: &'a mut mpsc::Receiver<ClientCommand>,
        events: &'a mpsc::Sender<AgentEvent>,
    ) -> Pin<Box<impl Future<Output = Option<T>> + 'a>> {
        Box::pin(self.wait_for_unboxed(ready, commands, events))
    }

    async fn wait_for_unboxed<T>(
        &mut self,
        ready: impl Future<Output = T>,
        commands: &mut mpsc::Receiver<ClientCommand>,
        events: &mpsc::Sender<AgentEvent>,
    ) -> Option<T> {
        tokio::pin!(ready);
        let processes = self.processes.clone();
        let mailbox = self.mailbox.clone();
        loop {
            tokio::select! {
                biased;
                command = commands.recv() => {
                    if self.on_command(command, events).await {
                        return None;
                    }
                }
                done = self.queue.done() => self.finish(done, events).await,
                () = processes.changed() => self.drain_processes(events).await,
                () = mailbox.arrived() => self.accept_mail(events).await,
                value = &mut ready => return Some(value),
            }
        }
    }
}
