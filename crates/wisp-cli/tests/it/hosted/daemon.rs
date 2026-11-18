use super::*;
use support::home::{DAEMON_STOPPED as STOPPED, both, err, mode, out, run};
use wisp_protocol::link::{FromDaemon, ToDaemon};

const ERROR: &str = ".local/share/wisp/daemon/daemon.err";
pub(super) const CLIENT_KEY: &str = "sk-ant-CANARY-DAEMON-CLIENT-ENVIRONMENT";

fn data(home: &Home) -> std::path::PathBuf {
    home.file(".local/share/wisp")
}

fn daemon_socket(home: &Home) -> std::path::PathBuf {
    link::daemon_socket(&data(home))
}

fn daemon_command(home: &Home, server: &Server, args: &[&str]) -> std::process::Output {
    run(command(home, server, WISP).arg("daemon").args(args), "")
}

pub(super) fn start_daemon(home: &Home, server: &Server) {
    let output = daemon_command(home, server, &["start"]);
    assert!(output.status.success(), "{}", both(&output));
    assert_eq!(out(&output), "Daemon: started\n");
}

fn daemon_pid(home: &Home) -> rustix::process::Pid {
    crate::scratch::peer_pid(std::os::unix::net::UnixStream::connect(daemon_socket(home)).unwrap())
        .unwrap()
}

fn connects(home: &Home) -> bool {
    std::os::unix::net::UnixStream::connect(daemon_socket(home)).is_ok()
}

pub(super) fn outside_job(test: &str) -> bool {
    if std::env::var_os("WISP_DAEMON_TEST").is_some() {
        return true;
    }
    let home = Home::new("daemon-client-job");
    let mut command = command_in(&home.0, std::env::current_exe().unwrap());
    command
        .args(["--exact", test, "--nocapture"])
        .env("WISP_DAEMON_TEST", "1")
        .env("ANTHROPIC_API_KEY", CLIENT_KEY)
        .env("TMPDIR", "/tmp");
    if let Some(stack) = std::env::var_os("RUST_MIN_STACK") {
        command.env("RUST_MIN_STACK", stack);
    }
    let output = run(&mut command, "");
    assert!(output.status.success(), "{}", both(&output));
    false
}

pub(super) async fn stream(home: &Home, id: &str) -> tokio::io::BufReader<tokio::net::UnixStream> {
    use tokio::io::{AsyncBufReadExt, AsyncWriteExt};
    let mut stream =
        tokio::io::BufReader::new(wisp_client::link::dial(&daemon_socket(home)).await.unwrap());
    let mut line = String::new();
    for (request, expected) in [
        (
            ToDaemon::Hello {
                daemon: link::DAEMON,
            },
            FromDaemon::Welcome {
                daemon: link::DAEMON,
            },
        ),
        (ToDaemon::Open { session: id.into() }, FromDaemon::Linked),
    ] {
        stream
            .write_all(jsonl::line(&request).unwrap().as_bytes())
            .await
            .unwrap();
        line.clear();
        stream.read_line(&mut line).await.unwrap();
        assert_eq!(link::read::<FromDaemon>(&line), Some(expected));
    }
    stream
}

#[test]
fn a_chat_starts_the_daemon_without_a_ready_reader_and_stop_holds() {
    if !usable() {
        return;
    }
    let home = Home::new("daemon-auto");
    let server = serve();
    fs::remove_file(home.file(STOPPED)).unwrap();
    let mut first = start(&home, &server, &[]);
    turn(&mut first, &home);
    assert!(wait_until(PATIENCE, || connects(&home)));
    let daemon = daemon_pid(&home);
    let host = pid(&home);
    let path = socket(&home).unwrap();
    let id = link::session_of(&path).unwrap().to_owned();
    first.send(b"/leave\r");
    assert!(first.finish().0.success());
    assert!(alive(daemon.as_raw_nonzero().get()));
    assert!(alive(host.as_raw_nonzero().get()));
    assert!(home.read(ERROR).is_empty());
    let stopped = daemon_command(&home, &server, &["stop"]);
    assert!(stopped.status.success(), "{}", both(&stopped));
    assert_eq!(
        out(&stopped),
        "Daemon: stopped\nChats stay open.\nStart it again with: wisp daemon start\n"
    );
    assert!(wait_until(PATIENCE, || !connects(&home)));
    assert!(alive(host.as_raw_nonzero().get()));
    let mut next = start(&home, &server, &["--resume", &id]);
    turn(&mut next, &home);
    assert!(!connects(&home));
    start_daemon(&home, &server);
    let daemon = daemon_pid(&home);
    rustix::process::kill_process(daemon, rustix::process::Signal::TERM).unwrap();
    assert!(wait_until(PATIENCE, || !connects(&home)));
    turn(&mut next, &home);
    start_daemon(&home, &server);
    let daemon = daemon_pid(&home);
    quit(next);
    assert!(alive(daemon.as_raw_nonzero().get()));
    drop(home);
    assert!(wait_until(PATIENCE, || !alive(
        daemon.as_raw_nonzero().get()
    )));
}

#[test]
fn status_lists_titles_and_windowed_chats_but_only_ids_in_a_job() {
    if !usable() {
        return;
    }
    let home = Home::new("daemon-status");
    let server = serve();
    let never = daemon_command(&home, &server, &[]);
    assert_eq!(
        out(&never),
        "Daemon: stopped\nStart it with: wisp daemon start\n\nNo chat is kept open.\n"
    );
    fs::remove_file(home.file(STOPPED)).unwrap();
    assert!(out(&daemon_command(&home, &server, &[])).starts_with("Daemon: not started\n"));
    home.write(STOPPED, "");
    let mut first = start(&home, &server, &[]);
    turn(&mut first, &home);
    let first_path = socket(&home).unwrap();
    let first_id = link::session_of(&first_path).unwrap().to_owned();
    first.send(b"/leave\r");
    assert!(first.finish().0.success());
    let mut command = command(&home, &server, "setsid");
    command
        .args(["--ctty", "--wait", WISP, "--model", MODEL, "--time", "off"])
        .current_dir(home.dir("longer-\x1b[31m\nfolder"))
        .env("TERM", "xterm-256color");
    let second = Pty::start(&mut command, 30, 100, Stdout::Tty);
    second.wait_for(READY);
    start_daemon(&home, &server);
    for running in [true, false] {
        if !running {
            assert!(daemon_command(&home, &server, &["stop"]).status.success());
        }
        let output = daemon_command(&home, &server, &[]);
        assert!(output.status.success(), "{}", both(&output));
        let text = out(&output);
        assert!(text.starts_with(if running {
            "Daemon: running\n"
        } else {
            "Daemon: stopped\n"
        }));
        assert!(
            text.lines().any(|line| line.contains("a short local test")
                && line.contains(wisp_protocol::short_id(&first_id))),
            "{text}"
        );
        assert!(
            text.lines()
                .any(|line| line.contains("~/longer-^[[31m^Jfolder")
                    && line.ends_with("in a window")),
            "{text}"
        );
        let output = super::command(&home, &server, WISP)
            .env(wisp_protocol::WISP_JOB, "1")
            .arg("daemon")
            .output()
            .unwrap();
        let job = out(&output);
        assert!(output.status.success(), "{job}");
        assert!(!job.contains("a short local test"), "{job}");
        assert!(
            job.lines()
                .any(|line| line.trim() == wisp_protocol::short_id(&first_id)),
            "{job}"
        );
        assert!(!job.contains('~') && !job.contains("in a window"), "{job}");
        assert!(!text.contains('\x1b'));
        assert_eq!(text.matches("in a window").count(), 1);
        assert!(text.ends_with("\nOpen one: wisp --resume <id>\nQuit one: wisp quit <id>\n"));
    }
    quit(second);
    assert!(quit_from_terminal(&home, &[&first_id]).0.success());
}

#[test]
fn starting_twice_keeps_errors_and_a_killed_daemon_is_replaced() {
    let home = Home::new("daemon-replace");
    let server = serve();
    start_daemon(&home, &server);
    let old = daemon_pid(&home);
    home.write(ERROR, "placeholder incumbent error\n");
    let second = daemon_command(&home, &server, &["start"]);
    assert!(second.status.success());
    assert_eq!(out(&second), "Daemon: already running\n");
    let loser = daemon_command(&home, &server, &["serve"]);
    assert!(!loser.status.success());
    assert_eq!(home.read(ERROR), "placeholder incumbent error\n");
    assert_eq!(daemon_pid(&home), old);
    rustix::process::kill_process(old, rustix::process::Signal::KILL).unwrap();
    assert!(wait_until(PATIENCE, || !alive(old.as_raw_nonzero().get())));
    start_daemon(&home, &server);
    assert_ne!(daemon_pid(&home), old);
    assert!(home.read(ERROR).is_empty());
    assert_eq!(mode(&daemon_socket(&home)), 0o600);
    assert_eq!(mode(&home.file(ERROR)), 0o600);
    assert_eq!(mode(&link::daemon_dir(&data(&home))), 0o700);
}

#[test]
fn a_failed_start_names_its_error_file_and_an_already_stopped_daemon_stays_stopped() {
    let home = Home::new("daemon-bind-fail");
    let server = serve();
    home.dir(".local/share/wisp/daemon/daemon.sock");
    let output = daemon_command(&home, &server, &["start"]);
    assert!(!output.status.success());
    assert!(out(&output).is_empty());
    assert_eq!(
        err(&output),
        "wisp: the daemon didn't start; see ~/.local/share/wisp/daemon/daemon.err\n"
    );
    assert!(!home.read(ERROR).is_empty());
    let output = daemon_command(&home, &server, &["stop"]);
    assert!(output.status.success());
    assert_eq!(
        out(&output),
        "Daemon: already stopped\nStart it with: wisp daemon start\n"
    );
    assert!(home.exists(STOPPED));
}

#[test]
fn status_shows_the_windows_warning_or_restart_note_first_and_never_asks() {
    let home = Home::new("daemon-wsl");
    let server = serve();
    let path = wsl(&home);
    let status = || {
        run(
            command(&home, &server, WISP)
                .arg("daemon")
                .env("PATH", &path),
            "",
        )
    };
    let missing = out(&status());
    assert!(
        missing.starts_with("About 15 seconds after the last\nterminal closes,"),
        "{missing}"
    );
    assert!(
        missing.find("instanceIdleTimeout=-1").unwrap() < missing.find("Daemon: stopped").unwrap()
    );
    if usable() {
        for no_color in [false, true] {
            let mut command = command(&home, &server, "setsid");
            command
                .args(["--ctty", "--wait", WISP, "daemon"])
                .env("PATH", &path)
                .env("TERM", "linux");
            if no_color {
                command.env("NO_COLOR", "1");
            }
            let pty = Pty::start(&mut command, 30, 100, Stdout::Tty);
            let screen = pty.screen.clone();
            assert!(pty.finish().0.success());
            let raw = String::from_utf8_lossy(&screen.lock().unwrap()).into_owned();
            assert!(!raw.contains("38;2;"));
            assert_eq!(!raw.contains('\x1b'), no_color);
        }
    }
    home.write(
        "mnt/c/Users/user/.wslconfig",
        "[general]\ninstanceIdleTimeout=-1\n",
    );
    let restart = out(&status());
    assert!(restart.starts_with(".wslconfig changed since Linux\n"));
    assert!(restart.find("wsl.exe --shutdown").unwrap() < restart.find("Daemon: stopped").unwrap());
    home.write("proc/stat", "cpu 0\nbtime 9999999999\n");
    assert!(out(&status()).starts_with("Daemon: stopped\n"));
}

#[test]
fn the_real_daemon_starts_reopens_and_restarts_under_a_running_chat_without_cache_or_log_changes() {
    if !outside_job(
        "hosted::daemon::the_real_daemon_starts_reopens_and_restarts_under_a_running_chat_without_cache_or_log_changes",
    ) {
        return;
    }
    use std::sync::atomic::{AtomicBool, Ordering};
    use wisp_client::link as client;
    use wisp_protocol::AgentEvent;
    let begun = Arc::new(AtomicBool::new(false));
    let released = Arc::new(AtomicBool::new(false));
    let (started, release) = (begun.clone(), released.clone());
    let server = Server::start(move |request| {
        if request.head.contains("count_tokens") {
            return counted();
        }
        if !started.swap(true, Ordering::SeqCst) {
            assert!(wait_until(PATIENCE, || release.load(Ordering::SeqCst)));
        }
        text_reply("daemon-local", MODEL, DONE, (1, 5))
    });
    let home = Home::new("daemon-real");
    start_daemon(&home, &server);
    runtime().block_on(async {
        let mut daemon = client::daemon(&daemon_socket(&home)).await.unwrap();
        let FromDaemon::Started { session: id } = daemon.start(&home.0, None).await.unwrap() else {
            panic!("chat did not start");
        };
        let host = pid(&home);
        assert_eq!(
            daemon.chats().await.unwrap().as_slice(),
            std::slice::from_ref(&id)
        );
        drop(daemon);
        let first = open(&home, &id).await;
        first
            .channels
            .commands
            .send(ClientCommand::send("daemon-1", "a local client turn"))
            .await
            .unwrap();
        tokio::time::timeout(PATIENCE, async {
            while !begun.load(Ordering::SeqCst) {
                tokio::time::sleep(std::time::Duration::from_millis(10)).await;
            }
        })
        .await
        .unwrap();
        let before = home.logs();
        drop(first);
        let mut reopened = open(&home, &id).await;
        let seen = tokio::time::timeout(PATIENCE, async {
            loop {
                if let Some(AgentEvent::TurnStarted { .. }) = reopened.channels.events.recv().await
                {
                    break;
                }
            }
        })
        .await;
        assert!(seen.is_ok());
        assert_eq!(home.logs(), before);
        assert!(daemon_command(&home, &server, &["stop"]).status.success());
        tokio::time::timeout(PATIENCE, reopened.ended.changed())
            .await
            .unwrap()
            .unwrap();
        assert!(alive(host.as_raw_nonzero().get()));
        start_daemon(&home, &server);
        let mut next = open(&home, &id).await;
        assert_eq!(home.logs(), before);
        released.store(true, Ordering::SeqCst);
        wait_turn(&mut next, &home, 1).await;
        let before = home.logs();
        drop(next);
        let mut next = open(&home, &id).await;
        assert_eq!(home.logs(), before);
        next.channels
            .commands
            .send(ClientCommand::send(
                "daemon-2",
                "a second local client turn",
            ))
            .await
            .unwrap();
        wait_turn(&mut next, &home, 2).await;
        drop(next);
        wait_unwindowed(&link::socket(&data(&home), &id)).await;
        let output = run(command(&home, &server, WISP).args(["quit", &id]), "");
        assert!(output.status.success(), "{}", both(&output));
        assert_eq!(
            out(&output),
            format!("Quit {}: it has ended.\n", wisp_protocol::short_id(&id))
        );
        assert!(wait_until(PATIENCE, || !alive(host.as_raw_nonzero().get())));
        assert!(
            client::daemon(&daemon_socket(&home))
                .await
                .unwrap()
                .chats()
                .await
                .unwrap()
                .is_empty()
        );
    });
    support::prefix::assert_prefix_stable(&requests(&server), None);
    assert!(connects(&home));
}

async fn open(home: &Home, id: &str) -> wisp_client::link::Link {
    let connection = wisp_client::link::daemon(&daemon_socket(home))
        .await
        .unwrap()
        .open(id.to_owned(), 0)
        .await
        .unwrap();
    wisp_client::link::open(connection).await.unwrap()
}

async fn wait_unwindowed(path: &std::path::Path) {
    tokio::time::timeout(PATIENCE, async {
        loop {
            if !wisp_client::link::connect(path, 0)
                .await
                .unwrap()
                .welcome
                .window
            {
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }
    })
    .await
    .unwrap();
}

async fn wait_turn(link: &mut wisp_client::link::Link, home: &Home, turn: u64) {
    tokio::time::timeout(PATIENCE, async {
        let mut started = false;
        loop {
            match link.channels.events.recv().await.unwrap() {
                wisp_protocol::AgentEvent::TurnStarted { turn: current, .. } => {
                    started = current == turn
                }
                wisp_protocol::AgentEvent::TurnEnded { .. } if started => break,
                _ => {}
            }
        }
    })
    .await
    .unwrap();
    settled(home);
}

#[test]
fn a_terminal_chat_can_be_opened_through_the_daemon_and_quit_from_outside() {
    if !outside_job(
        "hosted::daemon::a_terminal_chat_can_be_opened_through_the_daemon_and_quit_from_outside",
    ) || !usable()
    {
        return;
    }
    let home = Home::new("daemon-terminal");
    let server = serve();
    let mut first = start(&home, &server, &[]);
    turn(&mut first, &home);
    let path = socket(&home).unwrap();
    let id = link::session_of(&path).unwrap().to_owned();
    start_daemon(&home, &server);
    runtime().block_on(async {
        let connection = wisp_client::link::daemon(&daemon_socket(&home))
            .await
            .unwrap()
            .open(id.clone(), 0)
            .await
            .unwrap();
        assert!(connection.welcome.window);
        let link = wisp_client::link::open(connection).await.unwrap();
        first.wait_for("Opened in another window.");
        assert!(first.finish().0.success());
        drop(link);
        wait_unwindowed(&path).await;
    });
    assert!(quit_from_terminal(&home, &[&id]).0.success());
}

#[test]
fn a_job_cannot_stop_a_daemon_even_when_that_daemon_was_started_in_a_job() {
    let home = Home::new("daemon-job-peer");
    let server = serve();
    let start = run(
        command(&home, &server, WISP)
            .args(["daemon", "start"])
            .env(wisp_protocol::WISP_JOB, "1"),
        "",
    );
    assert!(start.status.success(), "{}", both(&start));
    assert!(!home.exists(STOPPED));
    let status = run(
        command(&home, &server, WISP)
            .arg("daemon")
            .env(wisp_protocol::WISP_JOB, "1"),
        "",
    );
    assert!(out(&status).starts_with("Daemon: running\n"));
    let stop = run(
        command(&home, &server, WISP)
            .args(["daemon", "stop"])
            .env(wisp_protocol::WISP_JOB, "1"),
        "",
    );
    assert!(!stop.status.success());
    assert!(connects(&home));
    assert!(!home.exists(STOPPED));
    assert!(daemon_command(&home, &server, &["stop"]).status.success());
}

#[test]
fn restart_replaces_a_running_daemon_without_a_stop_mark_and_leaves_a_stopped_one_alone() {
    for legacy in [false, true] {
        let home = Home::new("daemon-restart");
        let server = serve();
        let old = if legacy {
            let data = data(&home);
            let lock = wispd::daemon::lock(&data).unwrap();
            let path = daemon_socket(&home);
            let listener = UnixListener::bind(&path).unwrap();
            Some(std::thread::spawn(move || {
                for stream in listener.incoming() {
                    let mut stream = BufReader::new(stream.unwrap());
                    let mut line = String::new();
                    if stream.read_line(&mut line).unwrap() == 0 {
                        continue;
                    }
                    assert_eq!(
                        link::read::<ToDaemon>(&line),
                        Some(ToDaemon::Hello {
                            daemon: link::DAEMON
                        })
                    );
                    stream
                        .get_mut()
                        .write_all(
                            jsonl::line(&FromDaemon::Welcome {
                                daemon: link::DAEMON,
                            })
                            .unwrap()
                            .as_bytes(),
                        )
                        .unwrap();
                    line.clear();
                    assert!(stream.read_line(&mut line).unwrap() > 0);
                    assert_eq!(link::read::<ToDaemon>(&line), Some(ToDaemon::Stop));
                    drop(stream);
                    std::thread::sleep(std::time::Duration::from_millis(250));
                    break;
                }
                drop(listener);
                fs::remove_file(path).unwrap();
                std::thread::sleep(std::time::Duration::from_millis(250));
                drop(lock);
            }))
        } else {
            start_daemon(&home, &server);
            None
        };
        let before = daemon_pid(&home);
        let output = run(command(&home, &server, WISP).arg("restart"), "");
        if let Some(old) = old {
            old.join().unwrap();
        }
        assert!(output.status.success(), "{}", both(&output));
        assert_eq!(
            out(&output),
            "No chat is kept open.\nRestarted Wisp's daemon on the newest version.\n"
        );
        assert_ne!(daemon_pid(&home), before);
        assert!(!home.exists(STOPPED));
        assert!(daemon_command(&home, &server, &["stop"]).status.success());
        let output = run(command(&home, &server, WISP).arg("restart"), "");
        assert!(output.status.success(), "{}", both(&output));
        assert_eq!(out(&output), "No chat is kept open.\n");
        assert!(home.exists(STOPPED));
        assert!(!connects(&home));
    }
}
