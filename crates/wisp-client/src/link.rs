use std::fmt;
use std::io;
use std::path::{Path, PathBuf};
use std::time::Duration;

use tokio::io::{AsyncBufReadExt, AsyncRead, AsyncWrite, AsyncWriteExt, BufReader};
use tokio::net::UnixStream;
use tokio::sync::{mpsc, watch};
use wisp_protocol::link::{self, CONTRACT, ToClient, ToHost, Welcome};
use wisp_protocol::{AgentEvent, Now, Opening, SessionView, jsonl};

use crate::Channels;

pub const READ_AHEAD: usize = 16_384;
const PROBE_WAIT: Duration = Duration::from_secs(5);

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum LinkError {
    Io(io::ErrorKind),
    Closed,
    WrongUser,
    Refused { contract: u32, wisp: String },
    Unexpected,
    Encode,
    DaemonRefused { daemon: u32 },
    NotRunning,
}

impl fmt::Display for LinkError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Io(kind) => write!(f, "the chat connection failed: {kind}"),
            Self::Closed => f.write_str("the chat connection closed"),
            Self::WrongUser => f.write_str("the chat belongs to another user"),
            Self::Refused { contract, wisp } => write!(
                f,
                "Wisp {wisp} uses chat contract {contract}, which this client cannot open"
            ),
            Self::Unexpected => f.write_str("the chat sent an unexpected frame"),
            Self::DaemonRefused { daemon } => write!(
                f,
                "the daemon uses a connection version this Wisp cannot use ({daemon})"
            ),
            Self::NotRunning => f.write_str("the chat is not kept open"),
            Self::Encode => f.write_str("the chat frame could not be encoded"),
        }
    }
}

impl LinkError {
    pub fn is_gone(&self) -> bool {
        matches!(
            self,
            Self::Io(
                io::ErrorKind::NotFound
                    | io::ErrorKind::NotADirectory
                    | io::ErrorKind::ConnectionRefused
            )
        )
    }
}

impl std::error::Error for LinkError {}

impl From<io::Error> for LinkError {
    fn from(error: io::Error) -> Self {
        Self::Io(error.kind())
    }
}

pub struct Connection<S> {
    pub welcome: Welcome,
    stream: BufReader<S>,
    pending: Vec<u8>,
    at: Option<(PathBuf, u64)>,
}

impl<S: AsyncRead + AsyncWrite + Unpin> Connection<S> {
    pub async fn end(&mut self, forced: bool) -> Result<bool, LinkError> {
        end_with(&mut self.stream, &mut self.pending, forced).await
    }

    pub async fn restart(&mut self, wait: bool) -> Result<(), LinkError> {
        write(&mut self.stream, &ToHost::Restart { wait }).await
    }

    pub async fn next(&mut self) -> Result<ToClient, LinkError> {
        read_frame(&mut self.stream, &mut self.pending).await
    }

    pub async fn end_if_windowless(&mut self) -> Result<Option<bool>, LinkError> {
        write(&mut self.stream, &ToHost::EndIfWindowless).await?;
        loop {
            match self.next().await? {
                ToClient::Ended { saved } => return Ok(Some(saved)),
                ToClient::WindowOpen => return Ok(None),
                ToClient::Event(event) if matches!(*event, AgentEvent::Rejected { .. }) => {
                    return if self.welcome.window {
                        Ok(None)
                    } else {
                        self.end(false).await.map(Some)
                    };
                }
                _ => {}
            }
        }
    }
}

pub async fn end<S: AsyncRead + AsyncWrite + Unpin>(
    stream: S,
    forced: bool,
) -> Result<bool, LinkError> {
    end_with(&mut BufReader::new(stream), &mut Vec::new(), forced).await
}

async fn end_with<S: AsyncRead + AsyncWrite + Unpin>(
    stream: &mut BufReader<S>,
    pending: &mut Vec<u8>,
    forced: bool,
) -> Result<bool, LinkError> {
    write(stream, &ToHost::End { forced }).await?;
    loop {
        if let ToClient::Ended { saved } = read_frame(stream, pending).await? {
            return Ok(saved);
        }
    }
}

pub async fn dial(path: &Path) -> Result<UnixStream, LinkError> {
    let stream = UnixStream::connect(path).await?;
    if stream.peer_cred()?.uid() != rustix::process::getuid().as_raw() {
        return Err(LinkError::WrongUser);
    }
    Ok(stream)
}

pub async fn handshake<S: AsyncRead + AsyncWrite + Unpin>(
    mut stream: S,
    build: u64,
) -> Result<Connection<S>, LinkError> {
    write(
        &mut stream,
        &ToHost::Hello {
            contract: CONTRACT,
            build,
        },
    )
    .await?;
    let mut stream = BufReader::new(stream);
    match read(&mut stream).await? {
        ToClient::Welcome(welcome) if welcome.contract == CONTRACT => Ok(Connection {
            welcome,
            stream,
            pending: Vec::new(),
            at: None,
        }),
        ToClient::Refused { contract, wisp }
        | ToClient::Welcome(Welcome { contract, wisp, .. }) => {
            Err(LinkError::Refused { contract, wisp })
        }
        _ => Err(LinkError::Unexpected),
    }
}

pub async fn connect(path: &Path, build: u64) -> Result<Connection<UnixStream>, LinkError> {
    let mut connection = handshake(dial(path).await?, build).await?;
    connection.at = Some((path.to_owned(), build));
    Ok(connection)
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum LinkEnd {
    Taken,
    Cut,
    Restarting,
    Ended { saved: bool },
    Lost,
}

pub struct Link {
    pub welcome: Welcome,
    pub opening: Opening,
    pub view: SessionView,
    pub now: Option<Now>,
    pub channels: Channels,
    pub end: mpsc::Sender<bool>,
    pub ended: watch::Receiver<Option<LinkEnd>>,
}

impl Link {
    pub async fn end(&mut self, forced: bool) -> Result<bool, LinkError> {
        self.end.send(forced).await.map_err(|_| LinkError::Closed)?;
        loop {
            if let Some(end) = self.ended.borrow().as_ref() {
                return match end {
                    LinkEnd::Ended { saved } => Ok(*saved),
                    _ => Err(LinkError::Closed),
                };
            }
            tokio::select! {
                changed = self.ended.changed() => changed.map_err(|_| LinkError::Closed)?,
                _ = self.channels.events.recv() => {},
            }
        }
    }
}

pub async fn open<S: AsyncRead + AsyncWrite + Unpin + Send + 'static>(
    mut connection: Connection<S>,
) -> Result<Link, LinkError> {
    write(&mut connection.stream, &ToHost::Open).await?;
    let ToClient::Opened { opening, view, now } = connection.next().await? else {
        return Err(LinkError::Unexpected);
    };
    let (commands, mut input) = mpsc::channel(16);
    let (output, events) = mpsc::channel(READ_AHEAD);
    let (end, mut ending) = mpsc::channel(2);
    let (done, ended) = watch::channel(None);
    let at = connection.at;
    let (mut reader, mut writer) = tokio::io::split(connection.stream);
    let closed = ended.clone();
    tokio::spawn(async move {
        let mut reader = BufReader::new(&mut reader);
        let mut restarting = false;
        let finish = loop {
            match read(&mut reader).await {
                Ok(ToClient::Event(event)) => {
                    if output.send(*event).await.is_err() {
                        break LinkEnd::Lost;
                    }
                }
                Ok(ToClient::Taken) => break LinkEnd::Taken,
                Ok(ToClient::Restarting) => restarting = true,
                Ok(ToClient::Ended { .. }) | Err(_) if restarting => break LinkEnd::Restarting,
                Ok(ToClient::Ended { saved }) => break LinkEnd::Ended { saved },
                Ok(_) => {}
                Err(_) if !output.is_closed() => break lost(at).await,
                Err(_) => break LinkEnd::Lost,
            }
        };
        done.send_replace(Some(finish));
    });
    tokio::spawn(async move {
        let mut closed = closed;
        loop {
            let frame = tokio::select! {
                biased;
                forced = ending.recv() => match forced {
                    Some(forced) => ToHost::End { forced },
                    None => break,
                },
                _ = closed.changed() => break,
                command = input.recv() => match command {
                    Some(command) => ToHost::Command(command),
                    None => break,
                },
            };
            if write(&mut writer, &frame).await.is_err() {
                break;
            }
        }
        let _ = writer.shutdown().await;
    });
    Ok(Link {
        welcome: connection.welcome,
        opening: *opening,
        view,
        now,
        channels: Channels {
            commands,
            events,
            quit: None,
        },
        end,
        ended,
    })
}

async fn lost(at: Option<(PathBuf, u64)>) -> LinkEnd {
    let Some((path, build)) = at else {
        return LinkEnd::Lost;
    };
    let stream = match tokio::time::timeout(PROBE_WAIT, dial(&path)).await {
        Ok(Ok(stream)) => stream,
        Ok(Err(error)) if error.is_gone() => return LinkEnd::Lost,
        _ => return LinkEnd::Cut,
    };
    match tokio::time::timeout(PROBE_WAIT, handshake(stream, build)).await {
        Ok(Ok(connection)) if connection.welcome.window => LinkEnd::Taken,
        _ => LinkEnd::Cut,
    }
}

async fn read<R: AsyncRead + Unpin, T: serde::de::DeserializeOwned>(
    reader: &mut BufReader<R>,
) -> Result<T, LinkError> {
    read_frame(reader, &mut Vec::new()).await
}

async fn read_frame<R: AsyncRead + Unpin, T: serde::de::DeserializeOwned>(
    reader: &mut BufReader<R>,
    pending: &mut Vec<u8>,
) -> Result<T, LinkError> {
    loop {
        if reader.read_until(b'\n', pending).await? == 0 {
            return Err(LinkError::Closed);
        }
        let frame = std::str::from_utf8(pending)
            .map(link::read)
            .map_err(|_| LinkError::Io(io::ErrorKind::InvalidData));
        pending.clear();
        if let Some(frame) = frame? {
            return Ok(frame);
        }
    }
}

async fn write<W: AsyncWrite + Unpin, T: serde::Serialize>(
    writer: &mut W,
    frame: &T,
) -> Result<(), LinkError> {
    let line = jsonl::line(frame).map_err(|_| LinkError::Encode)?;
    writer.write_all(line.as_bytes()).await?;
    Ok(())
}

pub fn sockets(data: &Path) -> io::Result<Vec<String>> {
    let entries = match std::fs::read_dir(link::run_dir(data)) {
        Ok(entries) => entries,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(error) => return Err(error),
    };
    let mut names = Vec::new();
    for entry in entries {
        let path = entry?.path();
        if let Some(session) = link::session_of(&path) {
            names.push(session.to_owned());
        }
    }
    names.sort();
    Ok(names)
}

pub struct Daemon {
    stream: BufReader<UnixStream>,
}

async fn greet(path: &Path) -> Result<(Daemon, link::FromDaemon), LinkError> {
    let mut daemon = Daemon {
        stream: BufReader::new(dial(path).await?),
    };
    write(
        &mut daemon.stream,
        &link::ToDaemon::Hello {
            daemon: link::DAEMON,
        },
    )
    .await?;
    let answer = read(&mut daemon.stream).await?;
    Ok((daemon, answer))
}

pub async fn daemon(path: &Path) -> Result<Daemon, LinkError> {
    let (connection, answer) = greet(path).await?;
    match answer {
        link::FromDaemon::Welcome { daemon } if daemon == link::DAEMON => Ok(connection),
        link::FromDaemon::Refused { daemon } | link::FromDaemon::Welcome { daemon } => {
            Err(LinkError::DaemonRefused { daemon })
        }
        _ => Err(LinkError::Unexpected),
    }
}

impl Daemon {
    pub async fn chats(&mut self) -> Result<Vec<String>, LinkError> {
        write(&mut self.stream, &link::ToDaemon::Chats).await?;
        match read(&mut self.stream).await? {
            link::FromDaemon::Chats { running } => Ok(running),
            _ => Err(LinkError::Unexpected),
        }
    }

    pub async fn start(
        &mut self,
        folder: &Path,
        resume: Option<String>,
    ) -> Result<link::FromDaemon, LinkError> {
        write(
            &mut self.stream,
            &link::ToDaemon::Start {
                folder: folder.to_owned(),
                resume,
            },
        )
        .await?;
        match read(&mut self.stream).await? {
            result @ (link::FromDaemon::Started { .. } | link::FromDaemon::NotStarted { .. }) => {
                Ok(result)
            }
            _ => Err(LinkError::Unexpected),
        }
    }

    pub async fn open(
        mut self,
        session: String,
        build: u64,
    ) -> Result<Connection<BufReader<UnixStream>>, LinkError> {
        write(&mut self.stream, &link::ToDaemon::Open { session }).await?;
        match read(&mut self.stream).await? {
            link::FromDaemon::Linked => handshake(self.stream, build).await,
            link::FromDaemon::NotRunning => Err(LinkError::NotRunning),
            _ => Err(LinkError::Unexpected),
        }
    }

    pub async fn stop(mut self) -> Result<(), LinkError> {
        write(&mut self.stream, &link::ToDaemon::Stop).await?;
        let mut line = String::new();
        while self.stream.read_line(&mut line).await? != 0 {
            line.clear();
        }
        Ok(())
    }
}

pub async fn stop(path: &Path) -> Result<(), LinkError> {
    let (connection, answer) = greet(path).await?;
    match answer {
        link::FromDaemon::Welcome { .. } | link::FromDaemon::Refused { .. } => {
            connection.stop().await
        }
        _ => Err(LinkError::Unexpected),
    }
}

#[cfg(test)]
#[path = "../../wisp-tui/tests/support/scratch.rs"]
mod scratch;

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test(start_paused = true)]
    async fn a_silent_live_socket_is_cut_but_refused_or_missing_sockets_are_lost() {
        let scratch = scratch::ScratchDir::new("wisp-link-", "probe");
        let path = scratch.root.join("chat.sock");
        let listener = tokio::net::UnixListener::bind(&path).unwrap();
        let probe = tokio::spawn(lost(Some((path.clone(), 0))));
        let (stream, _) = listener.accept().await.unwrap();
        assert!(!probe.is_finished());
        tokio::time::advance(PROBE_WAIT).await;
        assert_eq!(probe.await.unwrap(), LinkEnd::Cut);
        drop((listener, stream));
        assert_eq!(lost(Some((path.clone(), 0))).await, LinkEnd::Lost);
        std::fs::remove_file(&path).unwrap();
        assert_eq!(lost(Some((path, 0))).await, LinkEnd::Lost);
    }

    #[tokio::test]
    async fn a_restart_frame_survives_the_final_end_and_a_cut_after_it() {
        for ended in [false, true] {
            let (client, server) = tokio::io::duplex(16_384);
            let host = tokio::spawn(async move {
                let mut server = BufReader::new(server);
                let _: ToHost = read(&mut server).await.unwrap();
                let welcome: ToClient = link::read(include_str!(
                    "../../wisp-protocol/tests/fixtures/link_welcome.json"
                ))
                .unwrap();
                write(&mut server, &welcome).await.unwrap();
                let _: ToHost = read(&mut server).await.unwrap();
                let opening = ToClient::Opened {
                    opening: Box::new(
                        link::read(include_str!(
                            "../../wisp-protocol/tests/fixtures/opening.json"
                        ))
                        .unwrap(),
                    ),
                    view: SessionView::default(),
                    now: None,
                };
                write(&mut server, &opening).await.unwrap();
                write(&mut server, &ToClient::Restarting).await.unwrap();
                if ended {
                    write(&mut server, &ToClient::Ended { saved: true })
                        .await
                        .unwrap();
                }
            });
            let mut client = open(handshake(client, 0).await.unwrap()).await.unwrap();
            while client.ended.borrow().is_none() {
                client.ended.changed().await.unwrap();
            }
            assert_eq!(*client.ended.borrow(), Some(LinkEnd::Restarting));
            host.await.unwrap();
        }
    }
}
