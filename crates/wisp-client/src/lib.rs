pub mod link;

use std::time::Duration;

use tokio::signal::unix::{Signal, SignalKind, signal};
use tokio::sync::{mpsc, watch};
use wisp_protocol::{AgentEvent, ClientCommand};

pub struct Channels {
    pub commands: mpsc::Sender<ClientCommand>,
    pub events: mpsc::Receiver<AgentEvent>,
    pub quit: Option<QuitBound>,
}

#[derive(Clone, Debug)]
pub struct QuitBound {
    pub worst: watch::Receiver<Duration>,
    pub margin: Duration,
}

impl QuitBound {
    pub fn limit(&self) -> Duration {
        self.worst.borrow().saturating_add(self.margin)
    }

    pub async fn expired(mut self) {
        let mut watching = true;
        loop {
            let limit = self.limit();
            tokio::select! {
                changed = self.worst.changed(), if watching => watching = changed.is_ok(),
                () = tokio::time::sleep(limit) => return,
            }
        }
    }
}

pub async fn drain_within(
    events: &mut mpsc::Receiver<AgentEvent>,
    quit: Option<QuitBound>,
    mut show: impl FnMut(&AgentEvent) -> Result<(), String>,
) -> Result<bool, String> {
    let expired = async move {
        match quit {
            Some(quit) => quit.expired().await,
            None => std::future::pending().await,
        }
    };
    let mut expired = std::pin::pin!(expired);
    loop {
        tokio::select! {
            event = events.recv() => match event {
                Some(event) => show(&event)?,
                None => return Ok(false),
            },
            () = &mut expired => return Ok(true),
        }
    }
}

pub const AGENT_STOPPED: &str = "the agent stopped unexpectedly";

pub fn interrupts() -> Result<Signal, String> {
    signal(SignalKind::interrupt()).map_err(|e| format!("could not listen for ctrl-c: {e}"))
}
