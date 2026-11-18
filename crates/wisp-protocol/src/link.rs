use std::path::{Path, PathBuf};

use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};

use crate::{AgentEvent, ClientCommand, Now, Opening, RequestId, SessionView, valid_id};

const SOCKET_EXTENSION: &str = "sock";

pub const CONTRACT: u32 = 1;
pub const HOST_FIRST: RequestId = RequestId(u64::MAX);
pub const REQUEST_RANGE: u64 = 1 << 32;

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", content = "data", rename_all = "snake_case")]
pub enum ToHost {
    Hello { contract: u32, build: u64 },
    Open,
    Command(ClientCommand),
    End { forced: bool },
    EndIfWindowless,
    Restart { wait: bool },
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Welcome {
    pub contract: u32,
    pub build: u64,
    pub wisp: String,
    pub session: String,
    pub first_request: RequestId,
    pub folder: PathBuf,
    pub window: bool,
    #[serde(default)]
    pub restart: bool,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", content = "data", rename_all = "snake_case")]
pub enum ToClient {
    Welcome(Welcome),
    Refused {
        contract: u32,
        wisp: String,
    },
    Opened {
        opening: Box<Opening>,
        view: SessionView,
        now: Option<Now>,
    },
    Event(Box<AgentEvent>),
    Taken,
    WindowOpen,
    Restarting,
    Ended {
        saved: bool,
    },
}

impl ToClient {
    pub fn event(event: AgentEvent) -> Self {
        Self::Event(Box::new(event))
    }
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Start {
    pub asks: bool,
    pub terminal: bool,
    pub account: Option<String>,
    pub follow: Vec<String>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Ready {
    Notice { text: String },
    Socket { path: PathBuf },
    Window { reason: String },
    Failed { text: String },
    Ask,
}

pub fn run_dir(data: &Path) -> PathBuf {
    data.join("run")
}

pub fn socket(data: &Path, session: &str) -> PathBuf {
    run_dir(data).join(format!("{session}.{SOCKET_EXTENSION}"))
}

pub fn error_file(socket: &Path) -> PathBuf {
    socket.with_extension("err")
}

pub fn session_of(path: &Path) -> Option<&str> {
    (path.extension()? == SOCKET_EXTENSION)
        .then(|| path.file_stem()?.to_str())
        .flatten()
        .filter(|id| valid_id(id))
}

pub fn read<T: DeserializeOwned>(line: &str) -> Option<T> {
    serde_json::from_str(line).ok()
}

pub const DAEMON: u32 = 1;

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", content = "data", rename_all = "snake_case")]
pub enum ToDaemon {
    Hello {
        daemon: u32,
    },
    Chats,
    Start {
        folder: PathBuf,
        resume: Option<String>,
    },
    Open {
        session: String,
    },
    Stop,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", content = "data", rename_all = "snake_case")]
pub enum FromDaemon {
    Welcome { daemon: u32 },
    Refused { daemon: u32 },
    Chats { running: Vec<String> },
    Started { session: String },
    NotStarted { ready: Option<Ready> },
    Linked,
    NotRunning,
    Unknown,
}

pub fn daemon_dir(data: &Path) -> PathBuf {
    data.join("daemon")
}

pub fn daemon_socket(data: &Path) -> PathBuf {
    daemon_dir(data).join("daemon.sock")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::jsonl;

    fn round_trip<T: Serialize + DeserializeOwned + PartialEq + std::fmt::Debug>(value: T) {
        assert_eq!(read::<T>(&jsonl::line(&value).unwrap()), Some(value));
    }

    #[test]
    fn every_frame_round_trips() {
        for frame in [
            ToHost::Hello {
                contract: CONTRACT,
                build: 7,
            },
            ToHost::Open,
            ToHost::Command(ClientCommand::Cancel),
            ToHost::End { forced: false },
            ToHost::EndIfWindowless,
            ToHost::Restart { wait: true },
            ToHost::Restart { wait: false },
        ] {
            round_trip(frame);
        }
        for frame in [
            ToClient::Welcome(Welcome {
                contract: CONTRACT,
                build: 7,
                wisp: "test".into(),
                session: "abcd-1234".into(),
                first_request: RequestId(1),
                folder: "/project".into(),
                window: false,
                restart: true,
            }),
            ToClient::Refused {
                contract: CONTRACT,
                wisp: "test".into(),
            },
            ToClient::event(AgentEvent::turn_started(1)),
            ToClient::Taken,
            ToClient::WindowOpen,
            ToClient::Restarting,
            ToClient::Ended { saved: true },
        ] {
            round_trip(frame);
        }
        round_trip(ToClient::Opened {
            opening: Box::new(
                serde_json::from_str(include_str!("../tests/fixtures/opening.json")).unwrap(),
            ),
            view: SessionView::default(),
            now: None,
        });
        round_trip(Start::default());
        for frame in [
            Ready::Notice {
                text: "notice".into(),
            },
            Ready::Socket {
                path: "/run/abcd.sock".into(),
            },
            Ready::Window {
                reason: "no log".into(),
            },
            Ready::Failed {
                text: "start failed".into(),
            },
            Ready::Ask,
        ] {
            round_trip(frame);
        }
    }

    #[test]
    fn every_daemon_frame_round_trips() {
        for frame in [
            ToDaemon::Hello { daemon: DAEMON },
            ToDaemon::Chats,
            ToDaemon::Start {
                folder: "/project".into(),
                resume: None,
            },
            ToDaemon::Start {
                folder: "/project".into(),
                resume: Some("abcd-1234".into()),
            },
            ToDaemon::Open {
                session: "abcd-1234".into(),
            },
            ToDaemon::Stop,
        ] {
            round_trip(frame);
        }
        for frame in [
            FromDaemon::Welcome { daemon: DAEMON },
            FromDaemon::Refused { daemon: DAEMON },
            FromDaemon::Chats {
                running: vec!["abcd-1234".into()],
            },
            FromDaemon::Started {
                session: "abcd-1234".into(),
            },
            FromDaemon::NotStarted { ready: None },
            FromDaemon::NotStarted {
                ready: Some(Ready::Ask),
            },
            FromDaemon::Linked,
            FromDaemon::NotRunning,
            FromDaemon::Unknown,
        ] {
            round_trip(frame);
        }
        assert_eq!(
            daemon_socket(Path::new("/data")),
            daemon_dir(Path::new("/data")).join("daemon.sock")
        );
        assert_eq!(session_of(&daemon_socket(Path::new("/data"))), None);
    }

    #[test]
    fn unknown_lines_are_skipped_without_an_error_holding_the_line() {
        let lines = [
            "{\"type\":\"future\",\"key\":\"CANARY-SECRET\"}",
            "not json CANARY-SECRET",
            "{\"type\":\"open\"}",
        ];
        let frames: Vec<ToHost> = lines.into_iter().filter_map(read).collect();
        assert_eq!(frames, [ToHost::Open]);
        assert!(!format!("{frames:?}").contains("CANARY-SECRET"));
    }

    #[test]
    fn only_a_valid_session_socket_names_a_chat() {
        let path = socket(Path::new("/data"), "abcd-1234");
        assert_eq!(session_of(&path), Some("abcd-1234"));
        assert_eq!(
            error_file(&path),
            run_dir(Path::new("/data")).join("abcd-1234.err")
        );
        for (name, id) in [
            ("abcd-1234.sock", Some("abcd-1234")),
            (".host-12-1.sock", None),
            ("not-a-session.sock", None),
            (".sock", None),
            ("abcd.err", None),
        ] {
            assert_eq!(session_of(Path::new(name)), id, "{name}");
        }
    }
}
