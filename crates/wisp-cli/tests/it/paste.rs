#![cfg(target_os = "linux")]
#![allow(
    clippy::unwrap_used,
    clippy::panic,
    reason = "test helpers fail the test on any socket, pty or process error"
)]

use crate::support;

use std::io::Write;
use std::path::Path;
use std::time::Duration;

use support::home::Home;
use support::pty::{Pty, READY, Stdout, usable, wait_for_exit};
use support::server::{Server, text_reply};

const DONE: &str = "PASTE-CHECKS-DONE";

fn serve() -> Server {
    Server::start(|_| text_reply("msg_paste", "claude-test", DONE, (0, 3)))
}

fn spawn_tui(home: &Home, port: u16) -> Pty {
    let mut command = Pty::command(&home.0);
    command
        .args(["--time", "off", "--env-file"])
        .arg(home.env_file())
        .env("TMPDIR", home.dir("tmp"))
        .env("ANTHROPIC_API_KEY", "test-key-not-used")
        .env("ANTHROPIC_BASE_URL", format!("http://127.0.0.1:{port}"));
    Pty::start(&mut command, 30, 100, Stdout::Tty)
}

fn last_user_text(body: &str) -> String {
    let request: serde_json::Value = serde_json::from_str(body).unwrap();
    let messages = request["messages"].as_array().unwrap();
    let user = messages.iter().rev().find(|m| m["role"] == "user").unwrap();
    let blocks = user["content"].as_array().unwrap();
    blocks.last().unwrap()["text"].as_str().unwrap().to_owned()
}

fn logged_texts(home: &Path) -> Vec<String> {
    let dir = home.join(".local/share/wisp/sessions");
    let mut out = vec![];
    for entry in std::fs::read_dir(dir).unwrap() {
        let text = std::fs::read_to_string(entry.unwrap().path()).unwrap();
        for line in text.lines() {
            let record: serde_json::Value = serde_json::from_str(line).unwrap();
            if record["type"] == "turn_started" {
                out.push(record["text"].as_str().unwrap().to_owned());
            }
        }
    }
    out
}

#[test]
fn a_pasted_chip_is_sent_and_logged_as_the_full_text() {
    if !usable() {
        return;
    }
    let home = Home::new("paste");
    let server = serve();
    let mut pty = spawn_tui(&home, server.port);
    pty.wait_for(READY);
    std::thread::sleep(Duration::from_millis(200));
    let pasted: String = (0..40)
        .map(|i| format!("row {i}:\tcaf\u{e9} \u{1F600} <tag> \"quoted\" \\ back"))
        .collect::<Vec<_>>()
        .join("\n");
    let evil = format!("{pasted}\n\u{1b}]0;PWNED\u{7}\u{1b}[2J\u{9b}31mred\u{202e}");
    pty.master.write_all(b"Look ").unwrap();
    pty.master.write_all(b"\x1b[200~").unwrap();
    pty.master.write_all(evil.as_bytes()).unwrap();
    pty.master.write_all(b"\x1b[201~").unwrap();
    pty.master.write_all(b" now").unwrap();
    pty.wait_for(b"[Pasted text #1 +41 lines]");
    pty.master.write_all(b"\r").unwrap();
    pty.wait_for(DONE.as_bytes());
    pty.wait_idle();
    pty.master.write_all(b"\x04").unwrap();
    wait_for_exit(&mut pty.child);
    let shown = pty.screen.lock().unwrap().clone();
    assert!(
        !shown.windows(8).any(|w| w == b"\x1b]0;PWNE"),
        "a pasted escape reached the terminal"
    );
    let expected = format!("Look {pasted}\n^[]0;PWNED^G^[[2JM-^[31mred<U+202E> now");
    let requests = server.bodies();
    assert_eq!(requests.len(), 1, "{requests:#?}");
    assert_eq!(last_user_text(&requests[0]), expected);
    assert_eq!(logged_texts(&home.0), [expected]);
}
