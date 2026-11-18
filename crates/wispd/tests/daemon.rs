#![allow(
    clippy::unwrap_used,
    reason = "scripted daemon helpers fail the test on setup, transport or assertion errors"
)]

#[path = "../../wisp-tui/tests/support/scratch.rs"]
mod scratch;

use std::collections::VecDeque;
use std::io;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};

#[path = "support/raw.rs"]
mod raw;
use raw::{Raw, WAIT};

use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader};
use tokio::net::{UnixListener, UnixStream};
use tokio::sync::{mpsc, oneshot};
use tokio::task::JoinHandle;
use wisp_client::link::{self as client, LinkError};
use wisp_protocol::jsonl;
use wisp_protocol::link::{self, DAEMON, FromDaemon, Ready, ToDaemon, ToHost};
use wispd::daemon::Daemon;

struct Script {
    scratch: scratch::ScratchDir,
    path: PathBuf,
    stop: oneshot::Sender<()>,
    task: JoinHandle<io::Result<()>>,
    starts: mpsc::UnboundedReceiver<(PathBuf, Option<String>)>,
    reaped: mpsc::UnboundedReceiver<()>,
}

impl Script {
    async fn new(replies: Vec<Option<Ready>>, job: fn(&UnixStream) -> bool) -> Self {
        let scratch = scratch::ScratchDir::new("wisp-daemon-", "service");
        let (start, starts) = mpsc::unbounded_channel();
        let (reap, reaped) = mpsc::unbounded_channel();
        let replies = Arc::new(Mutex::new(VecDeque::from(replies)));
        let daemon = Daemon::new(
            &scratch.root,
            move |folder, resume| {
                start.send((folder, resume)).unwrap();
                let reply = replies.lock().unwrap().pop_front().flatten();
                let reap = reap.clone();
                async move {
                    Ok((reply, async move {
                        reap.send(()).unwrap();
                    }))
                }
            },
            job,
        )
        .unwrap();
        let (ready, bound) = oneshot::channel();
        let (stop, stopped) = oneshot::channel();
        let task = tokio::spawn(daemon.run(
            move |path| {
                ready.send(path.to_owned()).unwrap();
            },
            async {
                let _ = stopped.await;
            },
        ));
        let path = bound.await.unwrap();
        Self {
            scratch,
            path,
            stop,
            task,
            starts,
            reaped,
        }
    }

    async fn raw(&self) -> Raw {
        Raw::connect(&self.path).await
    }

    async fn connect(&self) -> client::Daemon {
        client::daemon(&self.path).await.unwrap()
    }

    async fn finish(self) -> scratch::ScratchDir {
        let _ = self.stop.send(());
        tokio::time::timeout(WAIT, self.task)
            .await
            .unwrap()
            .unwrap()
            .unwrap();
        assert!(!self.path.exists());
        self.scratch
    }
}

#[tokio::test]
async fn greeting_versions_unknown_lines_and_cross_version_stop() {
    let script = Script::new(vec![], |_| false).await;
    let mut raw = script.raw().await;
    for daemon in [DAEMON, DAEMON + 1, DAEMON] {
        raw.send(&ToDaemon::Hello { daemon }).await;
        assert_eq!(
            raw.next::<FromDaemon>().await,
            if daemon == DAEMON {
                FromDaemon::Welcome { daemon: DAEMON }
            } else {
                FromDaemon::Refused { daemon: DAEMON }
            }
        );
    }
    raw.0
        .write_all(b"{\"type\":\"future\",\"key\":\"CANARY-SECRET\"}\nnot json CANARY-SECRET\n")
        .await
        .unwrap();
    assert_eq!(raw.next::<FromDaemon>().await, FromDaemon::Unknown);
    assert_eq!(raw.next::<FromDaemon>().await, FromDaemon::Unknown);
    raw.0.write_all(b"\xffCANARY-SECRET\n").await.unwrap();
    assert_eq!(raw.next::<FromDaemon>().await, FromDaemon::Unknown);
    raw.send(&ToDaemon::Hello { daemon: DAEMON + 1 }).await;
    assert_eq!(
        raw.next::<FromDaemon>().await,
        FromDaemon::Refused { daemon: DAEMON }
    );
    raw.send(&ToDaemon::Stop).await;
    let mut tail = String::new();
    tokio::time::timeout(WAIT, raw.0.read_to_string(&mut tail))
        .await
        .unwrap()
        .unwrap();
    assert!(tail.is_empty());
    script.finish().await;
}

#[tokio::test]
async fn chats_are_sorted_live_names_and_probes_send_no_frame() {
    let script = Script::new(vec![], |_| false).await;
    let data = &script.scratch.root;
    std::fs::create_dir(link::run_dir(data)).unwrap();
    let mut listeners = Vec::new();
    for name in ["bb-22", "aa-11", ".host-12-1", "cc-33"] {
        let listener = UnixListener::bind(link::socket(data, name)).unwrap();
        if name == "cc-33" {
            drop(listener);
        } else {
            listeners.push(listener);
        }
    }
    let mut daemon = script.connect().await;
    assert_eq!(daemon.chats().await.unwrap(), ["aa-11", "bb-22"]);
    for listener in &listeners[..2] {
        let (mut stream, _) = listener.accept().await.unwrap();
        let mut bytes = Vec::new();
        stream.read_to_end(&mut bytes).await.unwrap();
        assert!(bytes.is_empty());
    }
    assert_eq!(client::sockets(data).unwrap(), ["aa-11", "bb-22", "cc-33"]);
    assert_eq!(
        std::fs::metadata(link::daemon_dir(data))
            .unwrap()
            .permissions()
            .mode()
            & 0o777,
        0o700
    );
    assert_eq!(
        std::fs::metadata(&script.path)
            .unwrap()
            .permissions()
            .mode()
            & 0o777,
        0o600
    );
    script.finish().await;
}

use std::os::unix::fs::PermissionsExt;

#[tokio::test]
async fn open_forwards_buffered_hello_and_closes_in_both_directions() {
    for close_chat in [true, false] {
        let script = Script::new(vec![], |_| false).await;
        let data = &script.scratch.root;
        std::fs::create_dir(link::run_dir(data)).unwrap();
        let listener = UnixListener::bind(link::socket(data, "aa-11")).unwrap();
        let mut raw = script.raw().await;
        raw.send(&ToDaemon::Hello { daemon: DAEMON }).await;
        assert_eq!(
            raw.next::<FromDaemon>().await,
            FromDaemon::Welcome { daemon: DAEMON }
        );
        let hello = jsonl::line(&ToHost::Hello {
            contract: 71,
            build: 9,
        })
        .unwrap();
        raw.0
            .write_all(
                format!(
                    "{}{hello}",
                    jsonl::line(&ToDaemon::Open {
                        session: "aa-11".into()
                    })
                    .unwrap()
                )
                .as_bytes(),
            )
            .await
            .unwrap();
        assert_eq!(raw.next::<FromDaemon>().await, FromDaemon::Linked);
        let (chat, _) = listener.accept().await.unwrap();
        let mut chat = BufReader::new(chat);
        let mut line = String::new();
        chat.read_line(&mut line).await.unwrap();
        assert_eq!(line, hello);
        chat.write_all(b"placeholder host bytes\n").await.unwrap();
        line.clear();
        raw.0.read_line(&mut line).await.unwrap();
        assert_eq!(line, "placeholder host bytes\n");
        raw.0
            .write_all(b"placeholder client bytes\n")
            .await
            .unwrap();
        line.clear();
        chat.read_line(&mut line).await.unwrap();
        assert_eq!(line, "placeholder client bytes\n");
        let mut rest = Vec::new();
        if close_chat {
            drop(chat);
            tokio::time::timeout(WAIT, raw.0.read_to_end(&mut rest))
                .await
                .unwrap()
                .unwrap();
        } else {
            drop(raw);
            tokio::time::timeout(WAIT, chat.read_to_end(&mut rest))
                .await
                .unwrap()
                .unwrap();
        }
        assert!(rest.is_empty());
        script.finish().await;
    }
}

#[tokio::test]
async fn open_never_treats_a_request_as_a_path() {
    let script = Script::new(vec![], |_| false).await;
    for session in ["", "../aa-11", "not-a-chat", "aa-11", "/tmp/aa-11"] {
        assert!(matches!(
            script.connect().await.open(session.into(), 0).await,
            Err(LinkError::NotRunning)
        ));
    }
    script.finish().await;
}

#[tokio::test]
async fn start_validates_before_the_starter_and_reaps_every_started_child() {
    let mut script = Script::new(
        vec![
            Some(Ready::Socket {
                path: "/run/aa-11.sock".into(),
            }),
            Some(Ready::Window {
                reason: "placeholder reason".into(),
            }),
            None,
        ],
        |_| false,
    )
    .await;
    let folder = script.scratch.root.clone();
    let mut daemon = script.connect().await;
    for resume in [Some(""), Some("--resume"), Some("../aa-11")] {
        assert_eq!(
            daemon
                .start(&folder, resume.map(str::to_owned))
                .await
                .unwrap(),
            FromDaemon::NotStarted { ready: None }
        );
    }
    for bad in [
        PathBuf::from("relative"),
        folder.join("missing"),
        script.path.clone(),
    ] {
        assert_eq!(
            daemon.start(&bad, None).await.unwrap(),
            FromDaemon::NotStarted { ready: None }
        );
    }
    assert!(script.starts.try_recv().is_err());
    for (resume, expected) in [
        (
            None,
            FromDaemon::Started {
                session: "aa-11".into(),
            },
        ),
        (
            Some("bb-22".into()),
            FromDaemon::NotStarted {
                ready: Some(Ready::Window {
                    reason: "placeholder reason".into(),
                }),
            },
        ),
        (None, FromDaemon::NotStarted { ready: None }),
    ] {
        assert_eq!(
            daemon.start(&folder, resume.clone()).await.unwrap(),
            expected
        );
        assert_eq!(
            script.starts.recv().await.unwrap(),
            (folder.clone(), resume)
        );
        tokio::time::timeout(WAIT, script.reaped.recv())
            .await
            .unwrap()
            .unwrap();
    }
    script.finish().await;
}

#[tokio::test]
async fn a_named_job_peer_is_refused_without_reading_the_daemons_environment() {
    let script = Script::new(vec![], |peer| {
        peer.peer_cred()
            .is_ok_and(|credentials| credentials.pid() == Some(std::process::id() as i32))
    })
    .await;
    assert!(client::daemon(&script.path).await.is_err());
    assert!(client::stop(&script.path).await.is_err());
    assert!(script.path.exists());
    script.finish().await;
}

#[cfg(target_os = "linux")]
#[test]
fn the_job_check_reads_only_whole_entries_and_refuses_unreadable_peers() {
    for (mark, expected) in [
        (Some("1"), true),
        (Some("11"), true),
        (Some(""), true),
        (None, false),
    ] {
        let mut command = std::process::Command::new("sh");
        command
            .args(["-c", "printf READY; read line"])
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::piped())
            .env_remove(wisp_protocol::WISP_JOB);
        if let Some(mark) = mark {
            command.env(wisp_protocol::WISP_JOB, mark);
        }
        let mut child = command.spawn().unwrap();
        std::io::Read::read_exact(&mut child.stdout.take().unwrap(), &mut [0; 5]).unwrap();
        let got = wispd::daemon::pid_in_job(child.id());
        child.kill().unwrap();
        child.wait().unwrap();
        assert_eq!(got, expected, "mark {mark:?}");
    }
    assert!(wispd::daemon::pid_in_job(u32::MAX));
}

#[tokio::test]
async fn the_lock_excludes_a_second_daemon_and_a_dead_socket_is_replaced() {
    let script = Script::new(vec![], |_| false).await;
    assert!(
        Daemon::new(
            &script.scratch.root,
            |_, _| async { Ok((None, async {})) },
            |_| false
        )
        .is_err()
    );
    script.finish().await;
    let scratch = scratch::ScratchDir::new("wisp-daemon-", "dead");
    std::fs::create_dir(link::daemon_dir(&scratch.root)).unwrap();
    let path = link::daemon_socket(&scratch.root);
    drop(UnixListener::bind(&path).unwrap());
    let daemon = Daemon::new(
        &scratch.root,
        |_, _| async { Ok((None, async {})) },
        |_| false,
    )
    .unwrap();
    daemon
        .run(
            |path| {
                assert!(path.exists());
            },
            async {},
        )
        .await
        .unwrap();
    assert!(!path.exists());
}

#[tokio::test]
async fn stopping_or_termination_closes_a_relay_but_leaves_the_chat() {
    for by_command in [true, false] {
        let script = Script::new(vec![], |_| false).await;
        std::fs::create_dir(link::run_dir(&script.scratch.root)).unwrap();
        let chat_path = link::socket(&script.scratch.root, "aa-11");
        let listener = UnixListener::bind(&chat_path).unwrap();
        let mut raw = script.raw().await;
        raw.send(&ToDaemon::Hello { daemon: DAEMON }).await;
        raw.next::<FromDaemon>().await;
        raw.send(&ToDaemon::Open {
            session: "aa-11".into(),
        })
        .await;
        assert_eq!(raw.next::<FromDaemon>().await, FromDaemon::Linked);
        let (mut chat, _) = listener.accept().await.unwrap();
        if by_command {
            script.connect().await.stop().await.unwrap();
            assert!(!script.path.exists());
            let replacement = Daemon::new(
                &script.scratch.root,
                |_, _| async { Ok((None, async {})) },
                |_| false,
            )
            .unwrap();
            replacement
                .run(|path| assert!(path.exists()), async {})
                .await
                .unwrap();
        }
        let _scratch = script.finish().await;
        let mut rest = Vec::new();
        tokio::time::timeout(WAIT, raw.0.read_to_end(&mut rest))
            .await
            .unwrap()
            .unwrap();
        tokio::time::timeout(WAIT, chat.read_to_end(&mut rest))
            .await
            .unwrap()
            .unwrap();
        assert!(rest.is_empty());
        assert!(chat_path.exists());
    }
}
