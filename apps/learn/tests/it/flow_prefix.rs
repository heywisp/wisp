#![cfg(target_os = "linux")]
#![allow(
    clippy::unwrap_used,
    clippy::panic,
    reason = "test helpers fail the test on any file, socket or process error"
)]

use std::path::Path;
use std::process::Stdio;
use std::time::Duration;

use futures::SinkExt;
use rustix::process::{Pid, Signal, kill_process};
use serde_json::{Value, json};
use tokio_tungstenite::tungstenite::Message;

use crate::LEARN;
use crate::client::open;
use crate::home::{CONFIG, Home, command_in};
use crate::server::{Server, text_reply};
use crate::web::bound;

#[tokio::test(flavor = "multi_thread")]
async fn the_flows_cached_prefix_is_pinned() {
    let server = Server::start(|_| text_reply("msg_flow", "claude-sonnet-5-5", "ok", (1, 1)));
    let home = Home::configured("flow-prefix");
    home.write(
        CONFIG,
        &format!(
            "default = \"gw\"\n[endpoints.gw]\nauth = \"bearer\"\nkey_env = \"GW_TOKEN\"\nurl = \"{}\"\n",
            server.url()
        ),
    );
    let mut child = command_in(&home.0, LEARN)
        .args(["--port", "0"])
        .env("GW_TOKEN", "gw-token")
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();
    let port = bound(&mut child);
    let host = format!("127.0.0.1:{port}");
    let mut socket = open(port, &host, Some(&format!("http://{host}")))
        .await
        .unwrap();
    let note = json!({"type": "voice", "command": {"type": "note", "call": "", "text": "A"}});
    socket.send(Message::text(note.to_string())).await.unwrap();
    let body = tokio::time::timeout(Duration::from_secs(30), async {
        loop {
            if let Some(body) = server
                .bodies()
                .into_iter()
                .find(|b| b.contains("Tapped: A"))
            {
                return body;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .unwrap();
    let body: Value = serde_json::from_str(&body).unwrap();
    let prefix =
        serde_json::to_string_pretty(&json!({"system": body["system"], "tools": body["tools"]}))
            .unwrap()
            + "\n";
    let golden = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/golden/flow_prefix.json");
    if std::env::var_os("WISP_BLESS").is_some() {
        std::fs::write(&golden, &prefix).unwrap();
    }
    assert_eq!(
        std::fs::read_to_string(&golden).unwrap(),
        prefix,
        "run with WISP_BLESS=1 if that is intended"
    );
    crate::web::until(&mut socket, |v| v["type"] == "turn_ended").await;
    kill_process(
        Pid::from_raw(i32::try_from(child.id()).unwrap()).unwrap(),
        Signal::INT,
    )
    .unwrap();
    assert!(child.wait().unwrap().success());
}
