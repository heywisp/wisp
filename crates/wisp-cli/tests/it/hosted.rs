#![cfg(target_os = "linux")]
#![allow(
    clippy::unwrap_used,
    clippy::panic,
    reason = "the harness fails the test on any local socket, pty or process error"
)]

mod daemon;

use std::fs;
use std::io::{BufRead, BufReader, Write};
use std::net::Shutdown;
use std::os::unix::fs::PermissionsExt;
use std::os::unix::net::{UnixListener, UnixStream};
use std::process::{Command, Stdio};
use std::sync::{Arc, Mutex, mpsc};
use std::time::{Duration, Instant};

use rustix::process::{Signal, kill_process};
use serde_json::{Value, json};
use wisp_protocol::link::{self, Ready, Start, ToClient, ToHost, Welcome};
use wisp_protocol::{ClientCommand, SessionView, jsonl};

use crate::scratch::alive;
use crate::support::{self, WISP};
use support::home::{CONFIG, Home, SESSIONS, command_in};
use support::hosted::{pid, socket};
use support::pty::{PATIENCE, Pty, READY, Stdout, usable, wait_for_exit, wait_until};
use support::server::{Server, counted, text_reply};

const MODEL: &str = "claude-sonnet-5-5";
const DONE: &str = "HOSTED-REPLY";
const HOST_KEY: &str = "sk-ant-CANARY-HOSTED-FAKE-KEY";
const ID: &str = "00000000-0000-7000-8000-000000000001";

fn serve() -> Server {
    Server::start(|request| {
        if request.head.contains("count_tokens") {
            counted()
        } else {
            text_reply("message", MODEL, DONE, (1, 5))
        }
    })
}

fn requests(server: &Server) -> Vec<Value> {
    server
        .heads()
        .iter()
        .zip(server.bodies())
        .filter(|(head, _)| !support::server::is_count(head))
        .map(|(_, body)| serde_json::from_str(&body).unwrap())
        .collect()
}

fn command(home: &Home, server: &Server, program: &str) -> Command {
    let mut command = command_in(&home.0, program);
    command
        .env("ANTHROPIC_API_KEY", HOST_KEY)
        .env("ANTHROPIC_BASE_URL", server.url());
    command
}

fn launch(home: &Home, server: &Server, args: &[&str]) -> Pty {
    launch_sized(home, server, args, 100)
}

fn launch_sized(home: &Home, server: &Server, args: &[&str], columns: u16) -> Pty {
    Pty::start(
        &mut window_command(home, server, args),
        30,
        columns,
        Stdout::Tty,
    )
}

fn window_command(home: &Home, server: &Server, args: &[&str]) -> Command {
    let mut command = command(home, server, "setsid");
    command
        .args(["--ctty", "--wait", WISP, "--model", MODEL, "--time", "off"])
        .args(args)
        .env("TERM", "xterm-256color");
    command
}

fn start(home: &Home, server: &Server, args: &[&str]) -> Pty {
    let pty = launch(home, server, args);
    pty.wait_for(READY);
    assert!(
        socket(home).is_some(),
        "the chat must be hosted: {}",
        pty.shown()
    );
    pty
}

fn completed(home: &Home) -> usize {
    home.records()
        .iter()
        .filter(|record| record["type"] == "turn_ended")
        .count()
}

fn turn(pty: &mut Pty, home: &Home) {
    let next = completed(home) + 1;
    pty.send(b"a short local test\r");
    assert!(wait_until(PATIENCE, || completed(home) >= next));
    pty.wait_for(DONE);
    pty.wait_idle();
    settled(home);
}

fn settled(home: &Home) {
    assert!(
        wait_until(PATIENCE, || is_settled(&home.records())),
        "after-turn hooks did not finish"
    );
}

fn is_settled(records: &[Value]) -> bool {
    use wisp_plugin_api::manifest::{Manifest, Origin};
    let mut hooks = Vec::new();
    for bundled in wisp_plugins::bundled() {
        let manifest = Manifest::parse(bundled.manifest, Origin::Bundled).unwrap();
        for hook in manifest.hooks {
            if hook.point == "after-turn" && !hook.background {
                hooks.push((
                    manifest.id.clone(),
                    format!("{}/{}", manifest.id, hook.name),
                ));
            }
        }
    }
    let mut loaded = Vec::new();
    let mut waiting = Vec::new();
    for record in records {
        match record["type"].as_str() {
            Some("session_started") => {
                if !waiting.is_empty() {
                    return false;
                }
                loaded = record["plugins"]
                    .as_array()
                    .into_iter()
                    .flatten()
                    .filter_map(|plugin| plugin["id"].as_str())
                    .collect::<Vec<_>>();
            }
            Some("turn_ended") => {
                waiting = hooks
                    .iter()
                    .filter(|(id, _)| loaded.contains(&id.as_str()))
                    .map(|(_, hook)| hook.clone())
                    .collect();
            }
            Some("hook_ran") if record["point"] == "after-turn" => {
                waiting.retain(|hook| record["hook"].as_str() != Some(hook.as_str()));
            }
            _ => {}
        }
    }
    waiting.is_empty()
}

#[test]
fn a_turn_with_no_loaded_after_turn_hook_is_settled_at_its_end() {
    assert!(is_settled(&[
        json!({"type": "session_started", "plugins": []}),
        json!({"type": "turn_ended"}),
    ]));
}

fn quit(mut pty: Pty) {
    pty.send(b"/quit\r");
    let status = pty.finish().0;
    assert!(status.success(), "{status}");
}

#[test]
fn a_pinned_chat_starts_in_the_window_when_socket_publication_fails() {
    if !usable() {
        return;
    }
    use std::ffi::OsStr;
    use std::os::unix::{ffi::OsStrExt, fs::symlink};

    let home = Home::bare("pin-window");
    let data = home.0.join(OsStr::from_bytes(b"d\xff"));
    let blocked = link::socket(&data.join("wisp"), ID);
    fs::create_dir_all(&blocked).unwrap();
    symlink(&data, home.dir(".local").join("share")).unwrap();
    home.write(support::home::DAEMON_STOPPED, "");
    let decoy = home.write(&format!("d\u{fffd}/wisp/sessions/{ID}.jsonl"), "");
    let server = serve();
    for _ in 0..2 {
        let mut command = window_command(&home, &server, &["--session-id", ID]);
        command.env("XDG_DATA_HOME", &data);
        let mut window = Pty::start(&mut command, 30, 120, Stdout::Tty);
        window.wait_for("Is a directory (os error 21)");
        window.wait_for(READY);
        turn(&mut window, &home);
        quit(window);
    }
    assert!(blocked.is_dir());
    assert_eq!(fs::metadata(decoy).unwrap().len(), 0);
    assert_eq!(completed(&home), 2);
    assert!(
        home.records()
            .iter()
            .any(|record| record["type"] == "session_resumed")
    );
}

#[test]
fn a_chat_survives_its_window_and_quit_releases_the_log_before_returning() {
    if !usable() {
        return;
    }
    let home = Home::new("host-survive");
    let server = serve();
    let mut first = start(&home, &server, &["--"]);
    turn(&mut first, &home);
    let host = pid(&home);
    let path = socket(&home).unwrap();
    let id = link::session_of(&path).unwrap().to_owned();
    first.child.kill().unwrap();
    first.child.wait().unwrap();
    drop(first);
    assert!(alive(host.as_raw_nonzero().get()));
    let second = start(&home, &server, &["--resume", &id]);
    second.wait_for(DONE);
    quit(second);
    assert!(wait_until(PATIENCE, || !alive(host.as_raw_nonzero().get())));
    let store = wisp_core::session::JsonlStore::open(home.file(SESSIONS)).unwrap();
    assert!(store.probe(&id).is_ok());
    assert!(!path.exists());
    assert!(!link::error_file(&path).exists());
}

#[test]
fn leaving_returns_to_the_shell_without_ending_or_changing_the_chat() {
    if !usable() {
        return;
    }
    let home = Home::new("host-leave");
    let server = serve();
    let mut first = start(&home, &server, &[]);
    turn(&mut first, &home);
    let host = pid(&home);
    let path = socket(&home).unwrap();
    let id = link::session_of(&path).unwrap();
    let before = home.logs();
    first.send(b"/leave\r");
    first.wait_for("The chat stays open.");
    first.wait_for("Come back any time with:");
    first.wait_for(format!("wisp --resume {}", wisp_protocol::short_id(id)));
    assert!(!first.screen().contains("That was"));
    assert!(first.finish().0.success());
    assert!(alive(host.as_raw_nonzero().get()));
    assert_eq!(home.logs(), before);
    let mut second = start(&home, &server, &["--resume", id]);
    turn(&mut second, &home);
    support::prefix::assert_prefix_stable(&requests(&server), None);
    quit(second);
}

fn from_terminal(
    home: &Home,
    command: &str,
    ids: &[&str],
) -> (std::process::ExitStatus, String, String) {
    let mut wisp = Pty::command(&home.0);
    wisp.current_dir(home.dir("another-folder"))
        .arg(command)
        .args(ids);
    let pty = Pty::start(&mut wisp, 30, 100, Stdout::Piped);
    let errors = Arc::clone(&pty.screen);
    let (status, stdout) = pty.finish();
    let stderr = String::from_utf8_lossy(&errors.lock().unwrap()).replace("\r\n", "\n");
    (status, stdout, stderr)
}

#[test]
fn quitting_a_left_chat_waits_for_its_host_and_log_and_names_a_second_quit() {
    if !usable() {
        return;
    }
    let home = Home::new("host-quit-outside");
    let server = serve();
    let mut first = start(&home, &server, &[]);
    turn(&mut first, &home);
    let host = pid(&home);
    let path = socket(&home).unwrap();
    let id = link::session_of(&path).unwrap();
    let short = wisp_protocol::short_id(id);
    first.send(b"/leave\r");
    assert!(first.finish().0.success());
    let (status, stdout, stderr) = from_terminal(&home, "quit", &[&id[..8].to_ascii_uppercase()]);
    assert!(status.success());
    assert_eq!(stdout, format!("Quit {short}: it has ended.\n"));
    assert!(stderr.is_empty(), "{stderr}");
    assert!(wait_until(PATIENCE, || !alive(host.as_raw_nonzero().get())));
    let store = wisp_core::session::JsonlStore::open(home.file(SESSIONS)).unwrap();
    assert!(store.probe(id).is_ok());
    assert!(!path.exists());
    let (status, stdout, stderr) = from_terminal(&home, "quit", &[&id[..8], "5e6f"]);
    assert!(!status.success());
    assert!(stdout.is_empty());
    assert_eq!(
        stderr,
        format!(
            "wisp: {short} already ended; nothing to quit\nwisp: no session's ID starts with 5e6f\n"
        )
    );
    let (status, stdout, stderr) = from_terminal(&home, "restart", &[&id[..8], "5e6f"]);
    assert_eq!(status.code(), Some(1));
    assert!(stdout.is_empty(), "{stdout}");
    assert_eq!(
        stderr,
        format!(
            "wisp: {short} already ended; nothing to restart\nwisp: no session's ID starts with 5e6f\n"
        )
    );
}

#[test]
fn quitting_from_outside_leaves_both_kinds_of_window_alone() {
    use wisp_core::session::SessionStore;
    if !usable() {
        return;
    }
    let server = serve();
    for hosted in [false, true] {
        let home = Home::new("host-quit-window");
        if !hosted {
            home.write(".local/share/wisp/run", "unusable");
        }
        let mut first = launch(&home, &server, &[]);
        first.wait_for(READY);
        turn(&mut first, &home);
        let store = wisp_core::session::JsonlStore::open(home.file(SESSIONS)).unwrap();
        let id = store.list().unwrap().remove(0);
        let (status, stdout, stderr) = from_terminal(&home, "quit", &[&id]);
        assert!(!status.success());
        assert!(stdout.is_empty());
        assert_eq!(
            stderr,
            format!(
                "wisp: {} is open in a window; quit it there\n",
                wisp_protocol::short_id(&id)
            )
        );
        if !hosted {
            let (status, stdout, stderr) = from_terminal(&home, "restart", &[&id]);
            assert_eq!(status.code(), Some(1));
            assert!(stdout.is_empty(), "{stdout}");
            assert_eq!(
                stderr,
                format!(
                    "wisp: {} lives in its window; quit it there and open it again\n",
                    wisp_protocol::short_id(&id)
                )
            );
        }
        assert!(first.child.try_wait().unwrap().is_none());
        assert!(!first.saw("Opened in another window."));
        quit(first);
    }
}

#[test]
fn taking_a_chat_sends_the_first_window_to_the_shell() {
    if !usable() {
        return;
    }
    use wisp_render::transcript::copy;
    let home = Home::new("host-taken");
    let server = serve();
    let mut first = start(&home, &server, &[]);
    turn(&mut first, &home);
    first.send(b"an unsent draft");
    first.wait_shown("an unsent draft");
    let path = socket(&home).unwrap();
    let id = link::session_of(&path).unwrap();
    let second = start(&home, &server, &["--resume", id]);
    first.wait_for("Opened in another window.");
    first.wait_for("Bring it back here with:");
    assert!(wait_for_exit(&mut first.child).success());
    let shown = first.wait_screen("the unsent block", |shown| {
        shown
            .split_once(copy::NOT_SENT)
            .is_some_and(|(_, unsent)| unsent.trim() == "an unsent draft")
    });
    let Some((closing, unsent)) = shown.split_once(copy::NOT_SENT) else {
        panic!("unsent heading missing: {shown}");
    };
    assert!(closing.contains(copy::CHAT_TAKEN), "{shown}");
    assert_eq!(unsent.trim(), "an unsent draft", "{shown}");
    assert!(second.saw(DONE));
    quit(second);
}

#[test]
fn the_list_never_opens_a_kept_chat_without_being_asked() {
    if !usable() {
        return;
    }
    let home = Home::new("host-list");
    let server = serve();
    let mut first = start(&home, &server, &[]);
    turn(&mut first, &home);
    let mut picker = launch(&home, &server, &["--resume"]);
    picker.wait_for("kept open");
    assert!(first.child.try_wait().unwrap().is_none());
    picker.send(b"\r");
    picker.wait_for(DONE);
    picker.wait_idle();
    first.wait_for("Opened in another window.");
    quit(picker);
}

#[test]
fn switching_chats_leaves_the_chat_left_running() {
    if !usable() {
        return;
    }
    let home = Home::new("host-switch");
    let server = serve();
    let mut first = start(&home, &server, &[]);
    first.send(b"first chat\r");
    first.wait_for(DONE);
    first.wait_idle();
    let first_path = socket(&home).unwrap();
    let mut second = start(&home, &server, &[]);
    second.send(b"second chat\r");
    second.wait_for(DONE);
    second.wait_idle();
    let second_path = fs::read_dir(home.file(".local/share/wisp/run"))
        .unwrap()
        .flatten()
        .map(|entry| entry.path())
        .find(|path| path != &first_path && link::session_of(path).is_some())
        .unwrap();
    let second_id = link::session_of(&second_path).unwrap().to_owned();
    let stream = std::os::unix::net::UnixStream::connect(&second_path).unwrap();
    let second_pid = crate::scratch::peer_pid(stream).unwrap();
    second.send(b"/resume\r");
    second.wait_for("kept open");
    second.send(b"\r");
    first.wait_for("Opened in another window.");
    second.wait_screen("the first chat", |shown| {
        shown.contains("first chat") && !shown.contains("kept open")
    });
    second.wait_idle();
    assert!(alive(second_pid.as_raw_nonzero().get()));
    quit(second);
    assert!(alive(second_pid.as_raw_nonzero().get()));
    let reopened = start(&home, &server, &["--resume", &second_id]);
    reopened.wait_for("second chat");
    quit(reopened);
}
#[test]
fn a_chat_without_a_log_names_why_it_lives_in_its_window() {
    if !usable() {
        return;
    }
    let home = Home::new("host-window");
    let server = serve();
    let mut pty = launch(&home, &server, &["--no-log"]);
    let note = "This chat can't stay open without its window: the session log is off.";
    pty.wait_for(note);
    pty.wait_for(READY);
    pty.send(b"/leave\r");
    pty.wait_for_count(note, 2);
    assert!(pty.child.try_wait().unwrap().is_none());
    assert!(socket(&home).is_none());
    quit(pty);
}

#[test]
fn an_open_window_follows_its_chat_through_a_restart_without_losing_its_draft() {
    if !usable() {
        return;
    }
    use std::sync::atomic::{AtomicUsize, Ordering};
    use wisp_render::transcript::copy;
    const WAITING: &str = "deliver this waiting message exactly once";
    for (restarting, draft) in [(false, false), (true, false), (true, true)] {
        let home = Home::new("host-window-restart");
        let calls = AtomicUsize::new(0);
        let server = Server::start(move |request| {
            if request.head.contains("count_tokens") {
                counted()
            } else if calls.fetch_add(1, Ordering::SeqCst) == 0 {
                support::server::tool_reply(
                    "waiting-at-restart",
                    "bash",
                    &json!({"command": "sleep 60", "description": "Wait for the restart"}),
                )
            } else {
                text_reply("message", MODEL, DONE, (1, 5))
            }
        });
        let mut pty = start(&home, &server, &[]);
        pty.send(b"start the work\r");
        pty.wait_for("Wait for the restart");
        let path = socket(&home).unwrap();
        let previous = pid(&home);
        let mut restart = restarting.then(|| {
            let restart = Pty::start(Pty::command(&home.0).arg("restart"), 24, 100, Stdout::Tty);
            restart.wait_for("finishing: Wait for the restart");
            restart
        });
        pty.send(format!("{WAITING}\t").as_bytes());
        assert!(wait_until(PATIENCE, || home.records().iter().any(
            |record| record["type"] == "input_accepted" && record["text"] == WAITING
        )));
        if restarting {
            pty.wait_screen("the accepted message waiting after restart", |shown| {
                shown.contains("after restart") && shown.contains(WAITING)
            });
        }
        if draft {
            pty.send(b"an unsent draft");
            pty.wait_shown("an unsent draft");
        }
        if let Some(restart) = &mut restart {
            restart.send(b"\r");
        } else {
            runtime().block_on(async {
                assert!(
                    wisp_client::link::connect(&path, 0)
                        .await
                        .unwrap()
                        .end(false)
                        .await
                        .unwrap()
                );
            });
            assert!(wait_for_exit(&mut pty.child).success());
            pty.wait_for(copy::BYE);
            pty.wait_for(copy::RESUME_COMMAND);
            let shown = pty.shown();
            assert!(!shown.contains(WAITING), "{shown}");
            assert!(!shown.contains("an unsent draft"), "{shown}");
            assert!(!pty.saw(copy::CHAT_LOST));
            assert!(!pty.saw(copy::WINDOW_RESTARTING));
            assert_eq!(requests(&server).len(), 1);
            let id = link::session_of(&path).unwrap();
            let output = support::home::run(
                command(&home, &server, WISP).args([
                    "--model", MODEL, "--time", "off", "--resume", id, "--output", "jsonl",
                ]),
                "a non-window turn\n",
            );
            assert!(
                output.status.success(),
                "{}{}",
                support::home::out(&output),
                support::home::err(&output)
            );
            assert_eq!(requests(&server).len(), 2);
            let mut resumed = launch(&home, &server, &["--resume", id]);
            resumed.wait_shown(WAITING);
            resumed.wait_shown("Your message from before Wisp stopped");
            assert_eq!(requests(&server).len(), 2);
            let original = home
                .records()
                .into_iter()
                .find(|r| r["type"] == "input_accepted" && r["text"] == WAITING)
                .unwrap()["id"]
                .clone();
            assert!(wait_until(PATIENCE, || home
                .records()
                .iter()
                .filter(|r| r["type"] == "returned_taken" && r["ids"] == json!([original]))
                .count()
                == 1));
            resumed.send(b"\x01\x0b");
            quit(resumed);
            let again = launch(&home, &server, &["--resume", id]);
            again.wait_idle();
            let shown = again.screen();
            assert!(!shown.contains(WAITING), "{shown}");
            assert!(
                !shown.contains("Your message from before Wisp stopped"),
                "{shown}"
            );
            quit(again);
            assert_eq!(requests(&server).len(), 2);
            continue;
        }
        assert!(restart.unwrap().finish().0.success());
        pty.wait_for(copy::WINDOW_RESTARTING);
        pty.wait_for(copy::WINDOW_RESTARTED);
        assert!(!pty.saw(copy::CHAT_LOST));
        assert!(!pty.saw(copy::WINDOW_REOPENED));
        assert!(pty.child.try_wait().unwrap().is_none());
        assert_ne!(pid(&home), previous);
        assert!(wait_until(PATIENCE, || requests(&server).iter().any(
            |request| request["messages"].to_string().contains(WAITING)
        )));
        if draft {
            pty.wait_screen("the restart note and preserved draft", |shown| {
                shown.contains("This window still runs the older")
                    && shown.contains("an unsent draft")
            });
            assert!(
                requests(&server)
                    .iter()
                    .all(|request| !request["messages"].to_string().contains("an unsent draft"))
            );
            pty.send(b"\x01\x0b");
        }
        pty.send(b"after the restart\r");
        assert!(wait_until(PATIENCE, || requests(&server).iter().any(
            |request| request["messages"]
                .to_string()
                .contains("after the restart")
        )));
        pty.wait_idle();
        let records = home
            .records()
            .into_iter()
            .map(serde_json::from_value)
            .collect::<Result<Vec<wisp_protocol::log::LogRecord>, _>>()
            .unwrap();
        let original = records
            .iter()
            .find_map(|record| match &record.event {
                wisp_protocol::log::LogEvent::InputAccepted { id, text, .. } if text == WAITING => {
                    Some(id)
                }
                _ => None,
            })
            .unwrap();
        let replay =
            wisp_core::resume::transcript(&records, &wisp_core::tool::Toolbox::new([]), None);
        let resent = replay
            .iter()
            .filter_map(|event| match event {
                wisp_protocol::AgentEvent::InputAccepted { id, text, .. } if text == WAITING => {
                    Some(id)
                }
                _ => None,
            })
            .collect::<Vec<_>>();
        assert_eq!(resent.len(), 1, "{resent:?}");
        assert_ne!(original, resent[0]);
        assert_eq!(records.iter().filter(|record| matches!(&record.event, wisp_protocol::log::LogEvent::ReturnedTaken { ids } if ids == std::slice::from_ref(original))).count(), 1);
        assert_eq!(records.iter().filter(|record| matches!(&record.event, wisp_protocol::log::LogEvent::InputWithdrawn { ids, by: wisp_protocol::WithdrawnBy::SessionEnded, .. } if ids.contains(original))).count(), 1);
        let requests = requests(&server);
        assert!(
            requests
                .iter()
                .all(|request| request["messages"].to_string().matches(WAITING).count() <= 1)
        );
        assert_eq!(
            requests.last().unwrap()["messages"]
                .to_string()
                .matches(WAITING)
                .count(),
            1
        );
        quit(pty);
    }
}

#[test]
fn losing_the_host_names_the_failure_and_the_resume_command() {
    if !usable() {
        return;
    }
    use wisp_render::transcript::copy;
    for signal in [Signal::KILL, Signal::TERM] {
        let home = Home::new("host-lost");
        let server = serve();
        let mut pty = start(&home, &server, &[]);
        turn(&mut pty, &home);
        pty.send(b"an unsent draft");
        pty.wait_shown("an unsent draft");
        let host = pid(&home);
        let path = socket(&home).unwrap();
        fs::write(
            link::error_file(&path),
            "placeholder error line one\nplaceholder error line two\n",
        )
        .unwrap();
        kill_process(host, signal).unwrap();
        pty.wait_for(if signal == Signal::KILL {
            copy::CHAT_LOST
        } else {
            copy::BYE
        });
        pty.wait_for(copy::RESUME_COMMAND);
        assert_eq!(
            wait_for_exit(&mut pty.child).success(),
            signal == Signal::TERM
        );
        let shown = pty.wait_screen("the unsent block", |shown| {
            shown
                .split_once(copy::NOT_SENT)
                .is_some_and(|(_, unsent)| unsent.trim() == "an unsent draft")
        });
        let Some((closing, unsent)) = shown.split_once(copy::NOT_SENT) else {
            panic!("unsent heading missing: {shown}");
        };
        assert_eq!(unsent.trim(), "an unsent draft", "{shown}");
        assert!(closing.contains(copy::RESUME_COMMAND), "{shown}");
        if signal == Signal::KILL {
            assert!(
                closing.find("placeholder error line two").unwrap()
                    < closing.find(copy::CHAT_LOST).unwrap()
            );
        } else {
            assert!(!closing.contains("placeholder error line"), "{shown}");
            assert!(!closing.contains(copy::CHAT_LOST), "{shown}");
        }
    }
}

fn runtime() -> tokio::runtime::Runtime {
    tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap()
}

fn headless(
    home: &Home,
    server: &Server,
    program: &str,
    args: &[&str],
    start: &Start,
) -> (std::process::Child, std::path::PathBuf) {
    let mut command = command(home, server, program);
    let mut child = command
        .args(["--host", "--model", MODEL, "--time", "off"])
        .args(args)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    child
        .stdin
        .take()
        .unwrap()
        .write_all(jsonl::line(start).unwrap().as_bytes())
        .unwrap();
    let mut ready = BufReader::new(child.stdout.take().unwrap());
    let mut line = String::new();
    ready.read_line(&mut line).unwrap();
    let Ready::Socket { path } = link::read(&line).unwrap() else {
        panic!("host didn't start");
    };
    (child, path)
}

#[test]
fn a_client_that_is_not_the_tui_starts_and_runs_a_chat() {
    let home = Home::new("host-client");
    let server = serve();
    let (mut child, path) = headless(&home, &server, WISP, &[], &Start::default());
    runtime().block_on(async {
        let mut client = wisp_client::link::open(wisp_client::link::connect(&path, 0).await.unwrap()).await.unwrap();
        client.channels.commands.send(ClientCommand::send("test-client-1", "a local client turn")).await.unwrap();
        let mut events = Vec::new();
        while let Some(event) = client.channels.events.recv().await {
            let ended = matches!(event, wisp_protocol::AgentEvent::TurnEnded { .. });
            events.push(event);
            if ended { break; }
        }
        assert!(events.iter().any(|event| matches!(event, wisp_protocol::AgentEvent::TextDelta { text, .. } if text.contains(DONE))));
        assert!(client.end(false).await.unwrap());
    });
    assert!(wait_for_exit(&mut child).success());
}

#[test]
fn a_chat_restarts_from_its_original_path_after_the_program_is_replaced_by_rename() {
    if !daemon::outside_job(
        "hosted::a_chat_restarts_from_its_original_path_after_the_program_is_replaced_by_rename",
    ) {
        return;
    }
    let home = Home::new("host-replaced");
    let server = serve();
    let program = home.file("wisp-test");
    fs::copy(WISP, &program).unwrap();
    let (mut child, path) = headless(
        &home,
        &server,
        program.to_str().unwrap(),
        &[],
        &Start::default(),
    );
    let replacement = home.file("wisp-next");
    fs::copy(WISP, &replacement).unwrap();
    let file = fs::File::open(&replacement).unwrap();
    file.set_modified(std::time::SystemTime::now() + std::time::Duration::from_secs(1))
        .unwrap();
    fs::rename(&replacement, &program).unwrap();
    runtime().block_on(async {
        let mut peer = wisp_client::link::connect(&path, 0).await.unwrap();
        let previous = peer.welcome.build;
        peer.restart(true).await.unwrap();
        let mut restarting = false;
        loop {
            match tokio::time::timeout(PATIENCE, peer.next())
                .await
                .unwrap()
                .unwrap()
            {
                ToClient::Restarting => restarting = true,
                ToClient::Ended { saved } => {
                    assert!(saved && restarting);
                    break;
                }
                _ => {}
            }
        }
        let next = tokio::time::timeout(PATIENCE, async {
            loop {
                if let Ok(peer) = wisp_client::link::connect(&path, 0).await {
                    break peer;
                }
                tokio::time::sleep(std::time::Duration::from_millis(20)).await;
            }
        })
        .await
        .unwrap();
        assert!(next.welcome.build > previous);
        let mut client = wisp_client::link::open(next).await.unwrap();
        client
            .channels
            .commands
            .send(ClientCommand::send(
                "test-after-rename",
                "a local turn after restart",
            ))
            .await
            .unwrap();
        let mut replied = false;
        while let Some(event) = tokio::time::timeout(PATIENCE, client.channels.events.recv())
            .await
            .unwrap()
        {
            if let wisp_protocol::AgentEvent::TextDelta { text, .. } = &event {
                replied |= text.contains(DONE);
            }
            if matches!(event, wisp_protocol::AgentEvent::TurnEnded { .. }) {
                break;
            }
        }
        assert!(replied);
        assert!(client.end(false).await.unwrap());
    });
    assert!(wait_for_exit(&mut child).success());
}

#[test]
fn a_next_host_that_fails_leaves_its_reason_in_a_private_file_without_its_key() {
    let home = Home::new("host-restart-failed");
    let server = serve();
    let program = home.file("wisp-test");
    fs::copy(WISP, &program).unwrap();
    let (mut child, path) = headless(
        &home,
        &server,
        program.to_str().unwrap(),
        &[],
        &Start::default(),
    );
    let failure = jsonl::line(&Ready::Failed {
        text: format!("placeholder start failure {HOST_KEY}"),
    })
    .unwrap();
    let replacement = home.write(
        "wisp-next",
        &format!("#!/bin/sh\nIFS= read -r start\nprintf '%s' '{failure}'\n"),
    );
    fs::set_permissions(&replacement, fs::Permissions::from_mode(0o700)).unwrap();
    fs::rename(replacement, program).unwrap();
    let began = std::time::Instant::now();
    let output = command(&home, &server, WISP)
        .arg("restart")
        .output()
        .unwrap();
    assert!(!output.status.success());
    let shown = support::home::out(&output);
    assert!(shown.contains("placeholder start failure"), "{shown}");
    assert!(shown.contains("wisp --resume"), "{shown}");
    assert!(!shown.contains("CANARY"));
    assert!(began.elapsed() < std::time::Duration::from_secs(10));
    assert!(wait_for_exit(&mut child).success());
    let error = link::error_file(&path);
    let reason = fs::read_to_string(&error).unwrap();
    assert!(reason.contains("placeholder start failure"), "{reason}");
    assert!(!reason.contains("CANARY"));
    assert_eq!(
        fs::metadata(error).unwrap().permissions().mode() & 0o777,
        0o600
    );
    assert!(!path.exists());
}

fn fake_chat(
    home: &Home,
    build: u64,
    saved: Option<bool>,
    window: bool,
    windows: Option<(mpsc::Sender<UnixStream>, mpsc::Receiver<bool>)>,
    restart: bool,
) -> std::thread::JoinHandle<()> {
    let data = home.dir(".local/share/wisp");
    if !home.exists(&format!("{SESSIONS}/{ID}.jsonl")) {
        home.write(&format!("{SESSIONS}/{ID}.jsonl"), "");
    }
    let folder = home.0.clone();
    let received = home.file("fake-host-commands");
    let waiting = home.file("fake-host-wait");
    let silent = home.file("fake-host-silent");
    let release = home.file("fake-host-release");
    let sent = home.file("fake-host-sent");
    let knows = home.file("fake-host-knows");
    let other = home.file("fake-host-other");
    fs::create_dir_all(link::run_dir(&data)).unwrap();
    let listener = UnixListener::bind(link::socket(&data, ID)).unwrap();
    std::thread::spawn(move || {
        let mut input_ids = Vec::new();
        for stream in listener.incoming() {
            let mut writer = stream.unwrap();
            let mut reader = BufReader::new(writer.try_clone().unwrap());
            let mut line = String::new();
            if reader.read_line(&mut line).unwrap() == 0 {
                continue;
            }
            assert!(matches!(link::read(&line), Some(ToHost::Hello { .. })));
            if silent.exists() {
                assert!(wait_until(PATIENCE, || release.exists()));
                return;
            }
            let frame = ToClient::Welcome(Welcome {
                contract: link::CONTRACT + u32::from(other.exists()),
                build,
                wisp: "test".into(),
                session: ID.into(),
                first_request: wisp_protocol::RequestId(1),
                folder: folder.clone(),
                window,
                restart,
            });
            if writer
                .write_all(jsonl::line(&frame).unwrap().as_bytes())
                .is_err()
            {
                continue;
            }
            if other.exists() {
                return;
            }
            loop {
                line.clear();
                if reader.read_line(&mut line).unwrap() == 0 {
                    break;
                }
                fs::OpenOptions::new()
                    .create(true)
                    .append(true)
                    .open(&received)
                    .unwrap()
                    .write_all(b"frame\n")
                    .unwrap();
                match link::read(&line) {
                    Some(ToHost::Open) => {
                        let mut opening: wisp_protocol::Opening = serde_json::from_str(
                            include_str!("../../../wisp-protocol/tests/fixtures/opening.json"),
                        )
                        .unwrap();
                        opening.start = None;
                        opening.start_pick = None;
                        opening.resumed = None;
                        opening.next = None;
                        writer
                            .write_all(
                                jsonl::line(&ToClient::Opened {
                                    opening: Box::new(opening),
                                    view: SessionView {
                                        input_ids: if knows.exists() {
                                            input_ids.clone()
                                        } else {
                                            Vec::new()
                                        },
                                        ..Default::default()
                                    },
                                    now: None,
                                })
                                .unwrap()
                                .as_bytes(),
                            )
                            .unwrap();
                        if let Some((windows, ending)) = &windows {
                            windows.send(writer.try_clone().unwrap()).unwrap();
                            if ending.recv().unwrap() {
                                return;
                            }
                        }
                    }
                    Some(ToHost::Command(ClientCommand::Send { id, .. })) => {
                        input_ids.push(id.0);
                        fs::write(&sent, input_ids.join("\n")).unwrap();
                    }
                    Some(ToHost::Restart { .. }) if restart => return,
                    Some(ToHost::End { .. }) => {
                        if waiting.exists() {
                            assert!(wait_until(PATIENCE, || release.exists()));
                        }
                        if let Some(saved) = saved {
                            writer
                                .write_all(
                                    jsonl::line(&ToClient::Ended { saved }).unwrap().as_bytes(),
                                )
                                .unwrap();
                        }
                        return;
                    }
                    _ => {}
                }
            }
        }
    })
}

struct RestartWindow {
    pty: Pty,
    first: UnixStream,
    opened: mpsc::Receiver<UnixStream>,
    ending: mpsc::Sender<bool>,
    host: std::thread::JoinHandle<()>,
}

fn restarting_window(home: &Home, server: &Server) -> RestartWindow {
    use wisp_protocol::{AgentEvent, DeliveryMode, WithdrawnBy};
    let doctor = command(home, server, WISP)
        .args(["--model", MODEL, "--time", "off", "--resume", ID, "doctor"])
        .env(wisp_protocol::WISP_JOB, "1")
        .output()
        .unwrap();
    assert!(doctor.status.success(), "{}", support::home::both(&doctor));
    let (windows, opened) = mpsc::channel();
    let (ending, ended) = mpsc::channel();
    let host = fake_chat(home, 0, Some(true), false, Some((windows, ended)), true);
    let mut pty = Pty::start(
        window_command(home, server, &["--resume", ID]).env(wisp_protocol::WISP_JOB, "1"),
        30,
        100,
        Stdout::Tty,
    );
    pty.wait_for(READY);
    let mut first = opened.recv_timeout(PATIENCE).unwrap();
    first.set_read_timeout(Some(PATIENCE)).unwrap();
    let mut reader = BufReader::new(first.try_clone().unwrap());
    first
        .write_all(
            jsonl::line(&ToClient::event(AgentEvent::Restart {
                state: wisp_protocol::RestartState::GettingReady,
                open: Vec::new(),
            }))
            .unwrap()
            .as_bytes(),
        )
        .unwrap();
    pty.wait_shown("About to restart");
    pty.send(b"waiting message\r");
    let mut line = String::new();
    let (id, text) = loop {
        line.clear();
        assert_ne!(reader.read_line(&mut line).unwrap(), 0);
        if let Some(ToHost::Command(ClientCommand::Send { id, text, .. })) = link::read(&line) {
            break (id, text);
        }
    };
    for event in [
        AgentEvent::InputAccepted {
            opening: false,
            front: false,
            id: id.clone(),
            mode: DeliveryMode::Steer,
            text,
            images: Vec::new(),
            replaces: None,
            held: true,
            from: None,
            checklist_spans: Vec::new(),
        },
        AgentEvent::InputWithdrawn {
            ids: vec![id],
            by: WithdrawnBy::SessionEnded,
        },
    ] {
        first
            .write_all(jsonl::line(&ToClient::event(event)).unwrap().as_bytes())
            .unwrap();
    }
    pty.send(b"unconfirmed message\runsent draft");
    pty.wait_shown("unsent draft");
    loop {
        line.clear();
        assert_ne!(reader.read_line(&mut line).unwrap(), 0);
        if matches!(link::read(&line), Some(ToHost::Command(ClientCommand::Send { text, .. })) if text == "unconfirmed message")
        {
            break;
        }
    }
    RestartWindow {
        pty,
        first,
        opened,
        ending,
        host,
    }
}

#[test]
fn interrupting_a_restart_resend_reopens_the_window_with_its_unsent_text() {
    if !usable() {
        return;
    }
    use wisp_render::transcript::copy;
    let home = Home::new("host-resend-interrupted");
    let server = serve();
    let mut window = restarting_window(&home, &server);
    let mut line = String::new();
    window
        .first
        .write_all(jsonl::line(&ToClient::Restarting).unwrap().as_bytes())
        .unwrap();
    window.first.shutdown(Shutdown::Both).unwrap();
    window.ending.send(false).unwrap();
    let second = window.opened.recv_timeout(PATIENCE).unwrap();
    second.set_read_timeout(Some(PATIENCE)).unwrap();
    let mut reader = BufReader::new(second);
    loop {
        line.clear();
        assert_ne!(reader.read_line(&mut line).unwrap(), 0);
        if let Some(ToHost::Command(ClientCommand::Send { text, .. })) = link::read(&line) {
            assert_eq!(text, "waiting message");
            break;
        }
    }
    window.pty.send(b"\x03");
    assert!(wait_until(PATIENCE, || {
        window.pty.child.try_wait().unwrap().is_some()
            || window.pty.shown().contains(copy::WINDOW_RESTARTED)
    }));
    if window.pty.child.try_wait().unwrap().is_some() {
        window.ending.send(true).unwrap();
        window.host.join().unwrap();
        panic!(
            "Ctrl-C discarded the window's unconfirmed message and unsent draft: {}",
            window.pty.shown()
        );
    }
    window.pty.wait_shown("waiting message");
    window.pty.wait_shown("unconfirmed message");
    window.pty.wait_shown("unsent draft");
    window.pty.send(b"\x03/leave\r");
    assert!(wait_for_exit(&mut window.pty.child).success());
    window.ending.send(true).unwrap();
    window.host.join().unwrap();
    assert!(requests(&server).is_empty());
}

fn a_restart_reconnect_keeps_its_unsent_text(interrupt: bool) {
    if !usable() {
        return;
    }
    use wisp_render::transcript::copy;

    let home = Home::new("host-reconnect-unsent");
    let server = serve();
    let mut window = restarting_window(&home, &server);
    window.ending.send(true).unwrap();
    window.host.join().unwrap();
    if !interrupt {
        let socket = link::socket(&home.file(".local/share/wisp"), ID);
        fs::write(link::error_file(&socket), "placeholder restart failure\n").unwrap();
    }
    window
        .first
        .write_all(jsonl::line(&ToClient::Restarting).unwrap().as_bytes())
        .unwrap();
    window.first.shutdown(Shutdown::Both).unwrap();
    window.pty.wait_for(copy::WINDOW_RESTARTING);
    if interrupt {
        window.pty.send(b"\x03");
    }
    let status = wait_for_exit(&mut window.pty.child);
    assert_eq!(status.success(), interrupt);
    let shown = window.pty.shown();
    let Some((closing, unsent)) = shown.split_once(copy::NOT_SENT) else {
        panic!("unsent heading missing: {shown}");
    };
    assert!(closing.contains(copy::RESUME_COMMAND), "{shown}");
    if interrupt {
        assert!(closing.contains(copy::CHAT_STAYS_OPEN), "{shown}");
    } else {
        assert!(closing.contains("placeholder restart failure"), "{shown}");
    }
    assert_eq!(
        unsent.trim(),
        "unconfirmed message\n\nunsent draft",
        "{shown}"
    );
    assert!(!shown.contains("waiting message"), "{shown}");
    assert!(requests(&server).is_empty());
}

#[test]
fn a_failed_restart_connection_hands_back_its_unsent_messages() {
    a_restart_reconnect_keeps_its_unsent_text(false);
}

#[test]
fn interrupting_a_restart_connection_hands_back_its_unsent_messages() {
    a_restart_reconnect_keeps_its_unsent_text(true);
}

#[test]
fn another_build_is_named_without_restarting_the_chat() {
    if !usable() {
        return;
    }
    for build in [0, u64::MAX] {
        let home = Home::new("host-old");
        let server = serve();
        let host = fake_chat(&home, build, Some(true), false, None, false);
        let listed = command(&home, &server, WISP)
            .arg("daemon")
            .output()
            .unwrap();
        let listed = support::home::out(&listed);
        assert_eq!(listed.contains("older version"), build == 0, "{listed}");
        assert_eq!(
            listed.contains("Restart the chats kept open on the newest version: wisp restart"),
            build == 0,
            "{listed}"
        );
        let pty = launch(&home, &server, &["--resume", ID]);
        pty.wait_for(READY);
        if build == 0 {
            pty.wait_for("This chat runs an older version.");
            pty.wait_for("wisp\u{a0}restart");
        } else {
            assert!(!pty.saw("This chat runs an older version."));
        }
        quit(pty);
        host.join().unwrap();
    }
}

#[test]
fn a_window_that_loses_its_chat_says_what_became_of_it() {
    if !usable() {
        return;
    }
    use wisp_render::transcript::copy;
    for (window, first_run, missing_log, close) in [
        (false, false, false, None),
        (true, false, false, None),
        (false, true, false, None),
        (false, false, true, None),
        (false, false, false, Some(true)),
        (false, false, false, Some(false)),
    ] {
        let home = Home::new("host-cut");
        let server = serve();
        let (windows, opened) = mpsc::channel();
        let (ending, ended) = mpsc::channel();
        let host = fake_chat(&home, 0, Some(true), window, Some((windows, ended)), false);
        let mut pty = if first_run {
            let mut command = Pty::command(&home.0);
            command.env("PATH", wsl(&home));
            command.args(["--model", MODEL, "--time", "off", "--resume", ID]);
            let mut pty = Pty::start(&mut command, 30, 100, Stdout::Tty);
            super::first_run::key_card(&mut pty);
            super::first_run::fill_gateway(&mut pty, server.port, "gw");
            pty
        } else {
            launch(&home, &server, &["--resume", ID])
        };
        pty.wait_for(READY);
        let first = opened.recv_timeout(PATIENCE).unwrap();
        if close.is_none() {
            ending.send(false).unwrap();
        }
        if !window && close.is_none() {
            pty.send(b"sent before the cut\r");
            assert!(wait_until(PATIENCE, || home
                .read("fake-host-sent")
                .contains("tui-1")));
            home.write("fake-host-knows", "");
            pty.send(b"an unsent draft\x1b[D\x1b[D\x1b[D\x1b[D\x1b[D");
            pty.wait_shown("an unsent draft");
        }
        if missing_log {
            fs::remove_file(home.file(&format!("{SESSIONS}/{ID}.jsonl"))).unwrap();
        }
        if close.is_some() {
            fs::write(
                link::error_file(&socket(&home).unwrap()),
                "placeholder error line one\nplaceholder error line two\n",
            )
            .unwrap();
        }
        first.shutdown(Shutdown::Both).unwrap();
        if let Some(cancel) = close {
            pty.wait_for(copy::WINDOW_RECONNECTING);
            if cancel {
                pty.send(b"\x03");
                pty.wait_for(copy::CHAT_STAYS_OPEN);
                pty.wait_for("wisp --resume");
                assert!(!pty.saw(copy::CHAT_LOST));
                assert!(!pty.saw("placeholder error line two"));
                assert!(wait_for_exit(&mut pty.child).success());
            }
            ending.send(true).unwrap();
            host.join().unwrap();
            if !cancel {
                pty.wait_for(copy::CHAT_LOST);
                pty.wait_for("wisp --resume");
                pty.wait_for("placeholder error line two");
                let output = pty.screen();
                assert!(
                    output.find("placeholder error line two").unwrap()
                        < output.find(copy::CHAT_LOST).unwrap()
                );
                assert!(!wait_for_exit(&mut pty.child).success());
            }
            continue;
        }
        if window {
            pty.wait_for(copy::CHAT_TAKEN);
        } else {
            pty.wait_for(copy::WINDOW_RECONNECTING);
            pty.wait_for(copy::WINDOW_REOPENED);
            assert!(!pty.saw("No, don't ask again"));
            assert!(!pty.saw(copy::CHAT_LOST));
            assert!(pty.child.try_wait().unwrap().is_none());
            pty.wait_screen("unsent text after reconnect", |shown| {
                shown.contains(copy::WINDOW_REOPENED) && shown.contains("an unsent draft")
            });
            assert!(!pty.shown().contains("sent before the cut"));
            pty.send(b"!");
            pty.wait_shown("an unsent !draft");
            let second = opened.recv_timeout(PATIENCE).unwrap();
            ending.send(false).unwrap();
            second.shutdown(Shutdown::Both).unwrap();
            let _third = opened.recv_timeout(PATIENCE).unwrap();
            ending.send(false).unwrap();
            assert!(pty.child.try_wait().unwrap().is_none());
            pty.wait_screen("the reconnected composer", |shown| {
                shown.contains(copy::WINDOW_REOPENED) && shown.contains("an unsent !draft")
            });
            pty.send(b"\x01\x0b/leave\r");
            pty.wait_for(copy::CHAT_STAYS_OPEN);
            pty.wait_for("wisp --resume");
            assert!(!pty.saw(copy::CHAT_LOST));
        }
        assert!(wait_for_exit(&mut pty.child).success());
        runtime().block_on(async {
            let path = link::socket(&home.file(".local/share/wisp"), ID);
            let mut connection = wisp_client::link::connect(&path, 0).await.unwrap();
            assert!(connection.end(false).await.unwrap());
        });
        host.join().unwrap();
    }
}

#[test]
fn already_sent_rewound_text_stays_out_of_a_reconnected_window() {
    if !usable() {
        return;
    }
    use std::sync::atomic::{AtomicUsize, Ordering};
    let home = Home::new("host-rewind-cut");
    let (started, waiting) = mpsc::channel();
    let (release, released) = mpsc::channel();
    let released = Mutex::new(released);
    let calls = AtomicUsize::new(0);
    let server = Server::start(move |request| {
        if request.head.contains("count_tokens") {
            return counted();
        }
        if calls.fetch_add(1, Ordering::SeqCst) == 1 {
            started.send(()).unwrap();
            released.lock().unwrap().recv_timeout(PATIENCE).unwrap();
        }
        text_reply("message", MODEL, DONE, (1, 5))
    });
    let doctor = command(&home, &server, WISP)
        .env(wisp_protocol::WISP_JOB, "1")
        .arg("doctor")
        .output()
        .unwrap();
    assert!(doctor.status.success(), "{}", support::home::err(&doctor));
    let mut first = Pty::start(
        window_command(&home, &server, &[]).env(wisp_protocol::WISP_JOB, "1"),
        30,
        100,
        Stdout::Tty,
    );
    first.wait_for(READY);
    turn(&mut first, &home);
    let path = socket(&home).unwrap();
    let id = link::session_of(&path).unwrap();
    first.send(b"/rewind\r");
    first.wait_shown("enter choose");
    first.send(b"\r");
    first.wait_shown("Rewind to before turn 1");
    first.send(b"\r");
    first.wait_shown("rewound to before turn 1");
    first.send(b" resent\r");
    waiting.recv_timeout(PATIENCE).unwrap();
    first.child.kill().unwrap();
    first.child.wait().unwrap();
    drop(first);
    let mut second = launch(&home, &server, &["--resume", id]);
    second.wait_shown("rewound to before turn 1");
    second.wait_shown("a short local test resent");
    let shown = second.shown();
    assert!(!shown.contains(DONE), "the old answer must be cut: {shown}");
    assert_eq!(
        shown.matches("rewound to before turn 1").count(),
        1,
        "{shown}"
    );
    second.send(b"only the new draft\r");
    let mut actual = String::new();
    assert!(wait_until(PATIENCE, || {
        actual = home
            .records()
            .iter()
            .rev()
            .find_map(|record| {
                (record["type"] == "input_accepted")
                    .then(|| record["text"].as_str())
                    .flatten()
                    .filter(|text| text.contains("only the new draft"))
                    .map(str::to_owned)
            })
            .unwrap_or_default();
        !actual.is_empty()
    }));
    release.send(()).unwrap();
    quit(second);
    assert_eq!(actual, "only the new draft");
}

fn refuse_once(home: &Home) -> std::thread::JoinHandle<()> {
    let path = socket(home).unwrap();
    let hidden = link::socket(
        &home.file(".local/share/wisp"),
        &wisp_core::session::new_session_id(),
    );
    fs::rename(&path, &hidden).unwrap();
    let listener = UnixListener::bind(&path).unwrap();
    std::thread::spawn(move || {
        let (mut window, _) = listener.accept().unwrap();
        let mut host = UnixStream::connect(&hidden).unwrap();
        host.set_read_timeout(Some(PATIENCE)).unwrap();
        let mut client_reader = BufReader::new(window.try_clone().unwrap());
        let mut line = String::new();
        client_reader.read_line(&mut line).unwrap();
        let Some(ToHost::Hello { build, .. }) = link::read(&line) else {
            panic!("expected Hello");
        };
        host.write_all(
            jsonl::line(&ToHost::Hello {
                contract: link::CONTRACT + 1,
                build,
            })
            .unwrap()
            .as_bytes(),
        )
        .unwrap();
        let mut reader = BufReader::new(host);
        line.clear();
        reader.read_line(&mut line).unwrap();
        assert!(matches!(link::read(&line), Some(ToClient::Refused { .. })));
        window.write_all(line.as_bytes()).unwrap();
        line.clear();
        assert_eq!(client_reader.read_line(&mut line).unwrap(), 0);
        fs::remove_file(&path).unwrap();
        fs::rename(&hidden, &path).unwrap();
    })
}

#[test]
fn another_contract_offers_restart_with_no_default_and_cancellation_leaving_the_host_alone() {
    if !daemon::outside_job(
        "hosted::another_contract_offers_restart_with_no_default_and_cancellation_leaving_the_host_alone",
    ) {
        return;
    }
    if !usable() {
        return;
    }
    let server = serve();
    for (answer, job) in [
        (b"\r".as_slice(), false),
        (b"\x1b", false),
        (b"\x03", false),
        (b"\x1b[A\r", false),
        (b"", true),
    ] {
        let home = Home::new("host-contract");
        let mut first = start(&home, &server, &[]);
        turn(&mut first, &home);
        let host = pid(&home);
        let path = socket(&home).unwrap();
        let id = link::session_of(&path).unwrap();
        first.send(b"/leave\r");
        assert!(first.finish().0.success());
        let before = home.logs();
        let proxy = refuse_once(&home);
        let mut command = command(&home, &server, "setsid");
        command
            .args([
                "--ctty", "--wait", WISP, "--model", MODEL, "--time", "off", "--resume", id,
            ])
            .env("TERM", "xterm-256color");
        if job {
            command.env(wisp_protocol::WISP_JOB, "1");
        }
        let mut pty = Pty::start(&mut command, 30, 100, Stdout::Tty);
        if job {
            pty.wait_for("this chat was started by a version of Wisp this one can't talk to");
            assert!(!wait_for_exit(&mut pty.child).success());
            assert!(!pty.saw("Restart it to open it here?"));
        } else {
            pty.wait_for("Restart it to open it here?");
            proxy.join().unwrap();
            pty.send(answer);
            if answer == b"\x1b[A\r" {
                pty.wait_for("Restarting the chat…");
                pty.wait_for(READY);
                pty.wait_for(DONE);
                assert!(wait_until(PATIENCE, || !alive(host.as_raw_nonzero().get())));
                assert_ne!(pid(&home), host);
                quit(pty);
                continue;
            }
            pty.wait_for("wisp: not restarted; it stays open in the Wisp it started with");
            assert_eq!(
                wait_for_exit(&mut pty.child).code(),
                Some(if answer == b"\r" { 1 } else { 130 })
            );
            assert!(alive(host.as_raw_nonzero().get()));
            assert_eq!(home.logs(), before);
            quit(start(&home, &server, &["--resume", id]));
            continue;
        }
        proxy.join().unwrap();
        assert!(alive(host.as_raw_nonzero().get()));
        assert_eq!(home.logs(), before);
        quit(start(&home, &server, &["--resume", id]));
    }
}

fn wsl(home: &Home) -> String {
    home.write("proc/sys/kernel/osrelease", "6.6-microsoft-standard-WSL2");
    home.write("proc/stat", "cpu 0\nbtime 1\n");
    home.dir("mnt/c/Users/user");
    let mut entries: Vec<_> = std::env::split_paths(&std::env::var_os("PATH").unwrap_or_default())
        .filter(|entry| !entry.starts_with("/mnt/"))
        .collect();
    entries.push("/mnt/c/Users/user/AppData/Local/Microsoft/WindowsApps".into());
    std::env::join_paths(entries)
        .unwrap()
        .into_string()
        .unwrap()
}

fn wsl_launch(home: &Home, server: &Server, path: &str, job: bool) -> Pty {
    let mut command = command(home, server, "setsid");
    command
        .args(["--ctty", "--wait", WISP, "--model", MODEL, "--time", "off"])
        .env("PATH", path)
        .env("TERM", "xterm-256color");
    if job {
        command.env(wisp_protocol::WISP_JOB, "1");
    }
    Pty::start(&mut command, 30, 100, Stdout::Tty)
}

#[test]
fn yes_changes_the_scratch_windows_file_and_shows_the_restart_note_only() {
    if !usable() {
        return;
    }
    let home = Home::new("wsl-yes");
    let path = wsl(&home);
    let file = home.write("mnt/c/Users/user/.wslconfig", "[wsl2]\nmemory=8GB\n");
    fs::set_permissions(&file, fs::Permissions::from_mode(0o640)).unwrap();
    let server = serve();
    let mut pty = wsl_launch(&home, &server, &path, false);
    pty.wait_for("No, don't ask again");
    pty.send(b"\x1b[A\r");
    pty.wait_for(".wslconfig changed since Linux");
    pty.wait_for(READY);
    assert_eq!(
        fs::read_to_string(&file).unwrap(),
        "[wsl2]\nmemory=8GB\n[general]\ninstanceIdleTimeout=-1\n"
    );
    assert_eq!(
        fs::metadata(&file).unwrap().permissions().mode() & 0o777,
        0o640
    );
    quit(pty);
    let pty = wsl_launch(&home, &server, &path, false);
    pty.wait_for(READY);
    assert!(!pty.saw("No, don't ask again"));
    pty.wait_for(".wslconfig changed since Linux");
    quit(pty);
}

#[test]
fn no_is_the_default_and_is_remembered_but_escape_is_not() {
    if !usable() {
        return;
    }
    let server = serve();
    for cancel in [true, false] {
        let home = Home::new(if cancel { "wsl-escape" } else { "wsl-no" });
        let path = wsl(&home);
        let mut pty = wsl_launch(&home, &server, &path, false);
        pty.wait_for("No, don't ask again");
        pty.send(if cancel { b"\x1b" } else { b"\r" });
        if cancel {
            pty.wait_for("Cancelled; nothing was changed.");
            assert_eq!(wait_for_exit(&mut pty.child).code(), Some(130));
        } else {
            pty.wait_for(READY);
            quit(pty);
        }
        assert!(!home.exists("mnt/c/Users/user/.wslconfig"));
        assert_eq!(home.exists(".local/share/wisp/wsl-declined"), !cancel);
        let mut again = wsl_launch(&home, &server, &path, false);
        if cancel {
            again.wait_for("No, don't ask again");
            again.send(b"\r");
        }
        again.wait_for(READY);
        if !cancel {
            assert!(!again.saw("No, don't ask again"));
        }
        quit(again);
    }
}

#[test]
fn the_windows_question_is_never_asked_in_a_job_or_off_a_terminal() {
    if !usable() {
        return;
    }
    let home = Home::new("wsl-gates");
    let path = wsl(&home);
    let server = serve();
    let pty = wsl_launch(&home, &server, &path, true);
    pty.wait_for(READY);
    assert!(!pty.saw("No, don't ask again"));
    quit(pty);
    let output = command(&home, &server, WISP)
        .env("PATH", path)
        .args(["--output", "jsonl", "--model", MODEL])
        .stdin(Stdio::null())
        .output()
        .unwrap();
    assert!(output.status.success());
    assert!(!String::from_utf8_lossy(&output.stderr).contains("No, don't ask again"));
    assert!(!home.exists("mnt/c/Users/user/.wslconfig"));
}

#[test]
fn a_wisp_run_as_a_job_is_marked_and_ends_with_the_job() {
    if !usable() {
        return;
    }
    let home = Home::new("host-job");
    let path = wsl(&home);
    fs::remove_file(home.file("proc/sys/kernel/osrelease")).unwrap();
    let runs = Arc::new(Mutex::new(0));
    let count = Arc::clone(&runs);
    let nested = format!(
        "printf '%s' \"$WISP_JOB\" > job-mark; {WISP:?} quit 5e6f 2> job-quit; {WISP:?} restart 2> job-restart; exec {WISP:?} --model {MODEL} --time off"
    );
    let server = Server::start(move |request| {
        if request.head.contains("count_tokens") {
            return counted();
        }
        let mut count = count.lock().unwrap();
        *count += 1;
        if *count == 1 {
            support::server::tool_reply(
                "nested-wisp",
                "bash",
                &json!({
                    "command": nested, "tty": true, "run_in_background": true,
                    "description": "Starts a local test window",
                }),
            )
        } else {
            text_reply("done", MODEL, DONE, (1, 5))
        }
    });
    let mut pty = wsl_launch(&home, &server, &path, false);
    pty.wait_for(READY);
    home.write("proc/sys/kernel/osrelease", "6.6-microsoft-standard-WSL2");
    let outer = pid(&home);
    fs::remove_file(home.file(support::home::DAEMON_STOPPED)).unwrap();
    pty.send(b"start the local test window\r");
    pty.wait_for(DONE);
    assert_eq!(home.read("job-mark"), "1");
    assert_eq!(
        home.read("job-quit"),
        "wisp: run by an agent, wisp quit quits nothing\n"
    );
    assert_eq!(
        home.read("job-restart"),
        "wisp: run by an agent, wisp restart restarts nothing\n"
    );
    let mut hosts = Vec::new();
    assert!(wait_until(PATIENCE, || {
        hosts = fs::read_dir(home.file(".local/share/wisp/run"))
            .unwrap()
            .flatten()
            .filter_map(|entry| std::os::unix::net::UnixStream::connect(entry.path()).ok())
            .filter_map(crate::scratch::peer_pid)
            .collect();
        hosts.len() == 2
    }));
    assert!(!home.exists(".local/share/wisp/daemon/daemon.sock"));
    assert!(!home.exists(".local/share/wisp/wsl-declined"));
    quit(pty);
    assert!(wait_until(PATIENCE, || hosts
        .iter()
        .all(|pid| !alive(pid.as_raw_nonzero().get()))));
    assert!(!alive(outer.as_raw_nonzero().get()));
}

#[test]
fn secret_commands_and_an_undecodable_line_never_reach_host_files_or_window_output() {
    if !daemon::outside_job(
        "hosted::secret_commands_and_an_undecodable_line_never_reach_host_files_or_window_output",
    ) {
        return;
    }
    for through_daemon in [false, true] {
        if !usable() {
            return;
        }
        let home = Home::new("host-canary");
        let server = serve();
        let mut pty = start(&home, &server, &[]);
        let path = socket(&home).unwrap();
        if through_daemon {
            daemon::start_daemon(&home, &server);
        }
        let canary = "sk-ant-CANARY-HOSTED-WIRE-SECRET";
        runtime().block_on(async {
            use tokio::io::{AsyncBufReadExt, AsyncWriteExt};
            use wisp_protocol::endpoint::{EndpointEdit, EndpointSpec, StoreChoice};
            let mut stream = if through_daemon {
                daemon::stream(&home, link::session_of(&path).unwrap()).await
            } else {
                tokio::io::BufReader::new(wisp_client::link::dial(&path).await.unwrap())
            };
            stream
                .write_all(
                    jsonl::line(&ToHost::Hello {
                        contract: link::CONTRACT,
                        build: 0,
                    })
                    .unwrap()
                    .as_bytes(),
                )
                .await
                .unwrap();
            let mut stream = tokio::io::BufReader::new(stream);
            let mut line = String::new();
            stream.read_line(&mut line).await.unwrap();
            let ToClient::Welcome(welcome) = link::read(&line).unwrap() else {
                panic!("missing welcome");
            };
            stream
                .write_all(jsonl::line(&ToHost::Open).unwrap().as_bytes())
                .await
                .unwrap();
            line.clear();
            stream.read_line(&mut line).await.unwrap();
            assert!(matches!(
                link::read::<ToClient>(&line),
                Some(ToClient::Opened { .. })
            ));
            let key = || wisp_protocol::Secret::new(canary);
            let request = welcome.first_request;
            for command in [
                ClientCommand::AnswerSecret {
                    call_id: "missing-call".into(),
                    value: key(),
                },
                ClientCommand::SaveEndpoint {
                    request,
                    endpoint: EndpointSpec {
                        name: "/invalid".into(),
                        provider: "anthropic".into(),
                        auth: "api_key".into(),
                        url: None,
                        store: StoreChoice::File,
                        default: false,
                    },
                    key: key(),
                    check: false,
                },
                ClientCommand::EditEndpoint {
                    request,
                    name: "/invalid".into(),
                    edit: EndpointEdit {
                        key: Some(key()),
                        ..Default::default()
                    },
                    store: None,
                    check: false,
                },
                ClientCommand::SubscriptionAuth {
                    command: wisp_protocol::subscription_auth::AuthCommand::Finish {
                        request,
                        login: request,
                        response: Some(key()),
                        alias: None,
                    },
                },
            ] {
                stream
                    .write_all(jsonl::line(&ToHost::Command(command)).unwrap().as_bytes())
                    .await
                    .unwrap();
            }
            stream
                .write_all(format!("{{\"type\":\"future\",\"secret\":\"{canary}\"}}\n").as_bytes())
                .await
                .unwrap();
            stream
                .write_all(
                    jsonl::line(&ToHost::End { forced: false })
                        .unwrap()
                        .as_bytes(),
                )
                .await
                .unwrap();
            loop {
                line.clear();
                assert!(stream.read_line(&mut line).await.unwrap() > 0);
                assert!(!line.contains(canary));
                if matches!(link::read::<ToClient>(&line), Some(ToClient::Ended { .. })) {
                    break;
                }
            }
        });
        pty.wait_for("Opened in another window.");
        wait_for_exit(&mut pty.child);
        assert!(!pty.screen().contains(canary));
        assert!(!pty.screen().contains(daemon::CLIENT_KEY));
        let mut folders = vec![home.file(".local/share/wisp")];
        while let Some(folder) = folders.pop() {
            for entry in fs::read_dir(folder).unwrap().flatten() {
                if entry.file_type().unwrap().is_dir() {
                    folders.push(entry.path());
                } else if entry.file_type().unwrap().is_file() {
                    for key in [canary, daemon::CLIENT_KEY] {
                        assert!(
                            !fs::read(entry.path())
                                .unwrap()
                                .windows(key.len())
                                .any(|word| word == key.as_bytes())
                        );
                    }
                }
            }
        }
    }
}

#[test]
fn reopening_mid_turn_describing_and_taking_over_preserve_every_request_prefix() {
    if !daemon::outside_job(
        "hosted::reopening_mid_turn_describing_and_taking_over_preserve_every_request_prefix",
    ) {
        return;
    }
    if !usable() {
        return;
    }
    use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
    let begun = Arc::new(AtomicBool::new(false));
    let release = Arc::new(AtomicBool::new(false));
    let (started, ready) = (Arc::clone(&begun), Arc::clone(&release));
    let calls = AtomicUsize::new(0);
    let server = Server::start(move |request| {
        if request.head.contains("count_tokens") {
            return counted();
        }
        if !started.swap(true, Ordering::SeqCst) {
            assert!(wait_until(PATIENCE, || ready.load(Ordering::SeqCst)));
        }
        if calls.fetch_add(1, Ordering::SeqCst) == 4 {
            return support::server::tool_reply(
                "restart-prefix",
                "bash",
                &json!({"command": "sleep 60", "description": "Run the workspace tests"}),
            );
        }
        text_reply("prefix", MODEL, DONE, (1, 5))
    });
    let home = Home::new("host-prefix");
    let mut first = start(&home, &server, &[]);
    first.send(b"first\r");
    assert!(wait_until(PATIENCE, || begun.load(Ordering::SeqCst)));
    let path = socket(&home).unwrap();
    let id = link::session_of(&path).unwrap().to_owned();
    first.child.kill().unwrap();
    first.child.wait().unwrap();
    drop(first);
    let mut second = start(&home, &server, &["--resume", &id]);
    second.send(b"queued second");
    let reopened = second.wait_shown("queue for the next turn");
    assert!(
        reopened.contains("first"),
        "opening input missing after reconnect:
{reopened}"
    );
    second.send(b"\t");
    second.wait_screen("the queued input", |shown| {
        shown.contains("next turn")
            && shown.contains("queued second")
            && !shown.contains("queue for the next turn")
    });
    release.store(true, Ordering::SeqCst);
    let turns = || requests(&server);
    assert!(wait_until(PATIENCE, || turns().len() >= 2));
    assert!(wait_until(PATIENCE, || completed(&home) >= 2));
    second.wait_idle();
    runtime().block_on(async {
        let link = wisp_client::link::open(wisp_client::link::connect(&path, 0).await.unwrap())
            .await
            .unwrap();
        assert!(
            link.view
                .events
                .iter()
                .any(|event| matches!(event, wisp_protocol::AgentEvent::TurnEnded { .. }))
        );
        drop(link);
    });
    second.wait_for("Opened in another window.");
    wait_for_exit(&mut second.child);
    let mut third = start(&home, &server, &["--resume", &id]);
    third.send(b"third\r");
    assert!(wait_until(PATIENCE, || completed(&home) >= 3));
    third.wait_idle();
    assert!(wait_until(PATIENCE, || turns().len() == 3));
    let mut wider = launch_sized(&home, &server, &["--resume", &id], 120);
    wider.wait_for(READY);
    third.wait_for("Opened in another window.");
    wider.send(b"fourth\r");
    assert!(wait_until(PATIENCE, || completed(&home) >= 4));
    wider.wait_idle();
    assert!(wait_until(PATIENCE, || turns().len() == 4));
    wider.send(b"fifth before restart\r");
    wider.wait_for("Run the workspace tests");
    let mut restart = Pty::start(Pty::command(&home.0).arg("restart"), 24, 100, Stdout::Tty);
    restart.wait_for("finishing: Run the workspace tests");
    restart.send(b"\r");
    restart.wait_for("Stopped what 1 chat was finishing; its agents carry on.");
    restart.wait_for("stopped: Run the workspace tests");
    assert!(restart.finish().0.success());
    wider.wait_for(wisp_render::transcript::copy::WINDOW_RESTARTED);
    assert!(wider.child.try_wait().unwrap().is_none());
    assert!(!wider.saw(wisp_render::transcript::copy::CHAT_LOST));
    assert!(wait_until(PATIENCE, || turns().len() >= 6));
    let turns = turns();
    assert_eq!(turns.len(), 6);
    support::prefix::assert_prefix_stable(&turns, None);
    let terminal_count = |value: &Value| {
        value["messages"]
            .to_string()
            .matches("Terminal: replies are")
            .count()
    };
    assert_eq!(terminal_count(&turns[0]), terminal_count(&turns[2]));
    assert_eq!(terminal_count(&turns[3]), terminal_count(&turns[2]) + 1);
    assert!(
        turns[3]["messages"]
            .to_string()
            .contains("116 columns wide")
    );
    wider.send(b"/leave\r");
    wider.wait_for(wisp_render::transcript::copy::CHAT_STAYS_OPEN);
    assert!(wait_for_exit(&mut wider.child).success());
    runtime().block_on(async {
        assert!(
            wisp_client::link::connect(&path, 0)
                .await
                .unwrap()
                .end(false)
                .await
                .unwrap()
        );
    });
}

#[test]
fn an_account_question_starts_its_chosen_chat_in_a_host() {
    if !usable() {
        return;
    }
    let home = Home::new("host-account");
    let server = serve();
    home.write(CONFIG, &format!("[endpoints.team]\nprovider = \"anthropic\"\nauth = \"api_key\"\nkey_env = \"ANTHROPIC_API_KEY\"\nurl = \"{}\"\npricing = \"none\"\n", server.url()));
    let mut first = start(&home, &server, &["--account", "team"]);
    turn(&mut first, &home);
    let path = socket(&home).unwrap();
    let id = link::session_of(&path).unwrap().to_owned();
    quit(first);
    let log = home.file(&format!("{SESSIONS}/{id}.jsonl"));
    let records = fs::read_to_string(&log)
        .unwrap()
        .lines()
        .map(|line| {
            let mut record: Value = serde_json::from_str(line).unwrap();
            if record
                .get("credential_source")
                .is_some_and(|value| !value.is_null())
            {
                record["credential_source"] =
                    serde_json::to_value(wisp_protocol::log::CredentialSource::Mode {
                        billing: wisp_protocol::Billing::AnthropicPlan,
                        source: Box::new(wisp_protocol::log::CredentialSource::Environment),
                    })
                    .unwrap();
            }
            jsonl::line(&record).unwrap()
        })
        .collect::<String>();
    fs::write(log, records).unwrap();
    home.write(CONFIG, "");
    let mut question = launch(&home, &server, &["--resume", &id]);
    question.wait_for("Where should this chat continue?");
    question.send(b"\x1b[B\r");
    question.wait_for(READY);
    assert!(socket(&home).is_some());
    turn(&mut question, &home);
    quit(question);
}

#[test]
fn an_unsaved_host_or_a_lost_quit_is_not_reported_as_success() {
    if !usable() {
        return;
    }
    for saved in [Some(false), None] {
        let home = Home::new("host-end-fail");
        let server = serve();
        let host = fake_chat(&home, 0, saved, false, None, false);
        let mut pty = launch(&home, &server, &["--resume", ID]);
        pty.wait_for(READY);
        pty.send(b"/quit\r");
        assert_eq!(wait_for_exit(&mut pty.child).code(), Some(130));
        assert!(!pty.screen().contains("a settings save was still waiting"));
        host.join().unwrap();
    }
}

#[test]
fn chats_restart_one_at_a_time_and_ctrl_c_leaves_only_the_busy_chat_untouched() {
    if !daemon::outside_job(
        "hosted::chats_restart_one_at_a_time_and_ctrl_c_leaves_only_the_busy_chat_untouched",
    ) {
        return;
    }
    if !usable() {
        return;
    }
    use std::sync::atomic::{AtomicUsize, Ordering};
    for cancel in [false, true] {
        let home = Home::new(if cancel {
            "restart-cancel"
        } else {
            "restart-each"
        });
        let release = home.file("release-work");
        let work = format!("while test ! -f {release:?}; do sleep 0.02; done");
        let calls = AtomicUsize::new(0);
        let server = Server::start(move |request| {
            if request.head.contains("count_tokens") {
                return counted();
            }
            if calls.fetch_add(1, Ordering::SeqCst) == 0 {
                support::server::tool_reply(
                    "restart-work",
                    "bash",
                    &json!({"command": work, "description": "Run the workspace tests"}),
                )
            } else {
                text_reply(
                    "after-restart",
                    MODEL,
                    if request.body.contains("answer-after-restart") {
                        "AFTER-RESTART-ANSWER"
                    } else {
                        DONE
                    },
                    (1, 5),
                )
            }
        });
        let (mut first, idle) = headless(&home, &server, WISP, &[], &Start::default());
        let (mut second, busy) = headless(&home, &server, WISP, &[], &Start::default());
        let idle_pid = crate::scratch::peer_pid(UnixStream::connect(&idle).unwrap()).unwrap();
        let busy_pid = crate::scratch::peer_pid(UnixStream::connect(&busy).unwrap()).unwrap();
        let rt = runtime();
        let mut client = rt.block_on(async {
            let mut client =
                wisp_client::link::open(wisp_client::link::connect(&busy, 0).await.unwrap())
                    .await
                    .unwrap();
            client
                .channels
                .commands
                .send(ClientCommand::send("restart-busy", "restart-busy"))
                .await
                .unwrap();
            tokio::time::timeout(PATIENCE, async {
                while let Some(event) = client.channels.events.recv().await {
                    if matches!(event, wisp_protocol::AgentEvent::ToolCallStarted { .. }) {
                        break;
                    }
                }
            })
            .await
            .unwrap();
            client
        });
        let mut restart = Pty::start(Pty::command(&home.0).arg("restart"), 24, 100, Stdout::Tty);
        restart.wait_for("Run the workspace tests");
        assert!(wait_until(PATIENCE, || UnixStream::connect(&idle)
            .ok()
            .and_then(crate::scratch::peer_pid)
            .is_some_and(|pid| pid != idle_pid)));
        assert_eq!(
            crate::scratch::peer_pid(UnixStream::connect(&busy).unwrap()),
            Some(busy_pid)
        );
        if cancel {
            let before = restart.wait_shown("ctrl-c cancel");
            let shown = restart.wait_screen("the next restart redraw", |shown| {
                shown != before && shown.contains("Restarting 2 chats")
            });
            assert!(
                shown
                    .lines()
                    .any(|line| line.starts_with("enter restart now")),
                "{shown}"
            );
            let until = Instant::now() + Duration::from_millis(200);
            while Instant::now() < until {
                assert!(!restart.cooked(), "the restart wait left raw mode");
            }
            restart.send(b"\x03");
            let shown = restart.wait_shown("Cancelled: the other chat carries on as it was.");
            assert!(!restart.saw("^C"), "{shown}");
            assert_eq!(restart.finish().0.code(), Some(130));
            assert_eq!(
                crate::scratch::peer_pid(UnixStream::connect(&busy).unwrap()),
                Some(busy_pid)
            );
            home.write("release-work", "");
        } else {
            home.write("release-work", "");
            restart.wait_for("Restarted 2 chats on the newest version.");
            assert!(restart.finish().0.success());
            assert_ne!(
                crate::scratch::peer_pid(UnixStream::connect(&busy).unwrap()),
                Some(busy_pid)
            );
            assert!(wait_for_exit(&mut second).success());
        }
        assert!(wait_for_exit(&mut first).success());
        rt.block_on(async {
            for path in [&idle, &busy] {
                let connection = wisp_client::link::connect(path, 0).await.unwrap();
                if cancel && path == &busy {
                    assert!(
                        tokio::time::timeout(PATIENCE, async {
                            loop {
                                if let Some(wisp_protocol::AgentEvent::TurnEnded { .. }) =
                                    client.channels.events.recv().await
                                {
                                    break;
                                }
                            }
                        })
                        .await
                        .is_ok()
                    );
                }
                let mut active = wisp_client::link::open(connection).await.unwrap();
                active
                    .channels
                    .commands
                    .send(ClientCommand::send("post-restart", "answer-after-restart"))
                    .await
                    .unwrap();
                let mut answered = false;
                tokio::time::timeout(PATIENCE, async {
                    while let Some(event) = active.channels.events.recv().await {
                        if let wisp_protocol::AgentEvent::TextDelta { text, .. } = &event {
                            answered |= text.contains("AFTER-RESTART-ANSWER");
                        }
                        if answered && matches!(event, wisp_protocol::AgentEvent::TurnEnded { .. })
                        {
                            break;
                        }
                    }
                })
                .await
                .unwrap();
                assert!(answered);
                assert!(active.end(false).await.unwrap());
            }
        });
        if cancel {
            assert!(wait_for_exit(&mut second).success());
        }
    }
}

#[test]
fn an_older_chat_is_untouched_before_yes_and_starts_even_after_the_terminal_closes() {
    if !daemon::outside_job(
        "hosted::an_older_chat_is_untouched_before_yes_and_starts_even_after_the_terminal_closes",
    ) {
        return;
    }
    if !usable() {
        return;
    }
    for answer in *b"\r\x1b\x03y\0" {
        let home = Home::new("restart-older");
        let server = serve();
        let (mut child, path) = headless(&home, &server, WISP, &[], &Start::default());
        let previous = link::session_of(&path).unwrap().to_owned();
        runtime().block_on(async {
            assert!(
                wisp_client::link::connect(&path, 0)
                    .await
                    .unwrap()
                    .end(false)
                    .await
                    .unwrap()
            );
        });
        assert!(wait_for_exit(&mut child).success());
        let records = home
            .read(&format!("{SESSIONS}/{previous}.jsonl"))
            .replace(&previous, ID);
        fs::remove_file(home.file(&format!("{SESSIONS}/{previous}.jsonl"))).unwrap();
        home.write(&format!("{SESSIONS}/{ID}.jsonl"), &records);
        home.write("fake-host-wait", "");
        let host = fake_chat(&home, 0, Some(true), true, None, false);
        let named: &[&str] = if answer == b'\r' { &[ID, "9999"] } else { &[] };
        let mut restart = Pty::start(
            command(&home, &server, "setsid")
                .args(["--ctty", "--wait", WISP, "restart"])
                .args(named)
                .env("TERM", "xterm-256color"),
            30,
            100,
            Stdout::Tty,
        );
        restart.wait_for("Restart it on the newest version?");
        restart.wait_for("Its window closes; it prints the command that opens it again.");
        restart.wait_for("enter choose");
        assert!(!home.exists("fake-host-commands"));
        if answer == b'y' {
            restart.send(b"\x1b[A\r");
            assert!(wait_until(PATIENCE, || home.exists("fake-host-commands")));
            let pid = rustix::process::Pid::from_child(&restart.child);
            restart.disconnect();
            kill_process(pid, Signal::INT).unwrap();
            kill_process(pid, Signal::HUP).unwrap();
            home.write("fake-host-release", "");
            assert!(wait_for_exit(&mut restart.child).success());
            host.join().unwrap();
            runtime().block_on(async {
                assert!(
                    wisp_client::link::connect(
                        &link::socket(&home.file(".local/share/wisp"), ID),
                        0
                    )
                    .await
                    .unwrap()
                    .end(false)
                    .await
                    .unwrap()
                );
            });
        } else {
            if answer == 0 {
                restart.disconnect();
                let _ = wait_for_exit(&mut restart.child);
            } else {
                restart.send(&[answer]);
                restart.wait_for("No chat was restarted.");
                assert_eq!(
                    restart.saw("wisp: no session's ID starts with 9999"),
                    !named.is_empty()
                );
                assert_eq!(
                    restart.finish().0.code(),
                    Some(i32::from(!named.is_empty()))
                );
            }
            assert!(!home.exists("fake-host-commands"));
            home.write("fake-host-release", "");
            runtime().block_on(async {
                assert!(
                    wisp_client::link::connect(
                        &link::socket(&home.file(".local/share/wisp"), ID),
                        0
                    )
                    .await
                    .unwrap()
                    .end(false)
                    .await
                    .unwrap()
                );
            });
            host.join().unwrap();
        }
    }
}

#[test]
fn a_lost_restart_peer_is_shown_running_only_while_its_log_is_held() {
    for (silent, held, other) in [
        (false, false, false),
        (true, false, false),
        (false, true, false),
        (false, true, true),
    ] {
        let home = Home::new("restart-closed");
        let server = serve();
        let mut records = String::new();
        for (seq, text) in [
            include_str!("../../../wisp-protocol/tests/fixtures/log_session_started.json"),
            include_str!("../../../wisp-protocol/tests/fixtures/log_session_named.json"),
        ]
        .into_iter()
        .enumerate()
        {
            let mut record: Value = serde_json::from_str(text).unwrap();
            record["schema_version"] = json!(wisp_protocol::log::LOG_SCHEMA_VERSION);
            record["seq"] = json!(seq);
            records.push_str(&jsonl::line(&record).unwrap());
        }
        home.write(&format!("{SESSIONS}/{ID}.jsonl"), &records);
        if silent {
            home.write("fake-host-silent", "");
        }
        if other {
            home.write("fake-host-other", "");
        }
        let _held = held.then(|| {
            let log = fs::File::open(home.file(&format!("{SESSIONS}/{ID}.jsonl"))).unwrap();
            log.try_lock().unwrap();
            log
        });
        let host = fake_chat(&home, 0, Some(true), false, None, true);
        let output = command(&home, &server, WISP)
            .arg("restart")
            .output()
            .unwrap();
        home.write("fake-host-release", "");
        host.join().unwrap();
        let text = support::home::out(&output);
        assert!(!output.status.success(), "{text}");
        assert!(text.contains("Cache fix"), "{text}");
        if silent {
            assert!(text.contains("no answer, not restarted"), "{text}");
        } else if held {
            assert!(text.contains("still running"), "{text}");
            assert!(
                text.contains(if other {
                    "which this client cannot open"
                } else {
                    "the chat connection closed"
                }),
                "{text}"
            );
            assert!(!text.contains("didn't start again"), "{text}");
            assert!(!text.contains("wisp --resume"), "{text}");
        } else {
            assert!(text.contains("didn't start again"), "{text}");
            assert!(text.contains("the chat connection closed"), "{text}");
            assert!(
                text.contains(&format!("wisp --resume {}", wisp_protocol::short_id(ID))),
                "{text}"
            );
            assert!(!text.contains("It keeps running as it was"), "{text}");
        }
    }
}

#[test]
fn a_chat_never_restarts_itself_and_says_why_at_once() {
    use std::sync::atomic::{AtomicUsize, Ordering};
    for named in [true, false] {
        let home = Home::new("restart-self");
        let id = home.file("self-id");
        let out = home.file("self-out");
        let target = if named {
            format!("\"$(cat {id:?})\"")
        } else {
            String::new()
        };
        let work = format!(
            "env -u WISP_JOB {WISP} restart {target} > {out:?} 2>&1; echo code=$? >> {out:?}"
        );
        let calls = AtomicUsize::new(0);
        let server = Server::start(move |request| {
            if request.head.contains("count_tokens") {
                counted()
            } else if calls.fetch_add(1, Ordering::SeqCst) == 0 {
                support::server::tool_reply(
                    "self-restart",
                    "bash",
                    &json!({"command": work, "description": "Restart this chat"}),
                )
            } else {
                text_reply("after-self-restart", MODEL, DONE, (1, 5))
            }
        });
        let (mut child, path) = headless(&home, &server, WISP, &[], &Start::default());
        let (mut other, beside) = headless(&home, &server, WISP, &[], &Start::default());
        let session = link::session_of(&path).unwrap().to_owned();
        fs::write(&id, &session).unwrap();
        let pids = || {
            [&path, &beside]
                .map(|path| crate::scratch::peer_pid(UnixStream::connect(path).unwrap()).unwrap())
        };
        let hosts = pids();
        runtime().block_on(async {
            let mut client =
                wisp_client::link::open(wisp_client::link::connect(&path, 0).await.unwrap())
                    .await
                    .unwrap();
            client
                .channels
                .commands
                .send(ClientCommand::send("restart-self", "restart yourself"))
                .await
                .unwrap();
            tokio::time::timeout(PATIENCE, async {
                while let Some(event) = client.channels.events.recv().await {
                    if matches!(event, wisp_protocol::AgentEvent::TurnEnded { .. }) {
                        break;
                    }
                }
            })
            .await
            .unwrap();
            assert_eq!(pids(), hosts);
            assert!(client.end(false).await.unwrap());
            assert!(
                wisp_client::link::connect(&beside, 0)
                    .await
                    .unwrap()
                    .end(false)
                    .await
                    .unwrap()
            );
        });
        assert_eq!(
            fs::read_to_string(&out).unwrap(),
            format!(
                "wisp: this chat ({}) can't restart itself; restart it from outside\ncode=1\n",
                wisp_protocol::short_id(&session)
            )
        );
        assert!(wait_for_exit(&mut child).success());
        assert!(wait_for_exit(&mut other).success());
    }
}
