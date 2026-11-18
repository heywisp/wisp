use std::future::pending;

use tokio::sync::mpsc;
use tokio::time::{Instant, sleep_until};
use wisp_protocol::voice::AudioFrame;
use wisp_protocol::{AgentEvent, ClientCommand};

use super::Voice;
use super::service::{Call, Service, Started};
use crate::agents::{ROOT_COMMANDS, ROOT_EVENTS};

const LANE_FRAMES: usize = 64;

pub struct AudioLane {
    pub up: mpsc::Sender<AudioFrame>,
    pub down: mpsc::Receiver<AudioFrame>,
}

pub fn attach(
    commands: mpsc::Sender<ClientCommand>,
    events: mpsc::Receiver<AgentEvent>,
    voice: Voice,
) -> (
    mpsc::Sender<ClientCommand>,
    mpsc::Receiver<AgentEvent>,
    AudioLane,
) {
    let (client_commands, command_rx) = mpsc::channel(ROOT_COMMANDS);
    let (client_events, event_rx) = mpsc::channel(ROOT_EVENTS);
    let (up, up_rx) = mpsc::channel(LANE_FRAMES);
    let (down_tx, down) = mpsc::channel(LANE_FRAMES);
    let service = Service::new(voice, commands, client_events, up_rx, down_tx);
    tokio::spawn(run(service, command_rx, events));
    (client_commands, event_rx, AudioLane { up, down })
}

async fn run(
    mut service: Service,
    mut commands: mpsc::Receiver<ClientCommand>,
    mut events: mpsc::Receiver<AgentEvent>,
) {
    let mut client = true;
    loop {
        let quiet = service.quiet_at();
        let hold = service.hold_at();
        let renew = service.renew_at();
        tokio::select! {
            command = commands.recv(), if client => match command {
                Some(ClientCommand::Voice { command }) => service.command(command).await,
                Some(command) => service.pass(command).await,
                None => {
                    client = false;
                    service.client_gone().await;
                }
            },
            event = events.recv() => match event {
                Some(event) => service.event(event).await,
                None => break,
            },
            started = opening(&mut service.opening) => service.opened(started).await,
            frame = frame(&mut service.call) => service.frame(frame).await,
            () = until(quiet) => service.quiet().await,
            () = until(hold) => service.held().await,
            () = until(renew) => service.renew().await,
        }
    }
    service.session_gone().await;
}

async fn opening(slot: &mut Option<super::service::Opening>) -> Started {
    match slot {
        Some(opening) => (&mut opening.future).await,
        None => pending().await,
    }
}

async fn frame(call: &mut Option<Call>) -> Option<String> {
    match call.as_mut().and_then(|call| call.link.as_mut()) {
        Some(link) => link.frames.recv().await,
        None => pending().await,
    }
}

async fn until(at: Option<Instant>) {
    match at {
        Some(at) => sleep_until(at).await,
        None => pending().await,
    }
}
