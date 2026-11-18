#![cfg(target_os = "linux")]
#![allow(
    clippy::unwrap_used,
    clippy::panic,
    reason = "test helpers fail the test on any socket or process error"
)]

use crate::LEARN;
use crate::client::{Socket, get, next_text, open, page};
use crate::home::{CONFIG, Home, KEYS_FILE, command_in};
use crate::server::{Server, free_port, text_reply, tool_reply};

use std::io::{BufRead, BufReader};
use std::os::unix::fs::PermissionsExt;
use std::process::{Child, Command, Output, Stdio};
use std::sync::mpsc;
use std::time::Duration;

use futures::SinkExt;
use rustix::process::{Pid, Signal, kill_process};
use serde_json::{Value, json};
use tokio_tungstenite::tungstenite::{Error, Message};

const WAIT: Duration = Duration::from_secs(30);
const TOLD: &str = "hello from the page";
const SESSIONS: &str = ".local/share/learn/sessions";
const PICKS: &str = ".config/learn/settings.toml";

fn web(home: &Home, server: &Server) -> Command {
    configured(home, server, "");
    learn(home, server)
}

fn configured(home: &Home, server: &Server, extra: &str) {
    home.write(
        CONFIG,
        &format!(
            "default = \"gw\"\n{extra}[endpoints.gw]\nauth = \"bearer\"\nkey_env = \"GW_TOKEN\"\nurl = \"http://127.0.0.1:{}\"\n",
            server.port
        ),
    );
}

fn learn(home: &Home, server: &Server) -> Command {
    let mut command = command_in(&home.0, LEARN);
    command
        .args(["--port", "0"])
        .env("GW_TOKEN", "gw-token")
        .env(
            "ANTHROPIC_BASE_URL",
            format!("http://127.0.0.1:{}", server.port),
        )
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    command
}

fn first_line(child: &mut Child) -> String {
    let stdout = child.stdout.take().unwrap();
    let (tx, rx) = mpsc::channel();
    std::thread::spawn(move || {
        for line in BufReader::new(stdout).lines() {
            let _ = tx.send(line.unwrap_or_default());
        }
    });
    rx.recv_timeout(WAIT).unwrap_or_default()
}

pub(crate) fn bound(child: &mut Child) -> u16 {
    let line = first_line(child);
    let port = line.strip_prefix("Open http://127.0.0.1:");
    let port = port.and_then(|rest| rest.strip_suffix(" on this computer \u{b7} ctrl-c stops"));
    port.unwrap().parse().unwrap()
}

fn interrupt(child: &Child) {
    let pid = Pid::from_raw(i32::try_from(child.id()).unwrap()).unwrap();
    kill_process(pid, Signal::INT).unwrap();
}

async fn asked(socket: &mut Socket, request: u64, changes: &[(&str, &str)]) -> Value {
    let changes: Vec<Value> = changes
        .iter()
        .map(|(name, value)| json!({"target": {"name": name}, "value": {"kind": "text", "text": value}}))
        .collect();
    let command = json!({"type": "change_settings", "request": request, "save": true,
        "changes": changes});
    socket
        .send(Message::text(command.to_string()))
        .await
        .unwrap();
    let answers =
        |v: &Value| v.get("request").or_else(|| v.get("answers")) == Some(&json!(request));
    until(socket, answers).await
}

pub(crate) async fn until(socket: &mut Socket, wanted: impl Fn(&Value) -> bool) -> Value {
    loop {
        let next = next_text(socket).await.unwrap();
        if wanted(&next) {
            return next;
        }
    }
}

async fn screen(port: u16) -> (Socket, Value) {
    let host = format!("127.0.0.1:{port}");
    let mut socket = open(port, &host, Some(&format!("http://{host}")))
        .await
        .unwrap();
    let rows = next_text(&mut socket).await.unwrap();
    (socket, rows)
}

fn values(rows: &Value) -> Vec<Value> {
    let (brain, voice) = (&rows["rows"]["brain"], &rows["rows"]["voice"]);
    let shown = [&brain["account"]["name"], &brain["model"], &brain["effort"]];
    shown
        .into_iter()
        .chain([&voice["account"]["name"]])
        .cloned()
        .collect()
}

async fn described(socket: &mut Socket, request: u64) -> Value {
    let describe = json!({"type": "describe_settings", "request": request});
    socket
        .send(Message::text(describe.to_string()))
        .await
        .unwrap();
    until(socket, |v| v.get("rows").is_some()).await
}

async fn brain_rows(socket: &mut Socket, request: u64) -> Vec<Value> {
    values(&described(socket, request).await)[..2].to_vec()
}

fn stopped(child: Child) -> (Output, String) {
    interrupt(&child);
    let output = child.wait_with_output().unwrap();
    let stderr = String::from_utf8_lossy(&output.stderr).into_owned();
    assert!(output.status.success(), "{stderr}");
    (output, stderr)
}

async fn fresh(
    home: &Home,
    server: &Server,
    picks: &str,
    env: &[(&str, &str)],
) -> (Vec<Value>, String) {
    home.write(PICKS, picks);
    let mut child = learn(home, server)
        .arg("--new")
        .envs(env.iter().copied())
        .spawn()
        .unwrap();
    let port = bound(&mut child);
    let (_socket, rows) = screen(port).await;
    let (_, stderr) = stopped(child);
    (values(&rows), stderr)
}

fn lessons(home: &Home) -> Vec<std::path::PathBuf> {
    std::fs::read_dir(home.file(SESSIONS))
        .unwrap()
        .flatten()
        .map(|e| e.path())
        .filter(|p| p.extension().is_some_and(|e| e == "jsonl"))
        .collect()
}

async fn tapped(socket: &mut Socket, server: &Server, text: &str) -> String {
    let note = json!({"type": "voice", "command": {"type": "note", "call": "", "text": text}});
    socket.send(Message::text(note.to_string())).await.unwrap();
    let line = format!("Tapped: {text}");
    tokio::time::timeout(WAIT, async {
        loop {
            if let Some(body) = server.bodies().into_iter().find(|b| b.contains(&line)) {
                return body;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .unwrap()
}

#[tokio::test(flavor = "multi_thread")]
async fn learn_serves_the_page_and_drops_what_the_browser_may_not_send() {
    let server = Server::start(|_| text_reply("msg_web", "claude-sonnet-5-5", "no", (1, 1)));
    let home = Home::configured("web");
    let mut child = web(&home, &server)
        .args(["--account", "gw"])
        .env("ANTHROPIC_API_KEY", "test-key-not-used")
        .spawn()
        .unwrap();
    let port = bound(&mut child);
    let config = home.read(CONFIG);
    let (code, page) = get(port, &page(port, "/")).await;
    assert_eq!(code, 200, "{page}");
    assert!(
        page.contains("id=\"orb\"") && page.contains("Ready"),
        "{page}"
    );
    let host = format!("127.0.0.1:{port}");
    let refused = open(port, &host, Some("http://evil.example")).await;
    assert!(matches!(refused, Err(Error::Http(r)) if r.status() == 403));
    let own = format!("http://{host}");
    let mut socket = open(port, &host, Some(&own)).await.unwrap();
    let mut other = open(port, &host, Some(&own)).await.unwrap();
    let rows = next_text(&mut socket).await.unwrap();
    assert_eq!(next_text(&mut other).await.as_ref(), Some(&rows));
    let spent = json!({"type": "spend_changed", "total": "$0.00"});
    for screen in [&mut socket, &mut other] {
        assert_eq!(next_text(screen).await.as_ref(), Some(&spent));
    }
    let brain = &rows["rows"]["brain"];
    assert_eq!(brain["account"]["name"], "gw", "{rows}");
    assert_eq!(brain["account"]["kind"], "API", "{rows}");
    let groups = brain["groups"].as_array().unwrap();
    let headings: Vec<&Value> = groups.iter().map(|g| &g["heading"]["name"]).collect();
    assert_eq!(headings, ["Effort", "gateway", "gw"], "{rows}");
    for group in &groups[1..] {
        let items = group["items"].as_array().unwrap();
        assert!(!items.is_empty(), "{rows}");
        assert!(
            items
                .iter()
                .all(|m| m["shown"].as_str().unwrap().starts_with("claude-"))
        );
    }
    assert_eq!(rows["rows"]["voice"]["groups"], json!([]), "{rows}");
    let refused = |reason: &str| json!({"type": "rejected", "reason": reason, "request": 7});
    let answer = asked(&mut socket, 7, &[("display.theme", "light")]).await;
    assert_eq!(answer, refused(learn_web::REFUSED));
    let answer = asked(&mut socket, 7, &[("effort", "minimal")]).await;
    assert_eq!(
        answer,
        refused("effort must be one of low, medium, high, xhigh, max")
    );
    let send = json!({"type": "send", "id": "web-1", "text": TOLD, "mode": "queue"});
    socket.send(Message::text(send.to_string())).await.unwrap();
    for screen in [&mut socket, &mut other] {
        screen.send(Message::Ping(vec![1].into())).await.unwrap();
        assert_eq!(next_text(screen).await, None);
    }
    stopped(child);
    assert_eq!(home.read(CONFIG), config);
    assert!(!server.bodies().iter().any(|b| b.contains(TOLD)));
    assert!(
        !server
            .heads()
            .iter()
            .any(|h| h.starts_with("POST /v1/messages "))
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn a_change_shows_in_the_rows_and_the_next_tap_runs_on_it() {
    let server = Server::start(|_| text_reply("msg_web", "claude-sonnet-5-5", "ok", (1, 1)));
    let home = Home::configured("web-moves");
    let mut command = web(&home, &server);
    let config = home.read(CONFIG);
    let closed = free_port();
    let openai = format!(
        "[endpoints.oai]\nprovider = \"openai\"\nauth = \"api_key\"\nkey_env = \"GW_TOKEN\"\nurl = \"http://127.0.0.1:{closed}\"\n"
    );
    home.write(CONFIG, &format!("{config}{openai}"));
    let config = home.read(CONFIG);
    home.write(PICKS, "account = \"oai\"\nmodel = \"gpt-6.1-sol\"\n");
    let mut child = command.spawn().unwrap();
    let port = bound(&mut child);
    let host = format!("127.0.0.1:{port}");
    let mut socket = open(port, &host, Some(&format!("http://{host}")))
        .await
        .unwrap();
    assert_eq!(brain_rows(&mut socket, 6).await, ["oai", "gpt-6.1-sol"]);
    assert_eq!(
        asked(
            &mut socket,
            7,
            &[("endpoint", "gw"), ("model", "claude-opus-5-5")]
        )
        .await["type"],
        "settings_changed"
    );
    assert_eq!(brain_rows(&mut socket, 8).await, ["gw", "claude-opus-5-5"]);
    let body = tapped(&mut socket, &server, "B").await;
    assert!(body.contains(r#""model":"claude-opus-5-5""#), "{body}");
    let answer = asked(
        &mut socket,
        9,
        &[("endpoint", "gw"), ("model", "claude-sonnet-5-5")],
    )
    .await;
    assert_eq!(answer["type"], "settings_changed");
    assert_eq!(
        brain_rows(&mut socket, 10).await,
        ["gw", "claude-sonnet-5-5"]
    );
    tokio::time::sleep(wisp_tool_text::voice::TAP_GAP).await;
    let body = tapped(&mut socket, &server, "C").await;
    assert!(body.contains(r#""model":"claude-sonnet-5-5""#), "{body}");
    stopped(child);
    assert_eq!(home.read(CONFIG), config);
    assert_eq!(
        home.read(PICKS),
        "account = \"gw\"\nmodel = \"claude-sonnet-5-5\"\n"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn learn_starts_on_the_keys_file_account_and_names_an_open_key() {
    let server = Server::start(|_| text_reply("msg_web", "claude-sonnet-5-5", "no", (1, 1)));
    let home = Home::configured("web-keys");
    let keys = [("ANTHROPIC_API_KEY", "test-key-not-used")];
    let (rows, _) = fresh(&home, &server, "", &keys).await;
    assert_eq!(rows[0], "gateway", "{rows:?}");
    let key = home.write(".config/learn/web/key.pem", "not a key");
    std::fs::set_permissions(&key, std::fs::Permissions::from_mode(0o644)).unwrap();
    let output = learn(&home, &server).envs(keys).output().unwrap();
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert_eq!(output.status.code(), Some(1), "{stderr}");
    assert!(
        stderr
            .contains("learn: ~/.config/learn/web/key.pem is readable by other users (chmod 600)"),
        "{stderr}"
    );
    assert!(output.stdout.is_empty(), "{output:?}");
    assert!(
        !server
            .heads()
            .iter()
            .any(|h| h.starts_with("POST /v1/messages "))
    );
}

fn voice_row(rows: &Value) -> Value {
    values(rows)[3].clone()
}

#[tokio::test(flavor = "multi_thread")]
async fn a_lesson_continues_from_any_folder_and_its_picks_start_the_next() {
    let page = json!({"html": "<p>Fractions</p>"});
    let input = page.clone();
    let server = Server::start(move |seen| match seen.body.contains("tool_result") {
        true => text_reply("msg_web", "claude-sonnet-5-5", "Here.", (1, 1)),
        false => tool_reply("toolu_1", wisp_plugin_page::NAME, &input),
    });
    let home = Home::configured("web-resume");
    let talk = "[endpoints.talk]\nprovider = \"openai\"\nauth = \"chatgpt_subscription\"\nkey_env = \"TALK_TOKEN\"\n";
    configured(&home, &server, "");
    home.write(CONFIG, &format!("{}{talk}", home.read(CONFIG)));
    let start = |new: bool| {
        let mut command = learn(&home, &server);
        command.current_dir(home.dir("elsewhere"));
        command.args(new.then_some("--new"));
        let mut child = command.env("TALK_TOKEN", "talk-token").spawn().unwrap();
        let port = bound(&mut child);
        (child, port)
    };
    let (child, port) = start(false);
    let (mut socket, _) = screen(port).await;
    tapped(&mut socket, &server, "B").await;
    let started = until(&mut socket, |v| v["type"] == "tool_call_started").await;
    let report = json!({"type": "screen_report", "call_id": started["call_id"],
        "report": {"width": 380, "height": 560}, "images": [{"data": "/9j/AA=="}]});
    socket
        .send(Message::text(report.to_string()))
        .await
        .unwrap();
    until(&mut socket, |v| v["type"] == "turn_ended").await;
    let answered = server
        .bodies()
        .into_iter()
        .find(|b| b.contains("tool_result"));
    let answered = answered.unwrap();
    assert!(
        answered.contains("Shown on the board, 380 × 560 px."),
        "{answered}"
    );
    assert!(answered.contains(r#""data":"/9j/AA==""#), "{answered}");
    for (request, changes) in [
        (7, &[("endpoint", "gw"), ("model", "claude-sonnet-5-5")][..]),
        (8, &[("effort", "medium")]),
        (9, &[("voice.endpoint", "talk")]),
    ] {
        assert_eq!(
            asked(&mut socket, request, changes).await["type"],
            "settings_changed",
            "{changes:?}"
        );
    }
    let rows = described(&mut socket, 10).await;
    assert_eq!(voice_row(&rows), "talk", "{rows}");
    stopped(child);
    let saved = home.read(CONFIG);
    assert!(saved.contains("endpoint = \"talk\""), "{saved}");
    assert!(
        !saved.contains("sonnet") && !saved.contains("medium"),
        "{saved}"
    );
    let picks = "account = \"gw\"\nmodel = \"claude-sonnet-5-5\"\neffort = \"medium\"\n";
    assert_eq!(home.read(PICKS), picks);
    let (child, port) = start(false);
    let (mut socket, rows) = screen(port).await;
    assert_eq!(voice_row(&rows), "talk", "{rows}");
    let shown = until(&mut socket, |v| v["type"] == "tool_call_requested");
    let shown = tokio::time::timeout(WAIT, shown).await.unwrap();
    assert_eq!(shown["input"], page);
    let held = learn(&home, &server).output().unwrap();
    assert_eq!(held.status.code(), Some(1));
    assert_eq!(
        String::from_utf8_lossy(&held.stderr),
        "learn: the last lesson is open in another learn; close that one, or start fresh with learn --new\n"
    );
    let body = tapped(&mut socket, &server, "C").await;
    assert!(body.contains(r#""model":"claude-sonnet-5-5""#), "{body}");
    assert!(
        body.contains(r#""output_config":{"effort":"medium"}"#),
        "{body}"
    );
    let (_, stderr) = stopped(child);
    assert!(
        stderr.contains("Resumed session ") && !stderr.contains("--"),
        "{stderr}"
    );
    let turns: Vec<Value> = server
        .bodies()
        .iter()
        .map(|body| serde_json::from_str::<Value>(body).unwrap())
        .filter(|request| request["stream"] == true)
        .collect();
    assert_eq!(turns.len(), 3);
    crate::prefix::assert_prefix_stable(&turns, None);
    assert_eq!(lessons(&home).len(), 1);
    let (child, port) = start(true);
    let (mut socket, rows) = screen(port).await;
    assert_eq!(values(&rows)[..3], ["gw", "claude-sonnet-5-5", "medium"]);
    assert_eq!(voice_row(&rows), "talk", "{rows}");
    let spent = json!({"type": "spend_changed", "total": "$0.00"});
    assert_eq!(next_text(&mut socket).await, Some(spent));
    socket.send(Message::Ping(vec![1].into())).await.unwrap();
    assert_eq!(next_text(&mut socket).await, None);
    let body = tapped(&mut socket, &server, "D").await;
    assert!(!body.contains("Tapped: B"), "{body}");
    assert!(body.contains(r#""model":"claude-sonnet-5-5""#), "{body}");
    stopped(child);
    assert_eq!(lessons(&home).len(), 2);
}

#[tokio::test(flavor = "multi_thread")]
async fn a_pick_the_start_cant_use_is_dropped_with_its_line() {
    let server = Server::start(|_| text_reply("msg_web", "claude-sonnet-5-5", "ok", (1, 1)));
    let home = Home::configured("web-picks");
    let gone = "[endpoints.gone]\nauth = \"bearer\"\nkey_env = \"GONE_TOKEN\"\nurl = \"http://127.0.0.1:9\"\n";
    let broken = "[agents.presets.broken]\ndescription = \"B\"\nhammer = 1\n";
    configured(&home, &server, "");
    home.write(CONFIG, &format!("{}{gone}{broken}", home.read(CONFIG)));
    let (rows, stderr) = fresh(&home, &server, "effort = \"medium\"\n", &[]).await;
    assert_eq!(rows[..3], ["gw", "claude-opus-5-5", "medium"], "{stderr}");
    assert!(!stderr.contains("preset broken"), "{stderr}");
    let picks = "account = \"gone\"\nmodel = \"claude-sonnet-5-5\"\n";
    let (rows, stderr) = fresh(&home, &server, picks, &[]).await;
    assert_eq!(rows[..2], ["gw", "claude-opus-5-5"], "{stderr}");
    assert_eq!(
        stderr,
        "wisp: gone, where new chats start, can't be used now (GONE_TOKEN is not set in the environment); this chat starts on gw.\n"
    );
    let (rows, stderr) = fresh(&home, &server, "account = \"nowhere\"\n", &[]).await;
    assert_eq!(rows[0], "gw");
    assert_eq!(
        stderr,
        "learn: nowhere, picked in Settings, is no longer an account here; starting on gw\n"
    );
    let (rows, stderr) = fresh(&home, &server, "account = [\n", &[]).await;
    assert_eq!(rows[0], "gw");
    assert!(
        stderr.starts_with("learn: ~/.config/learn/settings.toml doesn't read (")
            && stderr.ends_with("); starting without it\n"),
        "{stderr}"
    );
    let keys = format!(
        "ANTHROPIC_API_KEY=test-key-not-used\nANTHROPIC_BASE_URL={}\n",
        server.url()
    );
    home.write(KEYS_FILE, &keys);
    let alias = wisp_protocol::KEYS_FILE_ALIAS;
    let voice = format!("{}[voice]\nendpoint = \"{alias}\"\n", home.read(CONFIG));
    home.write(CONFIG, &voice);
    let (rows, stderr) = fresh(&home, &server, "", &[]).await;
    assert_eq!(rows[0], "gw", "{stderr}");
    assert_eq!(rows[3], "gateway", "{stderr}");
    let (rows, stderr) = fresh(&home, &server, "account = \"gateway\"\n", &[]).await;
    assert_eq!(rows[0], "gateway", "{stderr}");
}

#[tokio::test(flavor = "multi_thread")]
async fn the_flows_settings_apply_over_the_users_config_and_a_session_change_wins() {
    let server = Server::start(|_| text_reply("msg_web", "claude-sonnet-5-5", "ok", (1, 1)));
    let home = Home::configured("web-flow");
    let other = "[endpoints.gw2]\nauth = \"bearer\"\nkey_env = \"GW_TOKEN\"\nurl = \"URL\"\n";
    let other = other.replace("URL", &server.url());
    let mine = "effort = \"high\"\ntools = [\"bash\"]\n\n";
    configured(&home, &server, mine);
    let presets =
        "[agents.presets.other]\ndescription = \"Another\"\ninstructions = \"OTHER.md\"\n";
    home.write(CONFIG, &format!("{}{other}{presets}", home.read(CONFIG)));
    let config = home.read(CONFIG);
    let mut child = learn(&home, &server)
        .args(["--account", "gw2"])
        .spawn()
        .unwrap();
    let port = bound(&mut child);
    let (mut socket, rows) = screen(port).await;
    assert_eq!(values(&rows)[0], "gw2", "{rows}");
    let body: Value = serde_json::from_str(&tapped(&mut socket, &server, "A").await).unwrap();
    assert_eq!(body["output_config"]["effort"], "high");
    let tools: Vec<&Value> = body["tools"]
        .as_array()
        .unwrap()
        .iter()
        .map(|t| &t["name"])
        .collect();
    assert_eq!(tools, [wisp_plugin_page::NAME]);
    let system = body["system"].to_string();
    assert!(system.contains("name=\\\"BRAIN.md\\\""), "{system}");
    assert!(!system.contains("OTHER.md"), "{system}");
    until(&mut socket, |v| v["type"] == "turn_ended").await;
    let picked = asked(&mut socket, 7, &[("effort", "medium")]).await;
    assert_eq!(picked["type"], "settings_changed");
    tokio::time::sleep(wisp_tool_text::voice::TAP_GAP).await;
    let body = tapped(&mut socket, &server, "B").await;
    assert!(
        body.contains(r#""output_config":{"effort":"medium"}"#),
        "{body}"
    );
    stopped(child);
    assert_eq!(home.read(CONFIG), config);
    configured(&home, &server, "");
    std::fs::remove_file(home.file(PICKS)).unwrap();
    let mut child = learn(&home, &server).arg("--new").spawn().unwrap();
    let port = bound(&mut child);
    let (mut socket, rows) = screen(port).await;
    assert_eq!(values(&rows)[2], "low", "{rows}");
    let body = tapped(&mut socket, &server, "C").await;
    assert!(
        body.contains(r#""output_config":{"effort":"low"}"#),
        "{body}"
    );
    stopped(child);
}
