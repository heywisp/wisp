use std::collections::{BTreeMap, VecDeque};
use std::convert::Infallible;
use std::future::Future;
use std::net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::time::Duration;

use futures::stream::SplitSink;
use futures::{SinkExt, StreamExt};
use http_body_util::Full;
use hyper::body::{Bytes, Incoming};
use hyper::header::{self, HeaderMap, HeaderName, HeaderValue};
use hyper::server::conn::http1;
use hyper::service::service_fn;
use hyper::upgrade::Upgraded;
use hyper::{Method, Request, Response, StatusCode};
use hyper_util::rt::{TokioIo, TokioTimer};
use serde_json::{Map, Value, json};
use tokio::io::{AsyncRead, AsyncWrite};
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::broadcast::error::RecvError;
use tokio::sync::{OwnedSemaphorePermit, Semaphore, broadcast, mpsc};
use tokio::task::JoinSet;
use tokio::time::{Instant, MissedTickBehavior};
use tokio_rustls::TlsAcceptor;
use tokio_rustls::rustls::ServerConfig;
use tokio_rustls::rustls::crypto::aws_lc_rs;
use tokio_rustls::rustls::pki_types::pem::{self, PemObject};
use tokio_rustls::rustls::pki_types::{CertificateDer, PrivateKeyDer};
use tokio_tungstenite::WebSocketStream;
use tokio_tungstenite::tungstenite::handshake::derive_accept_key;
use tokio_tungstenite::tungstenite::protocol::{Message, Role, WebSocketConfig};
use wisp_protocol::settings::{
    Chat, EndpointSummary, SettingChange, SettingTarget, SettingValue, SettingsSnapshot, names,
    offered, offers,
};
use wisp_protocol::voice::{AudioFrame, CallEnd, CallId, CallOutcome, VoiceCommand, VoiceEvent};
use wisp_protocol::{
    AccountSpend, AgentEvent, Billing, ClientCommand, RequestId, ToolOutcome, TurnOutcome,
    format_usd, marked_amount, shown_model,
};
use wisp_render::chrome::Credential;
use wisp_render::chrome::account::{
    AccountState, Surface, billing_token, billing_word, login_again, sign_in_again_with,
};
use wisp_render::motion::{self, MotionCtx, MotionPolicy, Presence};
use wisp_render::theme::glyphs::{self, GlyphSet};
use wisp_render::theme::{self, Depth, Mode, Theme};

pub const IN: &[&str] = &[
    "change_settings",
    "describe_settings",
    "screen_report",
    "voice",
];

pub const OUT: &[(&str, &[&str])] = &[
    ("turn_started", &[]),
    ("turn_ended", &[]),
    ("settings_changed", &["answers"]),
    ("spend_changed", &["total"]),
    ("rejected", &["reason", "request"]),
    ("voice", &["event/type", "event/call", "event/outcome"]),
    ("tool_call_requested", &["call_id", "tool", "input"]),
    ("tool_call_started", &["call_id"]),
    (
        "tool_call_finished",
        &["call_id", "outcome/status", "outcome/is_error"],
    ),
];

pub const REFUSED: &str = "Settings here can't make that change.";

pub const APP_POLICY: &str = "default-src 'self'; script-src 'self'; style-src 'self'; img-src 'self' data:; connect-src 'self'; frame-src 'self'; object-src 'none'; base-uri 'none'; form-action 'self'; frame-ancestors 'none'";

pub const APP_PERMISSIONS: &str = "microphone=(self), screen-wake-lock=(self), camera=(), geolocation=(), display-capture=(), payment=(), usb=(), serial=(), hid=(), bluetooth=()";

pub const FRAME_POLICY: &str = "sandbox allow-scripts; default-src 'none'; script-src 'unsafe-inline' blob:; style-src 'unsafe-inline'; img-src data: blob:; font-src data:; form-action 'none'; base-uri 'none'; frame-ancestors 'self'";

pub const FRAME_PERMISSIONS: &str = "autoplay=(), camera=(), microphone=(), screen-wake-lock=(), geolocation=(), display-capture=(), fullscreen=(), payment=(), usb=(), serial=(), hid=(), bluetooth=()";

pub const KEEP: usize = 1024 * 1024;

pub const MAX_MESSAGE: usize = 64 * 1024;

pub const PICTURE_BYTES: usize = 48 * 1024;

pub const SOUND_SECONDS: usize = 5;

#[derive(Debug)]
pub struct Library {
    pub name: &'static str,
    pub file: &'static str,
    pub sha256: &'static str,
    pub code: &'static str,
}

pub const LIBRARIES: [Library; 3] = [
    Library {
        name: "three",
        file: "three.module.min.js",
        sha256: "08fd7545d13d2c7fb65ab691530a802dafefd638596501854f267d0fb13c39e7",
        code: include_str!("../page/lib/three.module.min.js"),
    },
    Library {
        name: "three/addons/controls/OrbitControls.js",
        file: "OrbitControls.js",
        sha256: "80efaadea4f8a636a65fb0bd08bfef62f3d93a0bb94e2e7500f23176c5c07f4e",
        code: include_str!("../page/lib/OrbitControls.js"),
    },
    Library {
        name: "animejs",
        file: "anime.esm.min.js",
        sha256: "a19015a1a92d52025a2fb6703b6d67eadd1cc2aeaf880770e96e04cf6aa07be1",
        code: include_str!("../page/lib/anime.esm.min.js"),
    },
];

pub const ENCODER: Library = Library {
    name: "htmlToImage",
    file: "html-to-image.js",
    sha256: "a90b42909d80964269ef6d5f3d1e4a5a7e2a4c263a5d2a76a9e7151901343262",
    code: include_str!("../page/lib/html-to-image.js"),
};

pub const MAX_CONNECTIONS: usize = 128;

pub const PING: Duration = Duration::from_secs(5);

pub const FOLDER: &str = "web";
pub const CERTIFICATE: &str = "cert.pem";
pub const KEY: &str = "key.pem";

const MAX_HEAD: usize = 16 * 1024;
const LOOPBACK: [&str; 3] = ["127.0.0.1", "localhost", "[::1]"];
const START: RequestId = RequestId(1);
const BACKLOG: usize = 256;
const HEAD_TIME: Duration = Duration::from_secs(10);
const HANDSHAKE_TIME: Duration = Duration::from_secs(10);
const ACCEPT_PAUSE: Duration = Duration::from_millis(100);
const CHUNK: Presence = Presence::Answering {
    last_chunk: Some(Duration::ZERO),
};

const PAGE: [(&str, &str, &str); 4] = [
    ("/", HTML, include_str!("../page/index.html")),
    ("/app.js", JS, include_str!("../page/app.js")),
    ("/audio.js", JS, include_str!("../page/audio.js")),
    ("/app.css", CSS, include_str!("../page/app.css")),
];
const FRAME_PAGE: &str = include_str!("../page/frame.html");
const LIB: &str = "/lib/";
const HTML: &str = "text/html; charset=utf-8";
const JS: &str = "text/javascript; charset=utf-8";
const CSS: &str = "text/css; charset=utf-8";
const JSON: &str = "application/json";
const TEXT: &str = "text/plain; charset=utf-8";
const APP: Policies = [
    ("content-security-policy", APP_POLICY),
    ("permissions-policy", APP_PERMISSIONS),
];
const FRAME: Policies = [
    ("content-security-policy", FRAME_POLICY),
    ("permissions-policy", FRAME_PERMISSIONS),
];
const HEADERS: [(&str, &str); 3] = [
    ("x-content-type-options", "nosniff"),
    ("referrer-policy", "no-referrer"),
    ("cache-control", "no-store"),
];

type Policies = [(&'static str, &'static str); 2];

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct WebSetup {
    pub port: u16,
    pub folder: Option<PathBuf>,
    pub address: Option<IpAddr>,
}

#[derive(Clone, Copy, Debug)]
pub enum Shown<'a> {
    Address(&'a str),
    Event(&'a AgentEvent),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Ended {
    Stopped,
    AgentStopped,
}

struct Host {
    port: u16,
    tls: Option<TlsAcceptor>,
    theme: String,
    orb: String,
    commands: mpsc::WeakSender<ClientCommand>,
    out: broadcast::WeakSender<Line>,
    sheet: Arc<Sheet>,
    redact: Box<dyn Fn(&str) -> String + Send + Sync>,
    up: mpsc::Sender<AudioFrame>,
    calling: Mutex<Option<Calling>>,
    screens: AtomicU64,
    board: Mutex<Board>,
}

#[derive(Default)]
struct Board {
    asked: Vec<(String, Arc<str>)>,
    kept: VecDeque<[Arc<str>; 2]>,
    bytes: usize,
}

#[derive(Clone, Debug)]
struct Line {
    to: To,
    text: Arc<str>,
}

#[derive(Clone, Copy, Debug)]
enum To {
    All,
    Only(u64),
    Others(u64),
}

impl To {
    fn reaches(self, screen: u64) -> bool {
        match self {
            Self::All => true,
            Self::Only(asker) => asker == screen,
            Self::Others(asker) => asker != screen,
        }
    }
}

#[derive(Debug, Default)]
struct Known {
    rows: Option<Arc<str>>,
    spend: Option<Arc<str>>,
    asked: BTreeMap<RequestId, Asked>,
}

#[derive(Clone, Copy, Debug)]
struct Asked {
    screen: u64,
    request: RequestId,
}

struct Sheet {
    known: Mutex<Known>,
    next: AtomicU64,
}

struct Calling {
    screen: u64,
    call: CallId,
    live: bool,
    audio: mpsc::Sender<Message>,
    size: usize,
    samples: Vec<i16>,
    seq: u64,
}

pub fn failed_turn(event: &AgentEvent, model: &str) -> Option<String> {
    let shown = wisp_render::chrome::without_models(event, &[model]);
    match shown.as_ref().unwrap_or(event) {
        AgentEvent::TurnEnded {
            outcome: TurnOutcome::Error { message, .. },
            ..
        } => Some(wisp_render::chrome::one_line(&format!(
            "The brain couldn't answer: {message}"
        ))),
        _ => None,
    }
}

pub async fn serve(
    setup: WebSetup,
    (replayed, redact): (
        &[AgentEvent],
        impl Fn(&str) -> String + Send + Sync + 'static,
    ),
    commands: mpsc::Sender<ClientCommand>,
    events: &mut mpsc::Receiver<AgentEvent>,
    (up, mut down): (mpsc::Sender<AudioFrame>, mpsc::Receiver<AudioFrame>),
    stop: impl Future<Output = ()>,
    mut show: impl FnMut(Shown<'_>),
) -> Result<Ended, String> {
    let Some(snapshot) = described(&commands, events).await else {
        return Ok(Ended::AgentStopped);
    };
    let tls = setup.folder.as_deref().map(load_tls).transpose()?.flatten();
    let at: &[SocketAddr] = match tls {
        Some(_) => &[
            SocketAddr::new(Ipv6Addr::UNSPECIFIED.into(), setup.port),
            SocketAddr::new(Ipv4Addr::UNSPECIFIED.into(), setup.port),
        ],
        None => &[SocketAddr::new(Ipv4Addr::LOCALHOST.into(), setup.port)],
    };
    let listener = TcpListener::bind(at)
        .await
        .and_then(|l| Ok((l.local_addr()?.port(), l)))
        .map_err(|e| format!("could not serve on port {}: {e}", setup.port));
    let (port, listener) = listener?;
    let (out, _) = broadcast::channel(BACKLOG);
    let nothing = AgentEvent::SpendChanged {
        total: AccountSpend::default(),
        accounts: BTreeMap::new(),
    };
    let mut known = Known {
        spend: shown(&nothing).and_then(line),
        ..Known::default()
    };
    known.described(&snapshot);
    let sheet = Arc::new(Sheet {
        known: Mutex::new(known),
        next: AtomicU64::new(START.0 + 1),
    });
    let mut board = Board::default();
    replayed.iter().for_each(|event| board.heard(event));
    let palette = Theme::new(Mode::Dark, Depth::TrueColor);
    let line = match (&tls, setup.address) {
        (None, _) => format!("Open http://127.0.0.1:{port} on this computer \u{b7} ctrl-c stops"),
        (Some(_), Some(address)) => format!(
            "Open https://{} on each device \u{b7} ctrl-c stops",
            SocketAddr::new(address, port)
        ),
        (Some(_), None) => format!("Serving https on port {port} \u{b7} ctrl-c stops"),
    };
    let host = Arc::new(Host {
        port,
        tls,
        theme: theme::css(&palette),
        orb: orb(&palette),
        commands: commands.downgrade(),
        out: out.downgrade(),
        sheet: Arc::clone(&sheet),
        redact: Box::new(redact),
        up,
        calling: Mutex::new(None),
        screens: AtomicU64::new(0),
        board: Mutex::new(board),
    });
    show(Shown::Address(&line));
    let mut stop = std::pin::pin!(stop);
    let mut connections = JoinSet::new();
    let live = Arc::new(Semaphore::new(MAX_CONNECTIONS));
    loop {
        tokio::select! {
            () = &mut stop => return Ok(Ended::Stopped),
            event = events.recv() => {
                let Some(event) = event else {
                    return Ok(Ended::AgentStopped);
                };
                show(Shown::Event(&event));
                host.heard(&event);
                let mut board = host.board();
                board.heard(&event);
                fan_out(&out, &sheet, &event);
                drop(board);
            }
            frame = down.recv(), if !down.is_closed() => {
                if let Some(frame) = frame {
                    host.down(frame);
                }
            }
            accepted = listener.accept() => {
                let Ok((stream, peer)) = accepted else {
                    tokio::time::sleep(ACCEPT_PAUSE).await;
                    continue;
                };
                if private_peer(peer.ip())
                    && let Ok(permit) = Arc::clone(&live).try_acquire_owned()
                {
                    connections.spawn(connection(stream, Arc::clone(&host), permit));
                }
            }
            Some(_) = connections.join_next(), if !connections.is_empty() => {}
        }
    }
}

async fn described(
    commands: &mpsc::Sender<ClientCommand>,
    events: &mut mpsc::Receiver<AgentEvent>,
) -> Option<SettingsSnapshot> {
    let ask = ClientCommand::DescribeSettings { request: START };
    commands.send(ask).await.ok()?;
    loop {
        if let AgentEvent::SettingsSnapshot(snapshot) = events.recv().await?
            && snapshot.answers == START
        {
            return Some(snapshot);
        }
    }
}

fn load_tls(folder: &Path) -> Result<Option<TlsAcceptor>, String> {
    let (chain, key) = (folder.join(CERTIFICATE), folder.join(KEY));
    if !chain.exists() && !key.exists() {
        return Ok(None);
    }
    let chain = CertificateDer::pem_file_iter(&chain)
        .and_then(Iterator::collect::<Result<Vec<_>, _>>)
        .and_then(|chain| match chain.as_slice() {
            [] => Err(pem::Error::NoItemsFound),
            _ => Ok(chain),
        })
        .map_err(unread(CERTIFICATE, "certificate"))?;
    let key = PrivateKeyDer::from_pem_file(&key).map_err(unread(KEY, "private key"))?;
    ServerConfig::builder_with_provider(Arc::new(aws_lc_rs::default_provider()))
        .with_safe_default_protocol_versions()
        .and_then(|config| config.with_no_client_auth().with_single_cert(chain, key))
        .map(|config| Some(TlsAcceptor::from(Arc::new(config))))
        .map_err(|e| format!("{FOLDER}/{CERTIFICATE} and {FOLDER}/{KEY}: {e}"))
}

fn unread(file: &'static str, holds: &'static str) -> impl Fn(pem::Error) -> String {
    move |e| match e {
        pem::Error::Io(e) => format!("{FOLDER}/{file}: {e}"),
        _ => format!("{FOLDER}/{file} holds no {holds}"),
    }
}

pub fn private_peer(peer: IpAddr) -> bool {
    match peer.to_canonical() {
        IpAddr::V4(ip) => ip.is_loopback() || ip.is_private() || ip.is_link_local(),
        IpAddr::V6(ip) => ip.is_loopback() || ip.is_unique_local() || ip.is_unicast_link_local(),
    }
}

fn kind(value: &Value) -> Option<&str> {
    value.get("type")?.as_str()
}

fn shown(event: &AgentEvent) -> Option<Map<String, Value>> {
    let value = serde_json::to_value(event).ok()?;
    let (_, fields) = OUT.iter().find(|(name, _)| Some(*name) == kind(&value))?;
    let mut found = Value::Null;
    for field in std::iter::once("type").chain(fields.iter().copied()) {
        if let Some(part) = value.pointer(&format!("/{field}")) {
            *field.split('/').fold(&mut found, |at, key| &mut at[key]) = part.clone();
        }
    }
    let Value::Object(mut sent) = found else {
        return None;
    };
    if let AgentEvent::SpendChanged { total, .. } = event {
        sent.insert("total".to_owned(), Value::from(spent(total)));
    }
    Some(sent)
}

fn line(value: Map<String, Value>) -> Option<Arc<str>> {
    wisp_protocol::jsonl::line(&Value::Object(value))
        .ok()
        .map(Arc::from)
}

fn fan_out(out: &broadcast::Sender<Line>, sheet: &Sheet, event: &AgentEvent) {
    let send = |to, value| {
        if let Some(text) = line(value) {
            let _ = out.send(Line { to, text });
        }
    };
    if let AgentEvent::SettingsSnapshot(snapshot) = event {
        let mut known = sheet.known();
        known.described(snapshot);
        if let (Some(asked), Some(text)) = (known.asked.remove(&snapshot.answers), &known.rows) {
            let to = To::Only(asked.screen);
            let _ = out.send(Line {
                to,
                text: Arc::clone(text),
            });
        }
        return;
    }
    let Some(mut sent) = shown(event) else {
        return;
    };
    if let AgentEvent::SpendChanged { .. } = event {
        sheet.known().spend = line(sent.clone());
    }
    let (field, answers, alone) = match event {
        AgentEvent::Rejected { request, .. } => ("request", *request, true),
        AgentEvent::SettingsChanged { answers, .. } => ("answers", *answers, false),
        _ => ("", None, false),
    };
    sent.remove(field);
    match answers.and_then(|id| sheet.answered(id)) {
        Some(asked) => {
            if !alone {
                send(To::Others(asked.screen), sent.clone());
            }
            sent.insert(field.to_owned(), Value::from(asked.request.0));
            send(To::Only(asked.screen), sent);
        }
        None if alone => {}
        None => send(To::All, sent),
    }
}

fn heading(account: &EndpointSummary) -> Value {
    let billing = account.credential.as_ref().and_then(Credential::of);
    let state = match billing.map_or(Billing::Metered, Credential::billing) {
        _ if account.expired => AccountState::Expired,
        billing => AccountState::of(billing),
    };
    let sign_in = account.expired.then(|| {
        let command = login_again(Surface::Terminal, &account.name, true);
        let line = sign_in_again_with(&command);
        let (before, after) = line.split_once(&command).unwrap_or((&line, ""));
        [before.to_owned(), command.clone(), after.to_owned()]
    });
    json!({"name": account.name, "kind": billing_word(state),
        "tone": billing_token(state).name(), "expired": account.expired, "sign_in": sign_in})
}

fn group(heading: Value, items: impl IntoIterator<Item = Value>) -> Value {
    json!({"heading": heading, "items": items.into_iter().collect::<Vec<_>>()})
}

fn item(shown: &str, on: bool, changes: &[(&str, &str)]) -> Value {
    let changes: Vec<SettingChange> = changes
        .iter()
        .filter_map(|(name, text)| {
            SettingChange::named(name, SettingValue::Text((*text).to_owned()))
        })
        .collect();
    json!({"shown": shown, "on": on, "changes": changes})
}

fn spent(total: &AccountSpend) -> String {
    let charged = |usd| marked_amount(&format_usd(usd), false, total.lower_bound).to_string();
    match total.plan_usd {
        Some(plan) if plan > 0.0 => {
            let included = format!("about {}, included", format_usd(plan));
            match total.api_usd() {
                Some(api) if api > 0.0 => {
                    format!("{} {} {included}", charged(api), glyphs::UNICODE.separator)
                }
                _ => included,
            }
        }
        Some(_) => charged(total.usd),
        None => format!("about {}", format_usd(total.usd)),
    }
}

fn rows(snapshot: &SettingsSnapshot) -> Map<String, Value> {
    let context = &snapshot.context;
    let account = |name: &str| snapshot.endpoints.iter().find(|a| a.name == name);
    let effort = snapshot
        .settings
        .iter()
        .find(|s| s.name.as_str() == names::EFFORT);
    let efforts = effort.map_or(&[][..], |s| s.choices.as_slice());
    let effort = effort
        .and_then(|s| s.value.as_ref())
        .map(SettingValue::text);
    let effort = effort.filter(|_| !efforts.is_empty());
    let picks = efforts.iter().map(|e| {
        let on = effort.as_deref() == Some(e.as_str());
        item(e, on, &[(names::EFFORT, e)])
    });
    let efforts = (!efforts.is_empty()).then(|| {
        let mut efforts = group(json!({"name": "Effort"}), picks);
        efforts["effort"] = Value::Bool(true);
        efforts
    });
    let listed = offers(&snapshot.models);
    let at = snapshot.models.iter().find(|m| m.id == context.model);
    let chat = Chat {
        provider: at.and_then(|m| m.provider.as_deref()),
        model: &context.model,
    };
    let signed_in = snapshot
        .endpoints
        .iter()
        .filter(|a| a.unavailable.is_none());
    let models = signed_in.clone().map(|a| {
        let models = offered(&listed, a.landing(), chat).into_iter().map(|id| {
            let on = a.name == context.endpoint && id == context.model;
            item(
                shown_model(&id),
                on,
                &[(names::ENDPOINT, &a.name), (names::MODEL, &id)],
            )
        });
        group(heading(a), models)
    });
    let brain = json!({"model": shown_model(&context.model),
        "account": account(&context.endpoint).map(heading), "effort": effort,
        "groups": efforts.into_iter().chain(models).collect::<Vec<_>>()});
    let chosen = snapshot.voice_account().ok();
    let calls = chosen.as_deref().and_then(account);
    let voices = signed_in.filter_map(|a| {
        let on = chosen.as_deref() == Some(a.name.as_str());
        let model = item(
            a.voice_model.as_ref()?,
            on,
            &[(names::VOICE_ENDPOINT, &a.name)],
        );
        Some(group(heading(a), [model]))
    });
    let voice = json!({"model": calls.and_then(|a| a.voice_model.as_deref()),
        "account": calls.map(heading), "groups": voices.collect::<Vec<_>>()});
    Map::from_iter([("rows".to_owned(), json!({"brain": brain, "voice": voice}))])
}

impl Known {
    fn described(&mut self, snapshot: &SettingsSnapshot) {
        self.rows = line(rows(snapshot));
    }
}

impl Sheet {
    fn known(&self) -> std::sync::MutexGuard<'_, Known> {
        self.known.lock().unwrap_or_else(PoisonError::into_inner)
    }

    fn answered(&self, id: RequestId) -> Option<Asked> {
        self.known().asked.remove(&id)
    }

    fn forward(&self, mut command: ClientCommand, screen: u64) -> Result<ClientCommand, RequestId> {
        let request = match &mut command {
            ClientCommand::DescribeSettings { request } => request,
            ClientCommand::ChangeSettings {
                request,
                changes,
                save,
            } => match picked(changes) {
                Some(next_call) => {
                    *save = next_call;
                    request
                }
                None => return Err(*request),
            },
            _ => return Ok(command),
        };
        let own = RequestId(self.next.fetch_add(1, Ordering::Relaxed));
        let asked = Asked {
            screen,
            request: std::mem::replace(request, own),
        };
        self.known().asked.insert(own, asked);
        Ok(command)
    }
}

fn picked(changes: &[SettingChange]) -> Option<bool> {
    let named: Option<Vec<_>> = changes
        .iter()
        .map(|c| match (&c.target, &c.value) {
            (SettingTarget::Name(name), Some(_)) => Some(name),
            _ => None,
        })
        .collect();
    let named = named?;
    let mut sorted: Vec<&str> = named.iter().map(|n| n.as_str()).collect();
    sorted.sort_unstable();
    let one = [names::ENDPOINT, names::MODEL] == sorted.as_slice()
        || [names::EFFORT] == sorted.as_slice()
        || [names::VOICE_ENDPOINT] == sorted.as_slice();
    one.then(|| !named.first().is_some_and(|n| n.spec().mid_session))
}

fn voice(event: VoiceEvent) -> Option<Arc<str>> {
    shown(&AgentEvent::Voice { event }).and_then(line)
}

impl Board {
    fn heard(&mut self, event: &AgentEvent) {
        if !matches!(
            event,
            AgentEvent::ToolCallRequested { .. } | AgentEvent::ToolCallFinished { .. }
        ) {
            return;
        }
        let Some(line) = shown(event).and_then(self::line) else {
            return;
        };
        match event {
            AgentEvent::ToolCallRequested { call_id, .. } => {
                self.asked.push((call_id.clone(), line));
            }
            AgentEvent::ToolCallFinished {
                call_id, outcome, ..
            } => {
                let Some(at) = self.asked.iter().position(|(id, _)| id == call_id) else {
                    return;
                };
                let (_, asked) = self.asked.remove(at);
                if !matches!(
                    outcome,
                    ToolOutcome::Completed {
                        is_error: false,
                        ..
                    }
                ) {
                    return;
                }
                self.bytes += asked.len() + line.len();
                self.kept.push_back([asked, line]);
                while self.bytes > KEEP
                    && let Some(old) = self.kept.pop_front()
                {
                    self.bytes -= old.iter().map(|line| line.len()).sum::<usize>();
                }
            }
            _ => {}
        }
    }

    fn lines(&self) -> Vec<Arc<str>> {
        let asked = self.asked.iter().map(|(_, line)| line);
        self.kept.iter().flatten().chain(asked).cloned().collect()
    }
}

impl Host {
    fn board(&self) -> MutexGuard<'_, Board> {
        self.board.lock().unwrap_or_else(PoisonError::into_inner)
    }

    fn calling(&self) -> MutexGuard<'_, Option<Calling>> {
        self.calling.lock().unwrap_or_else(PoisonError::into_inner)
    }

    fn heard(&self, event: &AgentEvent) {
        let AgentEvent::Voice { event } = event else {
            return;
        };
        let mut calling = self.calling();
        match (event, calling.as_mut()) {
            (VoiceEvent::Started { call }, Some(held)) if held.call == *call => held.live = true,
            (VoiceEvent::Ended { call, .. }, Some(held)) if held.call == *call => *calling = None,
            _ => {}
        }
    }

    fn admitted(
        &self,
        screen: u64,
        command: &ClientCommand,
        audio: &mpsc::Sender<Message>,
    ) -> bool {
        let mut calling = self.calling();
        let command = match command {
            ClientCommand::Voice { command } => command,
            ClientCommand::ScreenReport { .. } => {
                return calling.as_ref().is_none_or(|held| held.screen == screen);
            }
            _ => return true,
        };
        match command {
            VoiceCommand::Start { call, format } if calling.is_none() => {
                *calling = Some(Calling {
                    screen,
                    call: call.clone(),
                    live: false,
                    audio: audio.clone(),
                    size: format.frame_samples(),
                    samples: Vec::new(),
                    seq: 0,
                });
                true
            }
            VoiceCommand::Start { .. } => false,
            VoiceCommand::Stop { call } => calling
                .as_ref()
                .is_some_and(|held| held.screen == screen && held.call == *call),
            VoiceCommand::Note { .. } => true,
        }
    }

    async fn received(
        &self,
        screen: u64,
        text: &str,
        audio: &mpsc::Sender<Message>,
    ) -> Option<Message> {
        let command = allowed(text)?;
        let command = match self.sheet.forward(command, screen) {
            Ok(command) => command,
            Err(request) => {
                let refused = shown(&AgentEvent::refused(request, REFUSED)).and_then(line);
                return refused.map(|text| Message::text(text.as_ref()));
            }
        };
        if self.admitted(screen, &command, audio) {
            if let Some(commands) = self.commands.upgrade() {
                let _ = commands.send(command).await;
            }
            return None;
        }
        let ClientCommand::Voice {
            command: VoiceCommand::Start { call, .. },
        } = command
        else {
            return None;
        };
        let end = CallEnd::Hangup;
        let outcome = CallOutcome::Completed { end };
        let refused = VoiceEvent::Ended {
            call,
            outcome,
            seconds: 0,
            cost: Default::default(),
        };
        voice(refused).map(|line| Message::text(line.as_ref()))
    }

    fn live_call(&self) -> Option<CallId> {
        self.calling()
            .as_ref()
            .filter(|held| held.live)
            .map(|held| held.call.clone())
    }

    fn left(&self, screen: u64) -> Option<CallId> {
        let mut calling = self.calling();
        let held = calling.take_if(|held| held.screen == screen)?;
        Some(held.call)
    }

    fn up(&self, screen: u64, bytes: &[u8]) {
        let mut calling = self.calling();
        let Some(held) = calling.as_mut().filter(|h| h.live && h.screen == screen) else {
            return;
        };
        let samples = bytes
            .as_chunks::<2>()
            .0
            .iter()
            .map(|b| i16::from_le_bytes(*b));
        held.samples.extend(samples);
        while held.samples.len() >= held.size {
            let samples = held.samples.drain(..held.size).collect();
            held.seq += 1;
            let (call, seq) = (held.call.clone(), held.seq);
            let _ = self.up.try_send(AudioFrame { call, seq, samples });
        }
    }

    fn down(&self, frame: AudioFrame) {
        let calling = self.calling();
        if let Some(held) = calling.as_ref().filter(|h| h.live && h.call == frame.call) {
            let bytes: Vec<u8> = frame.samples.iter().flat_map(|s| s.to_le_bytes()).collect();
            let _ = held.audio.try_send(Message::binary(bytes));
        }
    }
}

fn allowed(text: &str) -> Option<ClientCommand> {
    let value: Value = serde_json::from_str(text).ok()?;
    if !kind(&value).is_some_and(|k| IN.contains(&k)) {
        return None;
    }
    serde_json::from_value(value).ok()
}

async fn connection(stream: TcpStream, host: Arc<Host>, permit: OwnedSemaphorePermit) {
    let Some(tls) = host.tls.clone() else {
        return http(stream, host, permit).await;
    };
    if let Ok(Ok(stream)) = tokio::time::timeout(HANDSHAKE_TIME, tls.accept(stream)).await {
        http(stream, host, permit).await;
    }
}

async fn http(
    stream: impl AsyncRead + AsyncWrite + Send + Unpin + 'static,
    host: Arc<Host>,
    permit: OwnedSemaphorePermit,
) {
    let permit = Arc::new(permit);
    let service = service_fn(move |request| {
        let (host, permit) = (Arc::clone(&host), Arc::clone(&permit));
        async move { Ok::<_, Infallible>(route(request, &host, permit)) }
    });
    let _ = http1::Builder::new()
        .max_buf_size(MAX_HEAD)
        .timer(TokioTimer::new())
        .header_read_timeout(HEAD_TIME)
        .serve_connection(TokioIo::new(stream), service)
        .with_upgrades()
        .await;
}

fn route(
    mut request: Request<Incoming>,
    host: &Arc<Host>,
    permit: Arc<OwnedSemaphorePermit>,
) -> Response<Full<Bytes>> {
    if !from_page(request.headers(), host) {
        return status(StatusCode::FORBIDDEN);
    }
    if request.method() != Method::GET {
        return status(StatusCode::METHOD_NOT_ALLOWED);
    }
    let path = request.uri().path();
    let (kind, body) = match path {
        "/socket" => return socket(&mut request, host, permit),
        "/theme.css" => (CSS, Bytes::from(host.theme.clone())),
        "/orb.json" => (JSON, Bytes::from(host.orb.clone())),
        "/lib/map.json" => (JSON, Bytes::from(library_map())),
        "/frame" => {
            let frame = format!("{FRAME_PAGE}<script>{}</script>\n", ENCODER.code);
            return page_response(StatusCode::OK, HTML, frame.into(), FRAME);
        }
        _ => match PAGE.iter().find(|(at, _, _)| *at == path) {
            Some((_, kind, body)) => (*kind, Bytes::from_static(body.as_bytes())),
            None => match path.strip_prefix(LIB).and_then(library) {
                Some(library) => (TEXT, Bytes::from_static(library.code.as_bytes())),
                None => return status(StatusCode::NOT_FOUND),
            },
        },
    };
    page_response(StatusCode::OK, kind, body, APP)
}

fn library(file: &str) -> Option<&'static Library> {
    LIBRARIES.iter().find(|library| library.file == file)
}

fn library_map() -> String {
    let paths = LIBRARIES.iter().map(|library| {
        let path = format!("{LIB}{}", library.file);
        (library.name.to_owned(), Value::String(path))
    });
    Value::Object(paths.collect()).to_string()
}

fn one<'a>(headers: &'a HeaderMap, name: &HeaderName) -> Result<Option<&'a str>, ()> {
    let mut values = headers.get_all(name).iter();
    match (values.next(), values.next()) {
        (None, _) => Ok(None),
        (Some(value), None) => value.to_str().map(Some).map_err(|_| ()),
        (Some(_), Some(_)) => Err(()),
    }
}

fn from_page(headers: &HeaderMap, host: &Host) -> bool {
    let (Ok(Some(name)), Ok(origin)) = (one(headers, &header::HOST), one(headers, &header::ORIGIN))
    else {
        return false;
    };
    let (scheme, named) = match host.tls {
        Some(_) => ("https://", true),
        None => ("http://", loopback_host(name, host.port)),
    };
    named && origin.is_none_or(|origin| origin.strip_prefix(scheme) == Some(name))
}

fn loopback_host(host: &str, port: u16) -> bool {
    host.rsplit_once(':')
        .is_some_and(|(name, at)| LOOPBACK.contains(&name) && at == port.to_string())
}

fn socket(
    request: &mut Request<Incoming>,
    host: &Arc<Host>,
    permit: Arc<OwnedSemaphorePermit>,
) -> Response<Full<Bytes>> {
    let headers = request.headers();
    let is = |name: HeaderName, want: &str| {
        headers
            .get(name)
            .is_some_and(|v| v.as_bytes().eq_ignore_ascii_case(want.as_bytes()))
    };
    if !headers.contains_key(header::ORIGIN) {
        return status(StatusCode::FORBIDDEN);
    }
    let accept = match headers.get(header::SEC_WEBSOCKET_KEY) {
        Some(key)
            if is(header::UPGRADE, "websocket") && is(header::SEC_WEBSOCKET_VERSION, "13") =>
        {
            HeaderValue::try_from(derive_accept_key(key.as_bytes()))
        }
        _ => return status(StatusCode::BAD_REQUEST),
    };
    let (Ok(accept), Some(out)) = (accept, host.out.upgrade()) else {
        return status(StatusCode::SERVICE_UNAVAILABLE);
    };
    let upgrade = hyper::upgrade::on(request);
    let (out, kept) = {
        let board = host.board();
        (out.subscribe(), board.lines())
    };
    let host = Arc::clone(host);
    tokio::spawn(async move {
        if let Ok(upgraded) = upgrade.await {
            let config = WebSocketConfig::default()
                .max_message_size(Some(MAX_MESSAGE))
                .max_frame_size(Some(MAX_MESSAGE));
            let io = TokioIo::new(upgraded);
            let ws = WebSocketStream::from_raw_socket(io, Role::Server, Some(config)).await;
            screen(ws, &host, out, kept).await;
        }
        drop(permit);
    });
    let mut response = Response::new(Full::default());
    *response.status_mut() = StatusCode::SWITCHING_PROTOCOLS;
    let headers = response.headers_mut();
    headers.insert(header::CONNECTION, HeaderValue::from_static("upgrade"));
    headers.insert(header::UPGRADE, HeaderValue::from_static("websocket"));
    headers.insert(header::SEC_WEBSOCKET_ACCEPT, accept);
    response
}

type Socket = WebSocketStream<TokioIo<Upgraded>>;
type Sink = SplitSink<Socket, Message>;

async fn screen(ws: Socket, host: &Host, out: broadcast::Receiver<Line>, kept: Vec<Arc<str>>) {
    let me = host.screens.fetch_add(1, Ordering::Relaxed);
    talk(me, ws, host, out, kept).await;
    if let (Some(call), Some(commands)) = (host.left(me), host.commands.upgrade()) {
        let stop = VoiceCommand::Stop { call };
        let _ = commands.send(ClientCommand::Voice { command: stop }).await;
    }
}

async fn talk(
    me: u64,
    ws: Socket,
    host: &Host,
    mut out: broadcast::Receiver<Line>,
    kept: Vec<Arc<str>>,
) {
    let (mut sink, mut frames) = ws.split();
    let first: Vec<Arc<str>> = {
        let known = host.sheet.known();
        known.rows.iter().chain(&known.spend).cloned().collect()
    };
    for text in first {
        if !sent(&mut sink, Message::text(text.as_ref())).await {
            return;
        }
    }
    let live = host.live_call().map(|call| VoiceEvent::Started { call });
    for line in live.and_then(voice).into_iter().chain(kept) {
        if !sent(&mut sink, Message::text((host.redact)(&line))).await {
            return;
        }
    }
    let (audio, mut heard_audio) = mpsc::channel(BACKLOG);
    let mut ping = tokio::time::interval_at(Instant::now() + PING, PING);
    ping.set_missed_tick_behavior(MissedTickBehavior::Delay);
    let mut heard = true;
    loop {
        let reply = tokio::select! {
            line = out.recv() => match line {
                Ok(line) if line.to.reaches(me) => Some(Message::text((host.redact)(&line.text))),
                Ok(_) | Err(RecvError::Lagged(_)) => None,
                Err(RecvError::Closed) => return,
            },
            Some(samples) = heard_audio.recv() => Some(samples),
            _ = ping.tick() => match std::mem::replace(&mut heard, false) {
                true => Some(Message::Ping(Bytes::new())),
                false => return,
            },
            frame = frames.next() => {
                heard = true;
                match frame {
                    Some(Ok(Message::Text(text))) => host.received(me, &text, &audio).await,
                    Some(Ok(Message::Binary(bytes))) => {
                        host.up(me, &bytes);
                        None
                    }
                    Some(Ok(_)) => None,
                    None | Some(Err(_)) => return,
                }
            }
        };
        if let Some(reply) = reply
            && !sent(&mut sink, reply).await
        {
            return;
        }
    }
}

async fn sent(sink: &mut Sink, message: Message) -> bool {
    matches!(
        tokio::time::timeout(PING, sink.send(message)).await,
        Ok(Ok(()))
    )
}

fn status(code: StatusCode) -> Response<Full<Bytes>> {
    page_response(code, TEXT, Bytes::new(), APP)
}

fn page_response(
    code: StatusCode,
    kind: &'static str,
    body: Bytes,
    policies: Policies,
) -> Response<Full<Bytes>> {
    let mut response = Response::new(Full::new(body));
    *response.status_mut() = code;
    let headers = response.headers_mut();
    headers.insert(header::CONTENT_TYPE, HeaderValue::from_static(kind));
    for (name, value) in policies.into_iter().chain(HEADERS) {
        headers.insert(
            HeaderName::from_static(name),
            HeaderValue::from_static(value),
        );
    }
    response
}

fn orb(palette: &Theme) -> String {
    let states = [
        ("ready", Presence::Ready),
        ("listening", Presence::Listening),
        ("muted", Presence::Muted),
        ("thinking", Presence::Thinking),
        ("answering", Presence::Answering { last_chunk: None }),
        ("chunk", CHUNK),
        ("stopped", Presence::Stopped),
    ];
    let ms = |t: std::time::Duration| u64::try_from(t.as_millis()).unwrap_or(u64::MAX);
    let films = |policy| -> Map<String, Value> {
        let cx = MotionCtx::new(policy, GlyphSet::Unicode, Depth::TrueColor);
        let film = |state| {
            let film = motion::film(state, cx);
            let frames: Vec<Value> = film
                .frames
                .iter()
                .map(|(at, frame)| {
                    let dots: Vec<Value> = frame
                        .dots()
                        .into_iter()
                        .map(|(x, y, ink, part)| {
                            json!([x, y, palette.ink_rgb(ink).to_string(), part.name()])
                        })
                        .collect();
                    json!({"at": ms(*at), "dots": dots})
                })
                .collect();
            json!({"length": ms(film.length), "once": film.once, "frames": frames})
        };
        states
            .into_iter()
            .map(|(name, state)| (name.to_owned(), film(state)))
            .collect()
    };
    json!({"full": films(MotionPolicy::Full), "reduced": films(MotionPolicy::Reduced)}).to_string()
}
