#![allow(
    dead_code,
    clippy::unwrap_used,
    clippy::panic,
    reason = "scripted tests use different subsets of the shared transport harness and fail on setup or assertion errors"
)]

#[path = "../../../wisp-render/tests/support/events.rs"]
mod events;
#[path = "raw.rs"]
mod raw;
#[allow(
    clippy::duplicate_mod,
    reason = "the harness also runs outside wisp-cli"
)]
#[path = "../../../wisp-tui/tests/support/scratch.rs"]
mod scratch;

use std::path::PathBuf;

pub(crate) use raw::{Raw, WAIT};
use scratch::ScratchDir;
use tokio::io::BufReader;
use tokio::net::UnixStream;
use tokio::sync::{mpsc, oneshot, watch};
use tokio::task::JoinHandle;
use wisp_client::link::{self as client, Link, LinkEnd};
use wisp_protocol::link::{self, CONTRACT, ToClient, ToHost};
use wisp_protocol::{AgentEvent, ClientCommand, InputId, Opening, RequestId, SessionView};
use wispd::{Host, Session};

pub(crate) struct Script {
    pub(crate) _scratch: ScratchDir,
    pub(crate) path: PathBuf,
    pub(crate) daemon: Option<PathBuf>,
    pub(crate) handle: wispd::Handle,
    pub(crate) commands: mpsc::Receiver<ClientCommand>,
    pub(crate) events: mpsc::Sender<AgentEvent>,
    pub(crate) ending: oneshot::Receiver<(bool, watch::Receiver<bool>)>,
    pub(crate) saved: Option<oneshot::Sender<bool>>,
    pub(crate) host: JoinHandle<bool>,
}

impl Script {
    pub(crate) async fn new() -> Self {
        let view = SessionView {
            events: vec![AgentEvent::turn_started(1), end()],
            returned: vec![wisp_protocol::Returned {
                id: Some(InputId("input-1".into())),
                text: "waiting".into(),
                ..Default::default()
            }],
            ..Default::default()
        };
        Self::with(view, 16, Host::new).await
    }

    pub(crate) async fn with(
        view: SessionView,
        events_cap: usize,
        build: impl FnOnce(Session) -> Host,
    ) -> Self {
        let scratch = ScratchDir::new("wisp-host-", "link");
        let mut socket = wispd::bind(&scratch.root).await.unwrap();
        socket.publish("abcd-1234").unwrap();
        let path = socket.path().to_owned();
        let opening: Opening = serde_json::from_str(include_str!(
            "../../../wisp-protocol/tests/fixtures/opening.json"
        ))
        .unwrap();
        let host = build(Session {
            opening,
            view,
            session: "abcd-1234".into(),
            build: 7,
        });
        let handle = host.handle();
        let (commands, input) = mpsc::channel(16);
        let (output, events) = mpsc::channel(events_cap);
        let (stop, ending) = oneshot::channel();
        let (saved, save) = oneshot::channel();
        let host = tokio::spawn(async move {
            let shutdown = host.run(socket, commands, events).await.unwrap();
            stop.send((shutdown.requested, shutdown.forced.clone()))
                .unwrap();
            shutdown
                .finish(async move { save.await.unwrap_or(false) })
                .await
        });
        Self {
            _scratch: scratch,
            path,
            daemon: None,
            handle,
            commands: input,
            events: output,
            ending,
            saved: Some(saved),
            host,
        }
    }

    pub(crate) async fn connection(&self, build: u64) -> client::Connection<BufReader<UnixStream>> {
        if let Some(path) = &self.daemon {
            client::daemon(path)
                .await
                .unwrap()
                .open(link::session_of(&self.path).unwrap().to_owned(), build)
                .await
                .unwrap()
        } else {
            client::handshake(
                BufReader::new(client::dial(&self.path).await.unwrap()),
                build,
            )
            .await
            .unwrap()
        }
    }

    pub(crate) async fn buffer_before_open(
        &self,
        event: AgentEvent,
    ) -> client::Connection<BufReader<UnixStream>> {
        assert_eq!(self.events.max_capacity(), 1);
        self.events.send(event).await.unwrap();
        tokio::time::timeout(WAIT, async {
            while self.events.capacity() != 1 {
                tokio::task::yield_now().await;
            }
        })
        .await
        .unwrap();
        self.connection(7).await
    }

    pub(crate) async fn open(&self) -> Link {
        client::open(self.connection(7).await).await.unwrap()
    }

    pub(crate) async fn window(&self) -> Raw {
        let mut raw = Raw::connect(&self.path).await;
        raw.send(&ToHost::Hello {
            contract: CONTRACT,
            build: 7,
        })
        .await;
        assert!(matches!(raw.next::<ToClient>().await, ToClient::Welcome(_)));
        raw.send(&ToHost::Open).await;
        assert!(matches!(
            raw.next::<ToClient>().await,
            ToClient::Opened { .. }
        ));
        raw
    }

    pub(crate) async fn ended(&mut self, raw: &mut Raw) {
        raw.send(&ToHost::End { forced: false }).await;
        let _ = tokio::time::timeout(WAIT, &mut self.ending)
            .await
            .unwrap()
            .unwrap();
        self.saved.take().unwrap().send(true).unwrap();
        assert_eq!(
            raw.next::<ToClient>().await,
            ToClient::Ended { saved: true }
        );
    }

    pub(crate) async fn command(&mut self) -> ClientCommand {
        tokio::time::timeout(WAIT, self.commands.recv())
            .await
            .unwrap()
            .unwrap()
    }

    pub(crate) async fn describe(&mut self) -> RequestId {
        let ClientCommand::DescribeSession { request } = self.command().await else {
            panic!("expected a description");
        };
        request
    }

    pub(crate) async fn snapshot(
        &self,
        request: RequestId,
        view: SessionView,
        state: Vec<AgentEvent>,
    ) {
        let AgentEvent::SessionSnapshot { now, .. } = serde_json::from_str(include_str!(
            "../../../wisp-protocol/tests/fixtures/event_session_snapshot.json"
        ))
        .unwrap() else {
            unreachable!()
        };
        self.events
            .send(AgentEvent::SessionSnapshot {
                answers: request,
                view,
                now,
                state,
            })
            .await
            .unwrap();
    }

    pub(crate) async fn close(&mut self, link: &mut Link, forced: bool) {
        link.end.send(forced).await.unwrap();
        self.finish(link, forced).await;
    }

    pub(crate) async fn finish(&mut self, link: &mut Link, forced: bool) {
        let (requested, mut actual) = tokio::time::timeout(WAIT, &mut self.ending)
            .await
            .unwrap()
            .unwrap();
        assert!(requested);
        if forced {
            tokio::time::timeout(WAIT, actual.wait_for(|forced| *forced))
                .await
                .unwrap()
                .unwrap();
        }
        assert_eq!(*actual.borrow(), forced);
        assert!(self.commands.recv().await.is_none());
        self.saved.take().unwrap().send(true).unwrap();
        tokio::time::timeout(WAIT, async {
            while link.ended.borrow().is_none() {
                link.ended.changed().await.unwrap();
            }
        })
        .await
        .unwrap();
        assert_eq!(*link.ended.borrow(), Some(LinkEnd::Ended { saved: true }));
        assert!(!self.path.exists());
        assert!((&mut self.host).await.unwrap());
    }
}

impl Drop for Script {
    fn drop(&mut self) {
        self.host.abort();
    }
}

pub(crate) fn end() -> AgentEvent {
    events::turn_ended(events::End::default())
}
pub(crate) fn text(value: &str) -> AgentEvent {
    AgentEvent::TextDelta {
        index: 0,
        text: value.into(),
    }
}
pub(crate) async fn event(link: &mut Link) -> AgentEvent {
    tokio::time::timeout(WAIT, link.channels.events.recv())
        .await
        .unwrap()
        .unwrap()
}
pub(crate) async fn reopening(script: &Script) -> (tokio::task::JoinHandle<Link>, RequestId) {
    let connection = script.connection(8).await;
    let first = connection.welcome.first_request;
    (
        tokio::spawn(async move { client::open(connection).await.unwrap() }),
        first,
    )
}
