pub mod daemon;
mod socket;
mod store;

use std::collections::{BTreeMap, VecDeque};
use std::fs::OpenOptions;
use std::future::Future;
use std::io::{self, Write};
use std::os::unix::fs::OpenOptionsExt;
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;

use tokio::io::{AsyncBufReadExt, AsyncRead, AsyncWrite, AsyncWriteExt, BufReader};
use tokio::sync::{mpsc, oneshot, watch};
use wisp_protocol::link::{self, CONTRACT, HOST_FIRST, REQUEST_RANGE, ToClient, ToHost, Welcome};
use wisp_protocol::{
    AgentEvent, ClientCommand, Opening, RequestId, RestartState, SessionView, jsonl,
};

pub use socket::{Admission, Socket, admit, bind, runs_inside, same_user};
use store::Store;

pub const WAITING: usize = 64 << 20;
const BACKLOG: usize = 128;
const WRITE_WAIT: Duration = Duration::from_secs(1);

pub struct Session {
    pub opening: Opening,
    pub view: SessionView,
    pub session: String,
    pub build: u64,
}

struct Line {
    text: String,
    counted: usize,
    last: bool,
}

struct Peer {
    output: mpsc::UnboundedSender<Line>,
    waiting: Arc<AtomicUsize>,
    stop: watch::Sender<bool>,
    accepted: bool,
    admission: Admission,
    drained: oneshot::Receiver<()>,
}

impl Peer {
    fn send(&self, frames: &[ToClient]) -> bool {
        let counted = !matches!(frames.first(), Some(ToClient::Opened { .. }));
        for frame in frames {
            let Ok(text) = jsonl::line(frame) else {
                self.stop.send_replace(true);
                return false;
            };
            let counted = if counted { text.len() } else { 0 };
            self.waiting.fetch_add(counted, Ordering::Relaxed);
            let line = Line {
                text,
                counted,
                last: matches!(frame, ToClient::Taken | ToClient::Ended { .. }),
            };
            if self.output.send(line).is_err() {
                self.stop.send_replace(true);
                return false;
            }
        }
        true
    }

    fn behind(&self) -> bool {
        self.waiting.load(Ordering::Relaxed) > WAITING
    }
}

enum Received {
    End {
        forced: bool,
    },
    Joined {
        peer: Peer,
        reply: oneshot::Sender<u64>,
    },
    Frame {
        id: u64,
        frame: Option<ToHost>,
    },
    Gone {
        id: u64,
    },
}

#[derive(Clone)]
pub struct Handle(mpsc::Sender<Received>);

impl Handle {
    pub async fn end(&self, forced: bool) -> bool {
        self.0.send(Received::End { forced }).await.is_ok()
    }
}

type Outbound = Box<dyn FnMut(&mut [ToClient], &mut dyn FnMut(&[ToClient])) + Send>;

pub struct Host {
    session: Session,
    outbound: Option<Outbound>,
    first: bool,
    peers: BTreeMap<u64, Peer>,
    window: Option<u64>,
    restart: Option<u64>,
    restart_view: Option<(tokio::time::Instant, AgentEvent)>,
    live: bool,
    pending: BTreeMap<RequestId, u64>,
    request: RequestId,
    store: Store,
    received: mpsc::Receiver<Received>,
    handle: Handle,
    queued: VecDeque<ClientCommand>,
    errors: PathBuf,
}

impl Host {
    pub fn new(session: Session) -> Self {
        let (sent, received) = mpsc::channel(BACKLOG);
        Self {
            session,
            outbound: None,
            first: true,
            peers: BTreeMap::new(),
            window: None,
            restart: None,
            restart_view: None,
            live: false,
            pending: BTreeMap::new(),
            request: HOST_FIRST,
            store: Store::default(),
            received,
            handle: Handle(sent),
            queued: VecDeque::new(),
            errors: PathBuf::new(),
        }
    }

    #[must_use]
    pub fn with_outbound(
        mut self,
        outbound: impl FnMut(&mut [ToClient], &mut dyn FnMut(&[ToClient])) + Send + 'static,
    ) -> Self {
        self.outbound = Some(Box::new(outbound));
        self
    }

    pub fn handle(&self) -> Handle {
        self.handle.clone()
    }

    pub async fn run(
        mut self,
        socket: Socket,
        commands: mpsc::Sender<ClientCommand>,
        mut events: mpsc::Receiver<AgentEvent>,
    ) -> io::Result<Shutdown> {
        self.errors = link::error_file(socket.path());
        let mut next = 1_u64;
        loop {
            tokio::select! {
                accepted = socket.listener.accept() => {
                    let (stream, _) = accepted?;
                    if let Ok(admission) = admit(&stream, self.window.is_some()) {
                        let handle = self.handle();
                        tokio::spawn(async move {
                            serve_admitted(stream, handle, admission).await;
                        });
                    }
                }
                permit = commands.reserve(), if !self.queued.is_empty() => {
                    if let Ok(permit) = permit {
                        if let Some(command) = self.queued.pop_front() {
                            permit.send(command);
                        }
                    } else {
                        break;
                    }
                }
                event = events.recv() => match event {
                    Some(event) => {
                        let ready = matches!(event, AgentEvent::Restart { state: RestartState::Ready, .. });
                        self.event(event);
                        if ready && self.restart.is_some() {
                            return Ok(self.shutdown(socket, events, false, true, true));
                        }
                    },
                    None => break,
                },
                Some(received) = self.received.recv() => match received {
                    Received::Joined { peer, reply } => {
                        if next.checked_mul(REQUEST_RANGE).is_some_and(|n| n < (1 << 63)) {
                            self.peers.insert(next, peer);
                            if reply.send(next).is_err() {
                                self.gone(next);
                            }
                            next += 1;
                        }
                    }
                    Received::Gone { id } => self.gone(id),
                    Received::End { forced }
                    | Received::Frame { frame: Some(ToHost::End { forced }), .. } => {
                        return Ok(self.shutdown(socket, events, forced, true, false));
                    }
                    Received::Frame { id, frame: Some(ToHost::EndIfWindowless) } => {
                        if self.window.is_some() {
                            self.send(id, vec![ToClient::WindowOpen]);
                        } else {
                            return Ok(self.shutdown(socket, events, false, true, false));
                        }
                    }
                    Received::Frame { id, frame: Some(ToHost::Restart { wait }) }
                        if self.peers.get(&id).is_some_and(|peer| peer.accepted) =>
                    {
                        if self.restart.is_some_and(|peer| peer != id) {
                            self.send(id, vec![ToClient::event(AgentEvent::rejected("this chat is in another restart"))]);
                        } else if !wait {
                            return Ok(self.shutdown(socket, events, false, true, true));
                        } else if self.restart.replace(id).is_none() {
                            self.queued.push_back(ClientCommand::GetReady { on: true });
                        }
                    }
                    Received::Frame { id, frame } => self.frame(id, frame),
                },
            }
        }
        Ok(self.shutdown(socket, events, false, false, false))
    }

    fn shutdown(
        mut self,
        socket: Socket,
        events: mpsc::Receiver<AgentEvent>,
        forced: bool,
        requested: bool,
        restart: bool,
    ) -> Shutdown {
        self.restart_view = None;
        let (force, forced) = watch::channel(forced);
        Shutdown {
            forced,
            requested,
            restart,
            force,
            socket,
            host: self,
            events,
        }
    }

    fn gone(&mut self, id: u64) {
        if let Some(peer) = self.peers.remove(&id) {
            peer.stop.send_replace(true);
        }
        if self.restart == Some(id) {
            self.restart = None;
            self.restart_view = None;
            self.queued.push_back(ClientCommand::GetReady { on: false });
        }
        if self.window == Some(id) {
            self.window = None;
            self.live = false;
            self.queued.push_back(ClientCommand::WindowGone);
        }
    }

    fn send(&mut self, id: u64, mut frames: Vec<ToClient>) {
        if self.window == Some(id)
            && frames
                .iter()
                .any(|frame| matches!(frame, ToClient::Opened { .. }))
            && let Some((at, event)) = &self.restart_view
        {
            let mut event = event.clone();
            if let AgentEvent::Restart { open, .. } = &mut event {
                wisp_protocol::age_unready(open, at.elapsed());
            }
            frames.push(ToClient::event(event));
        }
        if self.peers.get(&id).is_some_and(Peer::behind) {
            self.note_cut();
            self.gone(id);
        } else if let Some(peer) = self.peers.get(&id) {
            let mut sent = false;
            if let Some(outbound) = &mut self.outbound {
                let mut called = false;
                outbound(&mut frames, &mut |frames| {
                    if !called {
                        called = true;
                        sent = peer.send(frames);
                    }
                });
            } else {
                sent = peer.send(&frames);
            }
            if !sent {
                self.gone(id);
            }
        }
    }

    fn note_cut(&self) {
        if let Ok(mut file) = OpenOptions::new()
            .append(true)
            .create(true)
            .mode(0o600)
            .open(&self.errors)
        {
            let time = jiff::Timestamp::now().strftime("%Y-%m-%dT%H:%M:%SZ");
            let size = WAITING >> 20;
            let _ = writeln!(
                file,
                "{time} window closed: it stopped reading, and more than {size} MiB was waiting for it"
            );
        }
    }

    fn frame(&mut self, id: u64, frame: Option<ToHost>) {
        match frame {
            Some(ToHost::Hello { contract, .. }) => {
                let Some(peer) = self.peers.get_mut(&id) else {
                    return;
                };
                peer.accepted = contract == CONTRACT;
                let frame = if peer.accepted {
                    ToClient::Welcome(Welcome {
                        contract: CONTRACT,
                        build: self.session.build,
                        wisp: self.session.opening.version.clone(),
                        session: self.session.session.clone(),
                        first_request: RequestId(id * REQUEST_RANGE),
                        folder: self.session.opening.cwd.clone().into(),
                        window: self.window.is_some(),
                        restart: true,
                    })
                } else {
                    ToClient::Refused {
                        contract: CONTRACT,
                        wisp: self.session.opening.version.clone(),
                    }
                };
                self.send(id, vec![frame]);
            }
            Some(ToHost::Open) if self.peers.get(&id).is_some_and(|peer| peer.accepted) => {
                self.open(id)
            }
            Some(ToHost::Command(command))
                if self.window == Some(id)
                    && self.live
                    && self.peers.get(&id).is_some_and(|peer| peer.accepted) =>
            {
                if matches!(command, ClientCommand::GetReady { .. }) {
                    self.send(
                        id,
                        vec![ToClient::event(AgentEvent::rejected(
                            "only the host can get this chat ready for a restart",
                        ))],
                    );
                } else if self.queued.len() < BACKLOG {
                    self.queued.push_back(command);
                } else {
                    self.send(
                        id,
                        vec![ToClient::event(AgentEvent::rejected(
                            "the chat is busy; send that command again",
                        ))],
                    );
                }
            }
            Some(ToHost::End { .. }) => {}
            _ => self.send(
                id,
                vec![ToClient::event(AgentEvent::rejected(
                    "this chat does not recognize that command",
                ))],
            ),
        }
    }

    fn open(&mut self, id: u64) {
        if self.window == Some(id) {
            return;
        }
        let unchecked = self
            .peers
            .get(&id)
            .is_some_and(|peer| peer.admission == Admission::Unchecked);
        if unchecked && self.window.is_some() {
            self.gone(id);
            return;
        }
        if let Some(previous) = self.window.take() {
            self.send(previous, vec![ToClient::Taken]);
            self.queued.push_back(ClientCommand::WindowGone);
        }
        self.window = Some(id);
        self.live = false;
        if std::mem::take(&mut self.first) {
            let mut frames = vec![ToClient::Opened {
                opening: Box::new(self.session.opening.clone()),
                view: std::mem::take(&mut self.session.view),
                now: None,
            }];
            frames.extend(
                self.store
                    .since(&SessionView::default(), &[])
                    .into_iter()
                    .map(ToClient::event),
            );
            self.send(id, frames);
            self.live = self.window == Some(id);
        } else {
            let request = self.request;
            self.request.0 -= 1;
            self.pending.insert(request, id);
            self.queued
                .push_back(ClientCommand::DescribeSession { request });
        }
    }

    fn event(&mut self, event: AgentEvent) {
        let answer = match &event {
            AgentEvent::SessionSnapshot { answers, .. } => Some(*answers),
            AgentEvent::Rejected { request, .. } => *request,
            _ => None,
        };
        if let Some(id) = answer.and_then(|request| self.pending.remove(&request)) {
            if self.window == Some(id) {
                if let AgentEvent::SessionSnapshot {
                    view, now, state, ..
                } = event
                {
                    let mut opening = self.session.opening.clone();
                    opening.start = None;
                    let replay = self.store.since(&view, &state);
                    let mut frames = vec![ToClient::Opened {
                        opening: Box::new(opening),
                        view,
                        now: Some(now),
                    }];
                    frames.extend(state.into_iter().chain(replay).map(ToClient::event));
                    self.send(id, frames);
                    self.live = self.window == Some(id);
                } else {
                    self.gone(id);
                }
            }
            if self.pending.is_empty() {
                self.store.trim();
            }
            return;
        }
        if let Some(request) = event.private_auth_request()
            && self.window != Some(request.0 / REQUEST_RANGE)
        {
            return;
        }
        if let AgentEvent::Restart { state, .. } = &event {
            self.restart_view = (*state != RestartState::Cancelled && self.restart.is_some())
                .then(|| (tokio::time::Instant::now(), event.clone()));
            if let Some(id) = self.restart {
                self.send(id, vec![ToClient::event(event.clone())]);
            }
        } else {
            self.store.push(event.clone(), !self.pending.is_empty());
        }
        if self.live
            && let Some(id) = self.window
        {
            self.send(id, vec![ToClient::event(event)]);
        }
    }
}

pub struct Shutdown {
    pub forced: watch::Receiver<bool>,
    pub requested: bool,
    pub restart: bool,
    force: watch::Sender<bool>,
    socket: Socket,
    host: Host,
    events: mpsc::Receiver<AgentEvent>,
}

impl Shutdown {
    pub fn finish(
        self,
        saved: impl Future<Output = bool>,
    ) -> std::pin::Pin<Box<impl Future<Output = bool>>> {
        Box::pin(async move {
            let Self {
                requested,
                restart,
                force,
                socket,
                mut host,
                mut events,
                ..
            } = self;
            let forward = |host: &mut Host, event| {
                if restart {
                    host.event(event);
                }
            };
            let mut saved = std::pin::pin!(saved);
            let saved = loop {
                let draining = !events.is_closed() || !events.is_empty();
                tokio::select! {
                    saved = &mut saved => break saved,
                    event = events.recv(), if draining => {
                        if let Some(event) = event {
                            forward(&mut host, event);
                        }
                    },
                    Some(received) = host.received.recv() => match received {
                        Received::End { forced: true }
                        | Received::Frame { frame: Some(ToHost::End { forced: true }), .. } => {
                            force.send_replace(true);
                        },
                        Received::Gone { id } => host.gone(id),
                        _ => {},
                    },
                }
            };
            while let Ok(event) = events.try_recv() {
                forward(&mut host, event);
            }
            drop(socket);
            if requested {
                let _ = std::fs::remove_file(&host.errors);
                for peer in host.peers.values() {
                    let mut frames = Vec::new();
                    if restart {
                        frames.push(ToClient::Restarting);
                    }
                    frames.push(ToClient::Ended { saved });
                    peer.send(&frames);
                }
                let by = tokio::time::Instant::now() + WRITE_WAIT;
                for peer in host.peers.values_mut() {
                    if tokio::time::timeout_at(by, &mut peer.drained)
                        .await
                        .is_err()
                    {
                        peer.stop.send_replace(true);
                    }
                }
            }
            saved
        })
    }
}

pub async fn serve<S: AsyncRead + AsyncWrite + Unpin>(stream: S, handle: Handle) {
    serve_admitted(stream, handle, Admission::Unchecked).await;
}

async fn serve_admitted<S: AsyncRead + AsyncWrite + Unpin>(
    stream: S,
    handle: Handle,
    admission: Admission,
) {
    let (output, mut outgoing) = mpsc::unbounded_channel();
    let waiting = Arc::new(AtomicUsize::new(0));
    let (stop, mut stopped) = watch::channel(false);
    let (finished, drained) = oneshot::channel();
    let peer = Peer {
        output,
        waiting: waiting.clone(),
        stop,
        accepted: false,
        admission,
        drained,
    };
    let (reply, joined) = oneshot::channel();
    if handle
        .0
        .send(Received::Joined { peer, reply })
        .await
        .is_err()
    {
        return;
    }
    let Ok(id) = joined.await else {
        return;
    };
    let (reader, mut writer) = tokio::io::split(stream);
    let mut lines = BufReader::new(reader).lines();
    loop {
        tokio::select! {
            _ = stopped.changed() => break,
            line = outgoing.recv() => {
                let Some(line) = line else {
                    break;
                };
                let sent = tokio::select! {
                    _ = stopped.changed() => false,
                    sent = writer.write_all(line.text.as_bytes()) => sent.is_ok(),
                };
                if !sent {
                    break;
                }
                waiting.fetch_sub(line.counted, Ordering::Relaxed);
                if line.last {
                    break;
                }
            }
            line = lines.next_line() => {
                let Ok(Some(line)) = line else {
                    break;
                };
                let received = Received::Frame { id, frame: link::read(&line) };
                if handle.0.send(received).await.is_err() {
                    break;
                }
            }
        }
    }
    let _ = finished.send(());
    let _ = handle.0.send(Received::Gone { id }).await;
}
