use std::fs::{self, File, OpenOptions};
use std::future::Future;
use std::io;
use std::os::unix::fs::{FileTypeExt, OpenOptionsExt};
use std::path::{Path, PathBuf};
use std::sync::Arc;

use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::UnixStream;
use tokio::sync::mpsc;
use tokio::task::JoinSet;
use wisp_client::link as client;
use wisp_protocol::link::{self, DAEMON, FromDaemon, Ready, ToDaemon};
use wisp_protocol::{jsonl, valid_id};

pub struct Daemon<F, J> {
    data: PathBuf,
    starter: F,
    job: J,
    _lock: File,
}

impl<F, J, S, W> Daemon<F, J>
where
    F: Fn(PathBuf, Option<String>) -> S + Send + Sync + 'static,
    J: Fn(&UnixStream) -> bool + Send + Sync + 'static,
    S: Future<Output = io::Result<(Option<Ready>, W)>> + Send,
    W: Future<Output = ()> + Send + 'static,
{
    pub fn new(data: &Path, starter: F, job: J) -> io::Result<Self> {
        Ok(Self {
            data: data.to_owned(),
            starter,
            job,
            _lock: lock(data)?,
        })
    }

    pub async fn run(
        self,
        ready: impl FnOnce(&Path),
        stop: impl Future<Output = ()>,
    ) -> io::Result<()> {
        let path = link::daemon_socket(&self.data);
        if fs::symlink_metadata(&path).is_ok_and(|meta| meta.file_type().is_socket()) {
            if !crate::socket::dead(&path).await {
                return Err(io::Error::from(io::ErrorKind::AddrInUse));
            }
            fs::remove_file(&path)?;
        }
        let socket = crate::Socket::at(&self.data, path.clone())?;
        ready(&path);
        let daemon = Arc::new(self);
        let (shutdown, mut stopped) = mpsc::unbounded_channel();
        let mut stopping = None;
        let mut clients = JoinSet::new();
        tokio::pin!(stop);
        let result = loop {
            tokio::select! {
                _ = &mut stop => break Ok(()),
                Some(stream) = stopped.recv() => {
                    stopping = Some(stream);
                    break Ok(());
                },
                Some(_) = clients.join_next() => {},
                accepted = socket.listener.accept() => match accepted {
                    Ok((stream, _)) => {
                        let daemon = daemon.clone();
                        let shutdown = shutdown.clone();
                        clients.spawn(async move { let _ = daemon.client(stream, shutdown).await; });
                    }
                    Err(error) => break Err(error),
                },
            }
        };
        clients.abort_all();
        while clients.join_next().await.is_some() {}
        drop(socket);
        drop(daemon);
        drop(stopping);
        drop(stopped);
        result
    }

    async fn client(
        &self,
        stream: UnixStream,
        shutdown: mpsc::UnboundedSender<UnixStream>,
    ) -> io::Result<()> {
        crate::same_user(&stream)?;
        if (self.job)(&stream) {
            return Err(io::Error::from(io::ErrorKind::PermissionDenied));
        }
        let mut stream = BufReader::new(stream);
        let mut greeted = false;
        let mut line = Vec::new();
        loop {
            line.clear();
            if stream.read_until(b'\n', &mut line).await? == 0 {
                return Ok(());
            }
            let frame: Option<ToDaemon> = std::str::from_utf8(&line).ok().and_then(link::read);
            line.clear();
            let reply = match frame {
                Some(ToDaemon::Stop) => {
                    let _ = shutdown.send(stream.into_inner());
                    return Ok(());
                }
                Some(ToDaemon::Hello { daemon }) => {
                    greeted = daemon == DAEMON;
                    if greeted {
                        FromDaemon::Welcome { daemon: DAEMON }
                    } else {
                        FromDaemon::Refused { daemon: DAEMON }
                    }
                }
                Some(ToDaemon::Chats) if greeted => FromDaemon::Chats {
                    running: running(&self.data).await?,
                },
                Some(ToDaemon::Start { folder, resume }) if greeted => {
                    self.start(folder, resume).await
                }
                Some(ToDaemon::Open { session }) if greeted => {
                    let listed = client::sockets(&self.data)?.contains(&session);
                    let chat = if listed {
                        client::dial(&link::socket(&self.data, &session)).await.ok()
                    } else {
                        None
                    };
                    if let Some(chat) = chat {
                        send(&mut stream, FromDaemon::Linked).await?;
                        return relay(stream, chat).await;
                    }
                    FromDaemon::NotRunning
                }
                _ => FromDaemon::Unknown,
            };
            send(&mut stream, reply).await?;
        }
    }

    async fn start(&self, folder: PathBuf, resume: Option<String>) -> FromDaemon {
        if !folder.is_absolute()
            || !folder.is_dir()
            || resume.as_deref().is_some_and(|id| !valid_id(id))
        {
            return FromDaemon::NotStarted { ready: None };
        }
        let Ok((ready, wait)) = (self.starter)(folder, resume).await else {
            return FromDaemon::NotStarted { ready: None };
        };
        tokio::spawn(wait);
        if let Some(Ready::Socket { path }) = &ready
            && let Some(session) = link::session_of(path)
        {
            return FromDaemon::Started {
                session: session.to_owned(),
            };
        }
        FromDaemon::NotStarted { ready }
    }
}

pub fn lock(data: &Path) -> io::Result<File> {
    let folder = link::daemon_dir(data);
    crate::socket::private_dir(&folder)?;
    let lock = OpenOptions::new()
        .create(true)
        .truncate(false)
        .read(true)
        .write(true)
        .mode(0o600)
        .open(folder.join("daemon.lock"))?;
    lock.try_lock().map_err(io::Error::other)?;
    Ok(lock)
}

pub async fn running(data: &Path) -> io::Result<Vec<String>> {
    let mut running = Vec::new();
    for session in client::sockets(data)? {
        if client::dial(&link::socket(data, &session)).await.is_ok() {
            running.push(session);
        }
    }
    Ok(running)
}

#[cfg(target_os = "linux")]
pub fn in_job(stream: &UnixStream) -> bool {
    crate::socket::peer_pid(stream)
        .ok()
        .flatten()
        .is_none_or(pid_in_job)
}

#[cfg(not(target_os = "linux"))]
pub fn in_job(_stream: &UnixStream) -> bool {
    false
}

#[cfg(target_os = "linux")]
pub fn pid_in_job(pid: u32) -> bool {
    let mark = format!("{}=", wisp_protocol::WISP_JOB);
    fs::read(format!("/proc/{pid}/environ")).map_or(true, |bytes| {
        bytes
            .split(|byte| *byte == 0)
            .any(|entry| entry.starts_with(mark.as_bytes()))
    })
}

#[cfg(not(target_os = "linux"))]
pub fn pid_in_job(_pid: u32) -> bool {
    false
}

async fn send(stream: &mut BufReader<UnixStream>, frame: FromDaemon) -> io::Result<()> {
    stream
        .write_all(jsonl::line(&frame).map_err(io::Error::other)?.as_bytes())
        .await
}

async fn relay(mut client: BufReader<UnixStream>, mut chat: UnixStream) -> io::Result<()> {
    chat.write_all(client.buffer()).await?;
    let buffered = client.buffer().len();
    client.consume(buffered);
    let (mut input, mut output) = tokio::io::split(client);
    let (mut chat_input, mut chat_output) = chat.split();
    tokio::select! {
        result = tokio::io::copy(&mut input, &mut chat_output) => { result?; },
        result = tokio::io::copy(&mut chat_input, &mut output) => { result?; },
    }
    Ok(())
}
