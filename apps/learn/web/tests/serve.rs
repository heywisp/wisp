#![allow(
    clippy::unwrap_used,
    clippy::panic,
    reason = "tests fail on any socket or channel error"
)]

use std::net::{IpAddr, Ipv4Addr};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use agent::{describe, page_call, page_done, port, snapshot};
use client::{Socket, connect, get, get_over, next_text, open, open_over, page};
use futures::{SinkExt, StreamExt};
use hostile::{corpus, hostile_events};
use learn_web::{
    APP_PERMISSIONS, APP_POLICY, CERTIFICATE, ENCODER, Ended, FRAME_PERMISSIONS, FRAME_POLICY, IN,
    KEEP, KEY, LIBRARIES, MAX_CONNECTIONS, MAX_MESSAGE, OUT, PICTURE_BYTES, PING, REFUSED,
    SOUND_SECONDS, Shown, WebSetup, failed_turn, private_peer,
};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpStream;
use tokio::sync::{mpsc, oneshot};
use tokio::task::JoinHandle;
use tokio_rustls::TlsConnector;
use tokio_rustls::client::TlsStream;
use tokio_rustls::rustls::crypto::aws_lc_rs;
use tokio_rustls::rustls::pki_types::ServerName;
use tokio_rustls::rustls::{ClientConfig, RootCertStore};
use tokio_tungstenite::tungstenite::{Error, Message};
use wisp_protocol::voice::{
    AudioFormat, AudioFrame, CallId, CallOutcome, VoiceCommand, VoiceEvent,
};
use wisp_protocol::{
    AccountSpend, AgentEvent, BackgroundReason, ClientCommand, ErrorKind, RequestId, ToolOutcome,
    TurnOutcome, TurnStats,
};
use wisp_render::motion::Part;
use wisp_render::theme::{self, Depth, Mode, Theme};

#[path = "support/agent.rs"]
mod agent;
#[path = "support/client.rs"]
mod client;
#[path = "../../../../crates/wisp-cli/src/output/tests/hostile.rs"]
#[allow(
    dead_code,
    reason = "wisp-cli's output tests use the rest of these helpers"
)]
mod hostile;

const PAGE: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/page");
const BRAIN: &str = concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/../../../flows/interactive_education/BRAIN.md"
);
const WAIT: Duration = Duration::from_secs(10);

struct Running {
    port: u16,
    line: String,
    events: mpsc::Sender<AgentEvent>,
    passed: mpsc::Receiver<ClientCommand>,
    up: mpsc::Receiver<AudioFrame>,
    down: mpsc::Sender<AudioFrame>,
    stop: oneshot::Sender<()>,
    served: JoinHandle<Result<Ended, String>>,
}

fn account(name: &str, keys_file: bool) -> Value {
    json!({"name": name, "provider": "anthropic", "priced": true, "default": false, "keys_file": keys_file})
}

fn plain() -> WebSetup {
    WebSetup {
        port: 0,
        folder: None,
        address: None,
    }
}

async fn start(brain: &str, accounts: &[Value]) -> Result<Running, String> {
    start_on(plain(), brain, accounts).await
}

async fn start_on(setup: WebSetup, brain: &str, accounts: &[Value]) -> Result<Running, String> {
    start_after(setup, brain, accounts, Vec::new()).await
}

async fn start_after(
    setup: WebSetup,
    brain: &str,
    accounts: &[Value],
    replayed: Vec<AgentEvent>,
) -> Result<Running, String> {
    start_masking(setup, brain, accounts, replayed, str::to_owned).await
}

async fn start_masking(
    setup: WebSetup,
    brain: &str,
    accounts: &[Value],
    replayed: Vec<AgentEvent>,
    redact: impl Fn(&str) -> String + Send + Sync + 'static,
) -> Result<Running, String> {
    let (commands, mut asked) = mpsc::channel(8);
    let (events, mut agent) = mpsc::channel(64);
    let (pass, passed) = mpsc::channel(8);
    let (lane_up, up) = mpsc::channel(8);
    let (down, lane_down) = mpsc::channel(8);
    let (stop, stopped) = oneshot::channel::<()>();
    let (told, address) = oneshot::channel::<String>();
    let (brain, accounts, answer) = (brain.to_owned(), accounts.to_vec(), events.clone());
    tokio::spawn(async move {
        describe(&mut asked, &answer, &brain, &accounts).await;
        drop(answer);
        while let Some(command) = asked.recv().await {
            let _ = pass.send(command).await;
        }
    });
    let mut told = Some(told);
    let served = tokio::spawn(async move {
        let show = |shown: Shown<'_>| {
            if let (Shown::Address(line), Some(told)) = (shown, told.take()) {
                let _ = told.send(line.to_owned());
            }
        };
        let stop = async move {
            let _ = stopped.await;
        };
        let lane = (lane_up, lane_down);
        let replayed = (replayed.as_slice(), redact);
        learn_web::serve(setup, replayed, commands, &mut agent, lane, stop, show).await
    });
    let Ok(line) = address.await else {
        return Err(served.await.unwrap().unwrap_err());
    };
    let port = port(&line);
    Ok(Running {
        port,
        line,
        events,
        passed,
        up,
        down,
        stop,
        served,
    })
}

async fn saved() -> Running {
    let running = start("home", &[account("home", false)]).await.unwrap();
    let port = running.port;
    let line = format!("Open http://127.0.0.1:{port} on this computer \u{b7} ctrl-c stops");
    assert_eq!(running.line, line);
    running
}

async fn screen(port: u16) -> Socket {
    let host = format!("127.0.0.1:{port}");
    let mut socket = open(port, &host, Some(&format!("http://{host}")))
        .await
        .unwrap();
    let rows = next_text(&mut socket).await.unwrap();
    assert!(rows["rows"]["brain"].is_object(), "{rows}");
    let spent = json!({"type": "spend_changed", "total": "$0.00"});
    assert_eq!(next_text(&mut socket).await, Some(spent));
    socket
}

fn change(request: u64, changes: Value) -> Message {
    let command = json!({"type": "change_settings", "request": request, "save": true,
        "changes": changes});
    Message::text(command.to_string())
}

fn text(value: &str) -> Value {
    json!({"kind": "text", "text": value})
}

async fn within<T>(wait: impl Future<Output = T>) -> T {
    tokio::time::timeout(WAIT, wait).await.unwrap()
}

async fn closed(socket: &mut Socket) -> bool {
    within(async {
        loop {
            match socket.next().await {
                Some(Ok(Message::Text(_) | Message::Pong(_))) => return false,
                Some(Ok(Message::Close(_)) | Err(_)) | None => return true,
                Some(Ok(_)) => {}
            }
        }
    })
    .await
}

fn header<'a>(response: &'a str, name: &str) -> Option<&'a str> {
    response.lines().find_map(|line| {
        let (key, value) = line.split_once(": ")?;
        key.eq_ignore_ascii_case(name).then_some(value)
    })
}

#[tokio::test]
async fn every_file_of_the_page_carries_the_headers() {
    let running = saved().await;
    let port = running.port;
    for (path, kind) in [
        ("/", "text/html; charset=utf-8"),
        ("/app.js", "text/javascript; charset=utf-8"),
        ("/audio.js", "text/javascript; charset=utf-8"),
        ("/app.css", "text/css; charset=utf-8"),
        ("/theme.css", "text/css; charset=utf-8"),
        ("/orb.json", "application/json"),
        ("/lib/map.json", "application/json"),
    ] {
        let (code, response) = get(port, &page(port, path)).await;
        assert_eq!(code, 200, "{path}: {response}");
        assert_eq!(header(&response, "content-type"), Some(kind), "{path}");
        for (name, value) in [
            ("content-security-policy", APP_POLICY),
            ("permissions-policy", APP_PERMISSIONS),
            ("x-content-type-options", "nosniff"),
            ("referrer-policy", "no-referrer"),
            ("cache-control", "no-store"),
        ] {
            assert_eq!(header(&response, name), Some(value), "{path} {name}");
        }
    }
    assert!(!APP_POLICY.contains("unsafe-inline") && APP_POLICY.contains("frame-src 'self';"));
    let frame = r#"<iframe id="board" sandbox="allow-scripts" src="/frame" title="Board">"#;
    let files = page_files();
    assert_eq!(
        files.iter().filter(|(_, t)| t.contains("<iframe")).count(),
        1
    );
    assert!(files.iter().any(|(_, text)| text.contains(frame)));
    assert!(files.iter().all(|(_, t)| !t.contains("allow-same-origin")));
    let (_, theme) = get(port, &page(port, "/theme.css")).await;
    assert!(
        theme.contains(":root {") && theme.contains("  --text-muted: #"),
        "{theme}"
    );
    let (_, orb) = get(port, &page(port, "/orb.json")).await;
    let orb: Value = serde_json::from_str(orb.split("\r\n\r\n").nth(1).unwrap()).unwrap();
    for motion in ["full", "reduced"] {
        for state in [
            "ready",
            "listening",
            "muted",
            "thinking",
            "answering",
            "chunk",
            "stopped",
        ] {
            let dots = &orb[motion][state]["frames"][0]["dots"];
            assert!(dots.as_array().unwrap().len() >= 7, "{motion} {state}");
            assert!(dots[0][2].as_str().unwrap().starts_with('#'), "{dots}");
            let mut parts: Vec<&str> = dots
                .as_array()
                .unwrap()
                .iter()
                .map(|dot| dot[3].as_str().unwrap())
                .collect();
            parts.dedup();
            let (ring, core) = (Part::Ring.name(), Part::Core.name());
            assert_eq!(parts, [ring, core, ring], "{motion} {state}");
        }
    }
    assert!(orb["full"]["ready"]["frames"].as_array().unwrap().len() > 8);
    assert_eq!(orb["full"]["stopped"]["once"], true);
    assert_eq!(orb["full"]["chunk"]["once"], true);
    assert_eq!(orb["full"]["answering"]["once"], false);
    assert_eq!(get(port, &page(port, "/nothing")).await.0, 404);
    let post = format!("POST / HTTP/1.1\r\nHost: 127.0.0.1:{port}\r\nContent-Length: 0");
    assert_eq!(get(port, &post).await.0, 405);
    let long = format!("{}\r\nX-Long: {}", page(port, "/"), "a".repeat(20 * 1024));
    assert_eq!(get(port, &long).await.0, 431);
}

#[tokio::test]
async fn the_board_frame_has_its_own_sandbox_and_no_network() {
    let running = saved().await;
    let port = running.port;
    let (code, response) = get(port, &page(port, "/frame")).await;
    assert_eq!(code, 200, "{response}");
    for (name, value) in [
        ("content-type", "text/html; charset=utf-8"),
        ("content-security-policy", FRAME_POLICY),
        ("permissions-policy", FRAME_PERMISSIONS),
        ("x-content-type-options", "nosniff"),
        ("referrer-policy", "no-referrer"),
        ("cache-control", "no-store"),
    ] {
        assert_eq!(header(&response, name), Some(value), "{name}");
    }
    assert!(response.ends_with(&format!("</script>\n<script>{}</script>\n", ENCODER.code)));
    let rules: Vec<&str> = FRAME_POLICY.split("; ").collect();
    assert_eq!(rules[..2], ["sandbox allow-scripts", "default-src 'none'"]);
    assert!(rules.contains(&"script-src 'unsafe-inline' blob:"));
    for rule in [
        "form-action 'none'",
        "base-uri 'none'",
        "frame-ancestors 'self'",
    ] {
        assert!(rules.contains(&rule), "{rule}");
    }
    for fetches in ["connect-src", "media-src", "frame-src", "'self'", "http"] {
        let opened: Vec<&&str> = rules.iter().filter(|r| r.contains(fetches)).collect();
        assert!(
            opened.iter().all(|r| r.starts_with("frame-ancestors")),
            "{fetches}"
        );
    }
    for feature in [
        "autoplay",
        "camera",
        "microphone",
        "screen-wake-lock",
        "fullscreen",
    ] {
        assert!(
            FRAME_PERMISSIONS.contains(&format!("{feature}=()")),
            "{feature}"
        );
    }
    let head = format!("GET /frame HTTP/1.1\r\nHost: 127.0.0.1:{port}\r\nOrigin: null");
    assert_eq!(get(port, &head).await.0, 403);
}

#[tokio::test]
async fn a_new_screen_catches_up_on_the_successful_calls_of_the_last_mib() {
    let replayed = [
        page_call("old", json!({"html": "<p>0</p>"})),
        page_done("old", false),
        page_call("bad", json!({"html": "<p>x</p>"})),
        page_done("bad", true),
    ];
    let accounts = [account("home", false)];
    let running = start_after(plain(), "home", &accounts, replayed.to_vec()).await;
    let running = running.unwrap();
    let mut first = screen(running.port).await;
    let old = json!({"type": "tool_call_requested", "call_id": "old", "tool": "show_page",
        "input": {"html": "<p>0</p>"}});
    assert_eq!(next_text(&mut first).await, Some(old));
    let done = json!({"type": "tool_call_finished", "call_id": "old",
        "outcome": {"status": "completed", "is_error": false}});
    assert_eq!(next_text(&mut first).await, Some(done));
    let html = "x".repeat(60 * 1024);
    let calls = KEEP / html.len() + 2;
    let mut events = Vec::new();
    for at in 0..=calls {
        let call = format!("c{at}");
        events.push(page_call(&call, json!({"html": html})));
        events.push(page_done(&call, at == calls));
    }
    let reason = BackgroundReason::Started;
    events.push(page_call("bg", json!({"html": "<p>1</p>"})));
    events.push(AgentEvent::call_finished(
        "bg",
        ToolOutcome::Backgrounded { reason },
    ));
    events.push(page_call(
        "open",
        json!({"html": "<p>2</p>", "part": "step2"}),
    ));
    let count = events.len();
    for event in events {
        running.events.send(event).await.unwrap();
    }
    let mut seen = Vec::new();
    for _ in 0..count {
        seen.push(next_text(&mut first).await.unwrap());
    }
    let done = json!({"type": "tool_call_finished", "call_id": "c0",
        "outcome": {"status": "completed", "is_error": false}});
    assert_eq!(seen[1], done);
    let asked = json!({"type": "tool_call_requested", "call_id": "open", "tool": "show_page",
        "input": {"html": "<p>2</p>", "part": "step2"}});
    assert_eq!(seen[count - 1], asked);
    let size = |line: &Value| line.to_string().len() + 1;
    let fits = seen[..2 * calls]
        .chunks(2)
        .rev()
        .scan(0, |total, pair| {
            *total += size(&pair[0]) + size(&pair[1]);
            (*total <= KEEP).then_some(())
        })
        .count();
    assert!(fits > 1 && fits < calls, "{fits}");
    let mut late = screen(running.port).await;
    let kept = &seen[2 * (calls - fits)..2 * calls];
    for line in kept.iter().chain([&asked]) {
        assert_eq!(next_text(&mut late).await.as_ref(), Some(line));
    }
    settled(&mut late).await;
}

#[tokio::test]
async fn a_late_secret_is_masked_in_the_kept_and_live_calls_a_screen_gets() {
    const CANARY: &str = "CANARYOpaqueRegisteredAfterKept";
    let held = Arc::new(AtomicBool::new(false));
    let masking = Arc::clone(&held);
    let redact = move |line: &str| match masking.load(Ordering::SeqCst) {
        true => line.replace(CANARY, "[redacted]"),
        false => line.to_owned(),
    };
    let requested = |call: &str, html: &str| {
        json!({"type": "tool_call_requested", "call_id": call, "tool": "show_page",
            "input": {"html": html}})
    };
    let raw = format!("<p>{CANARY}</p>");
    let replayed = vec![
        page_call("old", json!({"html": raw})),
        page_done("old", false),
    ];
    let accounts = [account("home", false)];
    let running = start_masking(plain(), "home", &accounts, replayed, redact).await;
    let running = running.unwrap();
    let mut first = screen(running.port).await;
    assert_eq!(next_text(&mut first).await, Some(requested("old", &raw)));
    next_text(&mut first).await;
    let live = page_call("new", json!({"html": raw}));
    running.events.send(live).await.unwrap();
    assert_eq!(next_text(&mut first).await, Some(requested("new", &raw)));
    held.store(true, Ordering::SeqCst);
    let mut late = screen(running.port).await;
    let masked = "<p>[redacted]</p>";
    assert_eq!(next_text(&mut late).await, Some(requested("old", masked)));
    next_text(&mut late).await;
    assert_eq!(next_text(&mut late).await, Some(requested("new", masked)));
    let next = page_call("next", json!({"html": raw}));
    running.events.send(next).await.unwrap();
    for socket in [&mut first, &mut late] {
        assert_eq!(next_text(socket).await, Some(requested("next", masked)));
    }
    settled(&mut late).await;
}

#[tokio::test]
async fn a_wrong_host_or_origin_is_refused_on_every_request_and_the_socket() {
    let running = saved().await;
    let port = running.port;
    let other = port.wrapping_add(1);
    for name in ["127.0.0.1", "localhost", "[::1]"] {
        let head = format!("GET / HTTP/1.1\r\nHost: {name}:{port}");
        assert_eq!(get(port, &head).await.0, 200, "{name}");
    }
    for extra in [
        format!("Host: evil.example:{port}"),
        format!("Host: 127.0.0.1:{other}"),
        "Host: 127.0.0.1".to_owned(),
        format!("Host: 127.0.0.1:{port}\r\nOrigin: http://evil.example"),
        format!("Host: 127.0.0.1:{port}\r\nOrigin: null"),
        format!("Host: 127.0.0.1:{port}\r\nOrigin: https://127.0.0.1:{port}"),
        format!("Host: 127.0.0.1:{port}\r\nHost: localhost:{port}"),
        String::new(),
    ] {
        let head = format!("GET / HTTP/1.1\r\n{extra}");
        assert_eq!(get(port, &head).await.0, 403, "{extra}");
    }
    let host = format!("127.0.0.1:{port}");
    let own = format!("http://{host}");
    for (host, origin) in [
        (host.as_str(), None),
        (host.as_str(), Some("null")),
        (host.as_str(), Some("http://evil.example")),
        ("evil.example", Some("http://evil.example")),
    ] {
        match open(port, host, origin).await {
            Err(Error::Http(response)) => assert_eq!(response.status(), 403, "{origin:?}"),
            other => panic!("{host} {origin:?} opened: {:?}", other.map(|_| ())),
        }
    }
    assert!(open(port, &host, Some(&own)).await.is_ok());
}

#[tokio::test]
async fn a_command_outside_the_list_never_reaches_the_agent() {
    let mut running = saved().await;
    let mut socket = screen(running.port).await;
    let mut other = screen(running.port).await;
    assert_eq!(
        IN,
        [
            "change_settings",
            "describe_settings",
            "screen_report",
            "voice"
        ]
    );
    for refused in [
        json!([{"target": {"path": "effort"}, "value": text("low")}]),
        json!([{"target": {"name": "model"}}]),
        json!([{"target": {"name": "endpoint"}, "value": text("home")},
            {"target": {"name": "voice.endpoint"}, "value": text("talk")}]),
        json!([{"target": {"name": "endpoint"}, "value": text("home")}]),
        json!([{"target": {"name": "voice.voice"}, "value": text("cove")}]),
        json!([{"target": {"name": "endpoint"}, "value": text("home")},
            {"target": {"name": "model"}, "value": text("m")},
            {"target": {"name": "effort"}, "value": text("low")}]),
        json!([]),
    ] {
        socket.send(change(7, refused.clone())).await.unwrap();
        let answer = next_text(&mut socket).await;
        let rejected = json!({"type": "rejected", "reason": REFUSED, "request": 7});
        assert_eq!(answer, Some(rejected), "{refused}");
    }
    let pair = json!([{"target": {"name": "model"}, "value": text("m")},
        {"target": {"name": "endpoint"}, "value": text("home")}]);
    let voice = json!([{"target": {"name": "voice.endpoint"}, "value": text("talk")}]);
    let effort = json!([{"target": {"name": "effort"}, "value": text("high")}]);
    for (changes, save) in [(pair, true), (voice, false), (effort, true)] {
        let command = json!({"type": "change_settings", "request": 7, "save": save,
            "changes": changes});
        socket
            .send(Message::text(command.to_string()))
            .await
            .unwrap();
    }
    let describe = json!({"type": "describe_settings", "request": 7});
    socket
        .send(Message::text(describe.to_string()))
        .await
        .unwrap();
    let (mut own, mut saves) = (Vec::new(), Vec::new());
    for _ in 0..4 {
        match running.passed.recv().await.unwrap() {
            ClientCommand::ChangeSettings { request, save, .. } => {
                own.push(request);
                saves.push(save);
            }
            ClientCommand::DescribeSettings { request } => own.push(request),
            passed => panic!("{passed:?}"),
        }
    }
    assert_eq!(saves, [false, true, false]);
    let mut distinct = own.clone();
    distinct.sort_unstable();
    distinct.dedup();
    assert_eq!(distinct.len(), 4, "{own:?}");
    assert!(!own.contains(&RequestId(7)) && !own.contains(&RequestId(1)));
    let answered = |answers: RequestId| -> AgentEvent {
        serde_json::from_value(json!({"type": "settings_changed", "answers": answers,
            "endpoint": "home", "model": "m", "effort": "low", "cache": {"type": "kept"}}))
        .unwrap()
    };
    running.events.send(answered(own[0])).await.unwrap();
    let refusal = AgentEvent::refused(own[1], "talk isn't a voice account here");
    running.events.send(refusal).await.unwrap();
    let mine = json!({"type": "settings_changed", "answers": 7});
    assert_eq!(next_text(&mut socket).await, Some(mine));
    let theirs = json!({"type": "settings_changed"});
    assert_eq!(next_text(&mut other).await, Some(theirs));
    let reason =
        json!({"type": "rejected", "reason": "talk isn't a voice account here", "request": 7});
    assert_eq!(next_text(&mut socket).await, Some(reason));
    running.events.send(answered(own[0])).await.unwrap();
    for screen in [&mut socket, &mut other] {
        assert_eq!(
            next_text(screen).await,
            Some(json!({"type": "settings_changed"}))
        );
    }
    let moved = snapshot(own[3].0, "away", &[account("home", false)]);
    running.events.send(moved).await.unwrap();
    let rows = next_text(&mut socket).await.unwrap();
    assert_eq!(rows["rows"]["brain"]["account"], Value::Null, "{rows}");
    assert_eq!(rows["rows"]["brain"]["effort"], Value::Null, "{rows}");
    other.send(Message::Ping(vec![1].into())).await.unwrap();
    assert_eq!(next_text(&mut other).await, None);
    for command in [
        json!({"type": "send", "id": "web-1", "text": "hello", "mode": "queue"}),
        json!({"type": "change_settings", "request": 7, "save": true,
            "changes": [{"target": {"name": "nonsense"}, "value": text("m")}]}),
        json!({"type": "voice", "command": {"type": "stop", "call": "c"}}),
        json!({"type": "cancel"}),
        json!({"type": "remove_endpoint", "request": 7, "name": "home"}),
    ] {
        socket
            .send(Message::text(command.to_string()))
            .await
            .unwrap();
    }
    socket.send(Message::text("not json")).await.unwrap();
    socket.send(Message::binary(vec![0u8; 64])).await.unwrap();
    socket.send(Message::Ping(vec![1].into())).await.unwrap();
    assert_eq!(next_text(&mut socket).await, None);
    assert!(running.passed.try_recv().is_err());
    socket
        .send(Message::binary(vec![0u8; MAX_MESSAGE + 1]))
        .await
        .unwrap();
    assert!(closed(&mut socket).await);
    running.stop.send(()).unwrap();
    assert_eq!(running.served.await.unwrap(), Ok(Ended::Stopped));
}

#[tokio::test]
async fn the_rows_group_each_signed_in_accounts_models_and_voices() {
    let mut running = saved().await;
    let mut socket = screen(running.port).await;
    let describe = json!({"type": "describe_settings", "request": 5});
    socket
        .send(Message::text(describe.to_string()))
        .await
        .unwrap();
    let Some(ClientCommand::DescribeSettings { request }) = running.passed.recv().await else {
        panic!("no describe");
    };
    let named = |name: &str, provider: &str, credential: &str, more: Value| {
        let mut account = json!({"name": name, "provider": provider, "priced": true,
            "default": false, "credential": credential});
        account
            .as_object_mut()
            .unwrap()
            .extend(more.as_object().unwrap().clone());
        account
    };
    let accounts = [
        named(
            "old",
            "anthropic",
            "anthropic_subscription",
            json!({"expired": true, "voice_model": "v-old"}),
        ),
        named(
            "talk",
            "openai",
            "chatgpt_subscription",
            json!({"voice_model": "gpt-live-1-codex"}),
        ),
        named("key", "anthropic", "api_key", json!({})),
        named(
            "gone",
            "openai",
            "api_key",
            json!({"unavailable": "signed out", "voice_model": "v"}),
        ),
    ];
    let model = |id: &str, provider: &str, listed: Value| json!({"id": id, "shipped": true, "configured": false, "provider": provider, "listed": listed});
    let mut described = serde_json::to_value(snapshot(request.0, "key", &accounts)).unwrap();
    described["context"]["model"] = json!("claude-opus-5-5");
    described["models"] = json!([
        model("claude-sonnet-5-5", "anthropic", json!(1)),
        model("claude-opus-5-5", "anthropic", json!(0)),
        model("claude-hidden", "anthropic", Value::Null),
        model("gpt-6.1-sol", "openai", json!(0)),
    ]);
    described["settings"] = json!([{"name": "effort", "value": text("low"), "layer": "default",
        "origin": "default", "choices": ["low", "high"], "applies": true}]);
    let described = serde_json::from_value(described).unwrap();
    running.events.send(described).await.unwrap();
    let pick = |shown: &str, on: bool, changes: &[(&str, &str)]| {
        let changes: Vec<Value> = changes
            .iter()
            .map(|(name, value)| json!({"target": {"name": name}, "value": text(value)}))
            .collect();
        json!({"shown": shown, "on": on, "changes": changes})
    };
    let heading = |name: &str, kind: &str, tone: &str, sign_in: Value| {
        json!({"name": name, "kind": kind, "tone": tone, "expired": !sign_in.is_null(),
            "sign_in": sign_in})
    };
    let old = heading(
        "old",
        "sign-in expired",
        "error",
        json!(["Run ", "wisp login old", " to sign in again."]),
    );
    let talk = heading("talk", "subscription", "ok", Value::Null);
    let key = heading("key", "API", "text.muted", Value::Null);
    let brain = |account: &str, on: bool| {
        [("claude-opus-5-5", on), ("claude-sonnet-5-5", false)]
            .map(|(id, on)| pick(id, on, &[("endpoint", account), ("model", id)]))
    };
    let efforts = [("low", true), ("high", false)].map(|(e, on)| pick(e, on, &[("effort", e)]));
    let expected = json!({"rows": {
        "brain": {"model": "claude-opus-5-5", "account": key, "effort": "low", "groups": [
            {"heading": {"name": "Effort"}, "effort": true, "items": efforts},
            {"heading": old, "items": brain("old", false)},
            {"heading": talk, "items": [pick("gpt-6.1-sol", false,
                &[("endpoint", "talk"), ("model", "gpt-6.1-sol")])]},
            {"heading": key, "items": brain("key", true)},
        ]},
        "voice": {"model": "gpt-live-1-codex", "account": talk, "groups": [
            {"heading": old, "items": [pick("v-old", false, &[("voice.endpoint", "old")])]},
            {"heading": talk, "items": [pick("gpt-live-1-codex", true,
                &[("voice.endpoint", "talk")])]},
        ]},
    }});
    let sent = next_text(&mut socket).await.unwrap();
    assert_eq!(sent, expected);
    let changes = &sent["rows"]["brain"]["groups"][1]["items"][1]["changes"];
    socket.send(change(8, changes.clone())).await.unwrap();
    let Some(ClientCommand::ChangeSettings { changes, save, .. }) = running.passed.recv().await
    else {
        panic!("the pick wasn't passed");
    };
    assert_eq!(changes.len(), 2);
    assert!(!save);
}

#[tokio::test]
async fn only_the_events_the_page_reads_reach_every_screen() {
    let running = saved().await;
    let mut screens = [screen(running.port).await, screen(running.port).await];
    let private: Vec<AgentEvent> = [
        json!({"type": "input_accepted", "id": "v-1", "mode": "queue", "text": "my address", "held": false}),
        json!({"type": "text_delta", "index": 0, "text": "the brain's words"}),
        json!({"type": "accounts_gone", "names": ["home"]}),
        json!({"type": "agent", "agent": "/helper", "event": {"type": "turn_started", "turn": 9}}),
        json!({"type": "rejected", "reason": "no"}),
        json!({"type": "rejected", "reason": "no", "request": 99}),
    ]
    .into_iter()
    .map(|v| serde_json::from_value(v).unwrap())
    .chain([snapshot(1, "home", &[account("home", false)])])
    .collect();
    for event in private {
        running.events.send(event).await.unwrap();
    }
    running
        .events
        .send(AgentEvent::turn_started(1))
        .await
        .unwrap();
    let ended = AgentEvent::TurnEnded {
        outcome: TurnOutcome::Cancelled {
            usage: None,
            cost: None,
            committed_steps: 0,
        },
        stats: TurnStats::default(),
    };
    running.events.send(ended).await.unwrap();
    let started = json!({"type": "tool_call_started", "call_id": "p", "cwd": "~/lesson"});
    let started = serde_json::from_value(started).unwrap();
    running.events.send(started).await.unwrap();
    let applied = |target: Value| {
        json!({"target": target, "value": text("x"), "layer": "config",
        "when": "now", "saved": {"path_display": "~/.config/wisp/config.toml"}})
    };
    let settings = json!({"type": "settings_changed", "answers": 99, "endpoint": "home",
        "model": "x", "effort": "low", "cache": {"type": "kept"}, "changes": [
            applied(json!({"name": "model"})), applied(json!({"name": "display.theme"})),
            applied(json!({"path": "effort"}))]});
    let spend = json!({"type": "spend_changed", "total": {"usd": 0.6, "plan_usd": 0.6},
        "accounts": {"home": {"usd": 0.6, "plan_usd": 0.6}}});
    let spent = json!({"type": "spend_changed", "total": "about $0.60, included"});
    for event in [settings, spend] {
        let event = serde_json::from_value(event).unwrap();
        running.events.send(event).await.unwrap();
    }
    for socket in &mut screens {
        assert_eq!(
            next_text(socket).await,
            Some(json!({"type": "turn_started"}))
        );
        assert_eq!(next_text(socket).await, Some(json!({"type": "turn_ended"})));
        let started = json!({"type": "tool_call_started", "call_id": "p"});
        assert_eq!(next_text(socket).await, Some(started));
        let changed = json!({"type": "settings_changed"});
        assert_eq!(next_text(socket).await, Some(changed));
        assert_eq!(next_text(socket).await, Some(spent.clone()));
        socket.send(Message::Ping(vec![1].into())).await.unwrap();
        assert_eq!(next_text(socket).await, None);
    }
    let host = format!("127.0.0.1:{}", running.port);
    let mut late = open(running.port, &host, Some(&format!("http://{host}")))
        .await
        .unwrap();
    assert!(next_text(&mut late).await.unwrap()["rows"].is_object());
    assert_eq!(next_text(&mut late).await, Some(spent));
    drop(running.events);
    assert_eq!(running.served.await.unwrap(), Ok(Ended::AgentStopped));
    for socket in &mut screens {
        assert!(closed(socket).await);
    }
}

fn voice(command: Value) -> Message {
    Message::text(json!({"type": "voice", "command": command}).to_string())
}

fn start_call(call: &str) -> Message {
    voice(json!({"type": "start", "call": call, "format": "pcm16_mono_48k"}))
}

fn started(call: &str) -> (AgentEvent, Option<Value>) {
    let event = json!({"type": "voice", "event": {"type": "started", "call": call}});
    (serde_json::from_value(event.clone()).unwrap(), Some(event))
}

fn screen_report(call: &str) -> (Message, Option<ClientCommand>) {
    let report = json!({"type": "screen_report", "call_id": call, "report": {"width": 380}});
    let command = serde_json::from_value(report.clone()).ok();
    (Message::text(report.to_string()), command)
}

fn pcm(samples: &[i16]) -> Message {
    Message::binary(
        samples
            .iter()
            .flat_map(|s| s.to_le_bytes())
            .collect::<Vec<u8>>(),
    )
}

fn voice_command(command: VoiceCommand) -> Option<ClientCommand> {
    Some(ClientCommand::Voice { command })
}

async fn next_audio(socket: &mut Socket) -> Vec<u8> {
    within(async {
        loop {
            match socket.next().await {
                Some(Ok(Message::Binary(bytes))) => return bytes.to_vec(),
                Some(Ok(Message::Ping(_))) => {}
                other => panic!("not audio: {other:?}"),
            }
        }
    })
    .await
}

async fn settled(socket: &mut Socket) {
    socket.send(Message::Ping(vec![1].into())).await.unwrap();
    assert_eq!(next_text(socket).await, None);
}

#[tokio::test]
async fn only_the_calling_screen_sends_hears_and_stops_its_call() {
    let mut running = saved().await;
    let (mut a, mut b) = (screen(running.port).await, screen(running.port).await);
    let call = CallId("a".to_owned());
    let (report, passed) = screen_report("p");
    b.send(report.clone()).await.unwrap();
    assert_eq!(within(running.passed.recv()).await, passed);
    a.send(pcm(&[1; 960])).await.unwrap();
    a.send(start_call("a")).await.unwrap();
    let format = AudioFormat::Pcm16Mono48k;
    let start = VoiceCommand::Start {
        call: call.clone(),
        format,
    };
    assert_eq!(within(running.passed.recv()).await, voice_command(start));
    b.send(start_call("b")).await.unwrap();
    let refused = json!({"type": "voice", "event": {"type": "ended", "call": "b",
        "outcome": {"type": "completed", "end": "hangup"}}});
    assert_eq!(next_text(&mut b).await, Some(refused));
    a.send(pcm(&[2; 960])).await.unwrap();
    settled(&mut a).await;
    let (event, line) = started("a");
    running.events.send(event).await.unwrap();
    for socket in [&mut a, &mut b] {
        assert_eq!(next_text(socket).await, line);
    }
    let mut c = screen(running.port).await;
    assert_eq!(next_text(&mut c).await, line);
    b.send(pcm(&[3; 960])).await.unwrap();
    settled(&mut b).await;
    a.send(pcm(&[4; 1000])).await.unwrap();
    a.send(pcm(&[5; 920])).await.unwrap();
    let first = within(running.up.recv()).await.unwrap();
    let second = within(running.up.recv()).await.unwrap();
    assert_eq!(
        (first.call, first.seq, first.samples),
        (call.clone(), 1, vec![4; 960])
    );
    let joined: Vec<i16> = [vec![4; 40], vec![5; 920]].concat();
    assert_eq!((second.seq, second.samples), (2, joined));
    for socket in [&mut b, &mut c] {
        socket
            .send(voice(json!({"type": "stop", "call": "a"})))
            .await
            .unwrap();
        socket.send(report.clone()).await.unwrap();
        settled(socket).await;
    }
    assert!(running.passed.try_recv().is_err());
    a.send(report).await.unwrap();
    assert_eq!(within(running.passed.recv()).await, passed);
    assert!(running.up.try_recv().is_err());
    for (on, samples) in [("x", vec![9]), ("a", vec![1, -2])] {
        let (call, seq) = (CallId(on.to_owned()), 1);
        running
            .down
            .send(AudioFrame { call, seq, samples })
            .await
            .unwrap();
    }
    assert_eq!(next_audio(&mut a).await, vec![1, 0, 254, 255]);
    settled(&mut b).await;
    a.send(voice(json!({"type": "stop", "call": "a"})))
        .await
        .unwrap();
    let stop = VoiceCommand::Stop { call: call.clone() };
    assert_eq!(within(running.passed.recv()).await, voice_command(stop));
    let outcome = CallOutcome::Failed {
        kind: ErrorKind::Network,
        message: "The connection to ChatGPT failed.".to_owned(),
    };
    let shown =
        json!({"type": "voice", "event": {"type": "ended", "call": "a", "outcome": outcome}});
    let event = VoiceEvent::Ended {
        call: call.clone(),
        outcome,
        seconds: 12,
        cost: AccountSpend::default(),
    };
    running
        .events
        .send(AgentEvent::Voice { event })
        .await
        .unwrap();
    for socket in [&mut a, &mut b, &mut c] {
        assert_eq!(next_text(socket).await, Some(shown.clone()));
    }
    a.send(pcm(&[6; 960])).await.unwrap();
    let samples = vec![7];
    running
        .down
        .send(AudioFrame {
            call,
            seq: 2,
            samples,
        })
        .await
        .unwrap();
    settled(&mut a).await;
    assert!(running.up.try_recv().is_err());
}

#[tokio::test]
async fn a_call_ends_when_its_screen_closes_or_stops_answering() {
    let mut running = saved().await;
    for (name, silent) in [("closed", false), ("silent", true)] {
        let mut socket = screen(running.port).await;
        socket.send(start_call(name)).await.unwrap();
        let passed = within(running.passed.recv()).await;
        assert!(
            matches!(passed, Some(ClientCommand::Voice { .. })),
            "{passed:?}"
        );
        let (event, line) = started(name);
        running.events.send(event).await.unwrap();
        assert_eq!(next_text(&mut socket).await, line);
        let held = if silent {
            Some(socket)
        } else {
            drop(socket);
            None
        };
        let stop = tokio::time::timeout(PING * 3, running.passed.recv()).await;
        let call = CallId(name.to_owned());
        assert_eq!(
            stop.unwrap(),
            voice_command(VoiceCommand::Stop { call }),
            "{name}"
        );
        drop(held);
    }
}

#[test]
fn only_a_private_peer_gets_in() {
    for peer in [
        "127.0.0.1",
        "10.1.2.3",
        "172.16.0.1",
        "172.31.255.255",
        "192.168.1.20",
        "169.254.7.7",
        "::1",
        "::ffff:192.168.1.20",
        "::ffff:127.0.0.1",
        "fd12::1",
        "fc00::1",
        "fe80::1",
    ] {
        assert!(private_peer(peer.parse().unwrap()), "{peer}");
    }
    for peer in [
        "8.8.8.8",
        "172.32.0.1",
        "100.64.0.1",
        "192.0.2.1",
        "0.0.0.0",
        "::ffff:8.8.8.8",
        "::192.168.1.20",
        "2001:db8::1",
        "fec0::1",
        "::",
    ] {
        assert!(!private_peer(peer.parse().unwrap()), "{peer}");
    }
}

#[tokio::test]
async fn past_the_cap_a_connection_is_closed_at_once() {
    let running = saved().await;
    let screen = screen(running.port).await;
    let mut held = Vec::new();
    let head = format!("{}\r\n\r\n", page(running.port, "/"));
    for _ in 1..MAX_CONNECTIONS {
        let mut stream = connect(running.port).await;
        within(async {
            stream.write_all(head.as_bytes()).await.unwrap();
            assert_eq!(stream.read(&mut [0; 1]).await.unwrap(), 1);
        })
        .await;
        held.push(stream);
    }
    let mut extra = connect(running.port).await;
    let read = within(async {
        let _ = extra.write_all(head.as_bytes()).await;
        extra.read(&mut [0; 1]).await
    })
    .await;
    assert!(matches!(read, Ok(0) | Err(_)), "{read:?}");
    drop((screen, held));
}

fn certified() -> (tempfile::TempDir, RootCertStore) {
    let names = ["localhost".to_owned(), "127.0.0.1".to_owned()];
    let made = rcgen::generate_simple_self_signed(names).unwrap();
    let folder = tempfile::tempdir().unwrap();
    std::fs::write(folder.path().join(CERTIFICATE), made.cert.pem()).unwrap();
    std::fs::write(folder.path().join(KEY), made.signing_key.serialize_pem()).unwrap();
    let mut roots = RootCertStore::empty();
    roots.add(made.cert.der().clone()).unwrap();
    (folder, roots)
}

async fn tls(port: u16, roots: &RootCertStore) -> TlsStream<TcpStream> {
    let config = ClientConfig::builder_with_provider(Arc::new(aws_lc_rs::default_provider()))
        .with_safe_default_protocol_versions()
        .unwrap()
        .with_root_certificates(roots.clone())
        .with_no_client_auth();
    let name = ServerName::try_from("127.0.0.1").unwrap();
    let connector = TlsConnector::from(Arc::new(config));
    connector.connect(name, connect(port).await).await.unwrap()
}

#[tokio::test]
async fn https_serves_every_device_with_the_origin_of_its_host() {
    let (folder, roots) = certified();
    let setup = |address| WebSetup {
        port: 0,
        folder: Some(folder.path().to_owned()),
        address,
    };
    let account = [account("home", false)];
    let unknown = start_on(setup(None), "home", &account).await.unwrap();
    let line = format!("Serving https on port {} \u{b7} ctrl-c stops", unknown.port);
    assert_eq!(unknown.line, line);
    let home: IpAddr = Ipv4Addr::LOCALHOST.into();
    let running = start_on(setup(Some(home)), "home", &account).await.unwrap();
    let port = running.port;
    let line = format!("Open https://127.0.0.1:{port} on each device \u{b7} ctrl-c stops");
    assert_eq!(running.line, line);
    let host = format!("192.168.1.20:{port}");
    let own = format!("https://{host}");
    for (origin, code) in [
        (None, 200),
        (Some(own.as_str()), 200),
        (Some("http://x"), 403),
    ] {
        let origin = origin.map_or_else(String::new, |o| format!("\r\nOrigin: {o}"));
        let head = format!("GET / HTTP/1.1\r\nHost: {host}{origin}");
        assert_eq!(
            get_over(tls(port, &roots).await, &head).await.0,
            code,
            "{origin}"
        );
    }
    let http = format!("http://{host}");
    for origin in [
        None,
        Some("null"),
        Some(http.as_str()),
        Some("https://evil.example"),
    ] {
        match open_over(tls(port, &roots).await, &host, origin).await {
            Err(Error::Http(response)) => assert_eq!(response.status(), 403, "{origin:?}"),
            other => panic!("{origin:?} opened: {:?}", other.map(|_| ())),
        }
    }
    assert!(
        open_over(tls(port, &roots).await, &host, Some(&own))
            .await
            .is_ok()
    );
}

fn page_files() -> Vec<(String, String)> {
    let mut files: Vec<(String, String)> = std::fs::read_dir(PAGE)
        .unwrap()
        .map(|entry| entry.unwrap().path())
        .filter(|path| path.is_file())
        .map(|path| {
            let name = path.file_name().unwrap().to_string_lossy().into_owned();
            (name, std::fs::read_to_string(&path).unwrap())
        })
        .collect();
    files.sort();
    files
}

fn colours(text: &str) -> Vec<String> {
    let lower = text.to_ascii_lowercase();
    let functions = [
        "rgb(", "rgba(", "hsl(", "hsla(", "hwb(", "lab(", "lch(", "oklab(", "oklch(", "color(",
    ];
    let mut found: Vec<String> = functions
        .iter()
        .filter(|f| lower.contains(*f))
        .map(|f| (*f).to_owned())
        .collect();
    for (at, _) in text.match_indices('#') {
        let before = text[..at].trim_end_matches([' ', '"', '\'']);
        let hex = text[at + 1..]
            .chars()
            .take_while(char::is_ascii_hexdigit)
            .count();
        if (before.ends_with(':') || before.ends_with('=')) && hex >= 3 {
            found.push(text[at..at + 1 + hex].to_owned());
        }
    }
    found
}

#[test]
fn no_file_of_the_page_holds_a_colour_value() {
    let files = page_files();
    assert!(files.iter().any(|(name, _)| name == "app.css"));
    for (name, text) in &files {
        assert_eq!(colours(text), Vec::<String>::new(), "{name}");
    }
    for caught in [
        "a { color: #fff; }",
        "<p style=\"fill:#a0b0c0\">",
        "<rect fill=\"#123\">",
        "b { color: RGB(1, 2, 3) }",
    ] {
        assert_eq!(colours(caught).len(), 1, "{caught}");
    }
    assert!(colours("#gear svg { fill: none; } #add { stroke: currentColor; }").is_empty());
}

#[test]
fn the_events_app_js_handles_are_exactly_out() {
    let app = std::fs::read_to_string(format!("{PAGE}/app.js")).unwrap();
    let table = app
        .split_once("const handlers = {")
        .and_then(|(_, rest)| rest.split_once("};"))
        .unwrap()
        .0;
    let mut handled: Vec<&str> = table
        .lines()
        .filter_map(|line| line.trim().split_once(':'))
        .map(|(name, _)| name)
        .collect();
    handled.sort_unstable();
    let mut out: Vec<&str> = OUT.iter().map(|(name, _)| *name).collect();
    out.sort_unstable();
    assert_eq!(handled, out);
}

#[test]
fn the_page_names_the_audio_format_and_the_page_tool() {
    let app = std::fs::read_to_string(format!("{PAGE}/app.js")).unwrap();
    let frame = std::fs::read_to_string(format!("{PAGE}/frame.html")).unwrap();
    let schema = wisp_plugin_page::schema();
    let fields: Vec<&String> = schema["properties"].as_object().unwrap().keys().collect();
    let fields = fields
        .iter()
        .map(|f| f.as_str())
        .collect::<Vec<_>>()
        .join(", ");
    let tool = format!("const TOOL = \"{}\";", wisp_plugin_page::NAME);
    assert!(app.contains(&tool) && app.contains(&format!("input: {{ {fields} }}")));
    assert!(frame.contains(&format!("data: {{ {fields}, call, theme, libraries }}")));
    let tell = format!("globalThis.{} = (text) =>", wisp_plugin_page::TELL);
    assert!(frame.contains(&tell));
    let sound = format!(
        "globalThis.{} = async (seconds, build) =>",
        wisp_plugin_page::SOUND
    );
    assert!(frame.contains(&sound));
    let brain = std::fs::read_to_string(BRAIN).unwrap();
    assert!(brain.contains(&format!("`{}(", wisp_plugin_page::SOUND)));
    let report = wisp_plugin_page::REPORT_FIELDS.join(", ");
    assert!(frame.contains(&format!("found = {{ {report}: errors.splice(0) }}")));
    let errors = wisp_plugin_page::MAX_ERRORS * wisp_plugin_page::MAX_ERROR_CHARS * 6;
    assert!(PICTURE_BYTES + errors + 1024 <= MAX_MESSAGE);
    for (name, cap) in [
        ("MAX_ERRORS", wisp_plugin_page::MAX_ERRORS),
        ("MAX_ERROR_CHARS", wisp_plugin_page::MAX_ERROR_CHARS),
        ("PICTURE_BYTES", PICTURE_BYTES),
        ("SOUND_SECONDS", SOUND_SECONDS),
        (
            "SOUND_RATE",
            AudioFormat::Pcm16Mono48k.sample_rate() as usize,
        ),
    ] {
        assert!(frame.contains(&format!("const {name} = {cap};")), "{name}");
    }
    assert!(app.contains(&format!("const CAP = {MAX_MESSAGE};")));
    assert!(app.contains(&format!("const SOUND_SECONDS = {SOUND_SECONDS};")));
    let format = AudioFormat::Pcm16Mono48k;
    let name = serde_json::to_value(format).unwrap();
    let named = format!(
        "const FORMAT = {{ name: {name}, rate: {} }};",
        format.sample_rate()
    );
    assert!(app.contains(&named), "{named}");
    let swell = app
        .split_once("const SWELL = { ")
        .and_then(|(_, rest)| rest.split_once(" };"))
        .unwrap()
        .0;
    let parts: Vec<&str> = swell
        .split(", ")
        .filter_map(|size| Some(size.split_once(": ")?.0))
        .collect();
    assert_eq!(parts, [Part::Ring.name(), Part::Core.name()]);
}

#[tokio::test]
async fn the_bundled_libraries_are_served_as_shipped_and_only_brain_md_names_them() {
    let running = saved().await;
    let port = running.port;
    let (_, map) = get(port, &page(port, "/lib/map.json")).await;
    let map: Value = serde_json::from_str(map.split("\r\n\r\n").nth(1).unwrap()).unwrap();
    assert_eq!(map.as_object().unwrap().len(), LIBRARIES.len());
    let imports_relative = |code: &str| {
        let starts = ["from", "from ", "import", "import ", "import("];
        let quoted = |start: &&str| {
            ["\"", "'"]
                .iter()
                .any(|q| code.contains(&format!("{start}{q}.")))
        };
        starts.iter().any(quoted)
    };
    for relative in [
        r#"import { Vector3 } from "./three.core.js";"#,
        "import'./x.js'",
    ] {
        assert!(imports_relative(relative), "{relative}");
    }
    let files = page_files();
    let brain = std::fs::read_to_string(BRAIN).unwrap();
    for library in &LIBRARIES {
        let path = format!("/lib/{}", library.file);
        assert_eq!(map[library.name], path.as_str());
        let (code, response) = get(port, &page(port, &path)).await;
        assert_eq!(
            (code, header(&response, "content-type")),
            (200, Some("text/plain; charset=utf-8"))
        );
        assert!(response.ends_with(library.code), "{path}");
        assert!(!imports_relative(library.code), "{path}");
        assert!(
            brain.contains(&format!("\"{}\"", library.name)),
            "{}",
            library.name
        );
        for (name, text) in &files {
            assert!(
                !text.contains(library.name),
                "{name} names {}",
                library.name
            );
        }
    }
    for library in LIBRARIES.iter().chain([&ENCODER]) {
        let file = std::fs::read(format!("{PAGE}/lib/{}", library.file)).unwrap();
        let sha: String = Sha256::digest(&file)
            .iter()
            .map(|b| format!("{b:02x}"))
            .collect();
        assert_eq!(
            (sha.as_str(), file.as_slice()),
            (library.sha256, library.code.as_bytes())
        );
    }
    let frame = std::fs::read_to_string(format!("{PAGE}/frame.html")).unwrap();
    assert!(frame.contains(&format!(" = {}.toCanvas(", ENCODER.name)));
    assert_eq!(get(port, &page(port, "/lib/html-to-image.js")).await.0, 404);
}

#[test]
fn every_colour_brain_md_names_is_a_theme_token() {
    let brain = std::fs::read_to_string(BRAIN).unwrap();
    let tokens = theme::css(&Theme::new(Mode::Dark, Depth::TrueColor));
    let named: Vec<&str> = brain
        .split("var(")
        .skip(1)
        .filter_map(|rest| Some(rest.split_once(')')?.0))
        .collect();
    assert!(named.len() >= 6, "{named:?}");
    for name in named {
        assert!(tokens.contains(&format!("  {name}: ")), "{name}");
    }
}

#[test]
fn only_a_failed_main_turn_prints_its_line_sanitized_without_the_model() {
    for h in corpus() {
        let lines: Vec<String> = hostile_events(&h)
            .iter()
            .filter_map(|event| failed_turn(event, "claude-x"))
            .collect();
        assert_eq!(lines.len(), 1, "{h:?}");
        assert!(
            lines[0].starts_with("The brain couldn't answer: "),
            "{lines:?}"
        );
        assert!(!lines[0].chars().any(char::is_control), "{lines:?}");
    }
    let failed = |message: &str| AgentEvent::TurnEnded {
        outcome: TurnOutcome::Error {
            kind: ErrorKind::Overloaded,
            message: message.to_owned(),
            usage: None,
            cost: None,
            committed_steps: 0,
            detail: None,
            attempts: 0,
            gave_up: None,
            offer: None,
        },
        stats: TurnStats::default(),
    };
    let key = "sk-ant-api03-CANARYMODEL0000";
    let line = failed_turn(&failed(&format!("model: {key}")), key).unwrap();
    assert_eq!(
        line,
        format!(
            "The brain couldn't answer: model: {}",
            wisp_protocol::NOT_SHOWN
        )
    );
    let child = AgentEvent::Agent {
        agent: wisp_protocol::AgentPath("/helper".to_owned()),
        event: Box::new(failed("busy")),
    };
    assert_eq!(failed_turn(&child, "claude-x"), None);
}
