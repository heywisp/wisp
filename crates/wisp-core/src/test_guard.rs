use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use rustix::process::Pid;
use serde_json::{Value, json};
use tokio::sync::mpsc;
use wisp_protocol::log::{CredentialSource, LogEvent, LogRecord};
use wisp_protocol::plan_usage::UsageState;
use wisp_protocol::settings::SettingValue;
use wisp_protocol::{
    Activity, AgentEvent, AgentPath, AgentState, ContentBlock, ErrorKind, FailurePhase, MailId,
    MailKind, Message, RetryClass, Role,
};

use crate::agent::LogContext;
use crate::agent::testing::{Step, classified};
use crate::agents::news::{MailNews, StateNews, TreeNews};
use crate::context::ModelSettings;
use crate::endpoints::{ConfigFile, EndpointName, PathShown};
use crate::jobs::exec::{HostSpawner, find_program};
use crate::models::{ConfigModels, ModelTable};
use crate::provider::{ProviderError, ProviderEvent};
use crate::session::{MemoryStore, Redactions, SessionLog, SessionStore, new_session_id};
use crate::workspace::Workspace;

pub(crate) fn mcp_server() -> String {
    std::env::current_exe()
        .unwrap()
        .parent()
        .unwrap()
        .parent()
        .unwrap()
        .join("mcp-test-server")
        .to_str()
        .unwrap()
        .to_owned()
}

pub(crate) mod http;
#[path = "../../wisp-tui/tests/support/pick.rs"]
mod pick;
#[path = "../../wisp-cli/tests/it/support/prefix.rs"]
pub(crate) mod prefix;
#[path = "../../wisp-tui/tests/support/scratch.rs"]
mod scratch;
#[path = "../tests/session_locks/shared.rs"]
pub(crate) mod shared;

pub(crate) use fixture::RED_PIXEL;
pub(crate) use pick::pick;
pub(crate) use scratch::{ScratchDir, alive};

#[allow(
    dead_code,
    reason = "core reads the protocol's fixtures and never blesses them"
)]
#[path = "../../wisp-protocol/tests/support/fixture.rs"]
pub(crate) mod fixture;

const PATIENCE: Duration = Duration::from_secs(60);

pub(crate) const BLUE_PIXEL: &str =
    "iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAIAAACQd1PeAAAADElEQVR4nGNgYPgPAAEDAQAIicLsAAAAAElFTkSuQmCC";
pub(crate) const GREEN_PIXEL: &str =
    "iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAIAAACQd1PeAAAADElEQVR4nGNg+M8AAAICAQB7CYF4AAAAAElFTkSuQmCC";
pub(crate) const YELLOW_PIXEL: &str =
    "iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAIAAACQd1PeAAAADElEQVR4nGP4/58BAAT/Af9dfQKHAAAAAElFTkSuQmCC";

pub(crate) fn table_rows<'a>(doc: &'a str, header: &'a str) -> impl Iterator<Item = &'a str> {
    doc.lines()
        .skip_while(move |line| !line.starts_with(header))
        .skip(2)
        .take_while(|line| line.starts_with('|'))
}

pub(crate) fn ticked(text: &str) -> Vec<&str> {
    text.split('`').skip(1).step_by(2).collect()
}

pub(crate) fn protocol_fixtures() -> PathBuf {
    fixture::directory()
}

pub(crate) fn protocol_fixture(name: &str) -> LogRecord {
    let path = protocol_fixtures().join(format!("{name}.json"));
    serde_json::from_str(&fixture::stored(&fs::read_to_string(path).unwrap())).unwrap()
}

pub(crate) fn log_context() -> LogContext {
    LogContext {
        endpoint: "https://api.anthropic.com".into(),
        endpoint_name: "env".into(),
        credential_source: CredentialSource::Environment,
        redactions: Redactions::default(),
    }
}

pub(crate) fn stored(records: &[LogRecord]) -> MemoryStore {
    let store = MemoryStore::default();
    let mut writer = store.clone();
    for record in records {
        writer.append("s1", record).unwrap();
    }
    store
}

pub(crate) fn continued(records: &[LogRecord]) -> (MemoryStore, SessionLog) {
    let store = stored(records);
    let seq = u64::try_from(records.len()).unwrap();
    let log = SessionLog::continuing(Box::new(store.clone()), "s1", seq);
    (store, log)
}

pub(crate) fn have_bash() -> bool {
    let found = find_program("bash").is_some();
    if !found {
        eprintln!("skipped: bash is not on PATH");
    }
    found
}

pub(crate) fn has_ctty() -> bool {
    if !have_bash() {
        return false;
    }
    let found = HostSpawner::detect().has_ctty();
    if !found {
        eprintln!("skipped: setsid has no --ctty");
    }
    found
}

pub(crate) async fn pid_in(path: &Path) -> i32 {
    let read = || {
        fs::read_to_string(path)
            .ok()
            .filter(|text| text.ends_with('\n'))
            .and_then(|text| text.trim().parse().ok())
    };
    assert!(
        until(|| read().is_some()).await,
        "{} never appeared",
        path.display()
    );
    read().unwrap()
}

pub(crate) fn runner_alive() -> String {
    format!("kill -0 {} 2>/dev/null", std::process::id())
}

pub(crate) fn hold() -> String {
    format!("while {}; do sleep 0.05; done", runner_alive())
}

pub(crate) fn wait_for(file: &str) -> String {
    format!(
        "until [ -e {file} ]; do {} || exit; sleep 0.02; done",
        runner_alive()
    )
}

pub(crate) async fn gone(pid: i32) -> bool {
    until(|| !alive(pid)).await
}

pub(crate) async fn wait_for_members(leader: Pid, at_least: usize) {
    assert!(
        until(|| crate::jobs::exec::session_members(leader).len() >= at_least).await,
        "session {leader:?} never reached {at_least} members"
    );
}

pub(crate) async fn until(mut done: impl FnMut() -> bool) -> bool {
    let start = Instant::now();
    while !done() {
        if start.elapsed() > PATIENCE {
            return false;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    true
}

pub(crate) async fn within<F: Future>(what: &str, future: F) -> F::Output {
    tokio::time::timeout(PATIENCE, future)
        .await
        .unwrap_or_else(|_| panic!("no {what}"))
}

pub(crate) async fn until_event(
    events: &mut mpsc::Receiver<AgentEvent>,
    seen: &mut Vec<AgentEvent>,
    what: &str,
    pick: impl Fn(&AgentEvent) -> bool,
) -> AgentEvent {
    loop {
        let event = tokio::time::timeout(PATIENCE, events.recv())
            .await
            .unwrap_or_else(|_| panic!("no {what}; seen {seen:#?}"))
            .unwrap_or_else(|| panic!("the events ended before {what}; seen {seen:#?}"));
        seen.push(event.clone());
        if pick(&event) {
            return event;
        }
    }
}

pub(crate) async fn until_turn_end(
    events: &mut mpsc::Receiver<AgentEvent>,
    seen: &mut Vec<AgentEvent>,
) -> AgentEvent {
    until_event(events, seen, "the turn's end", is_turn_end).await
}

pub(crate) fn is_turn_end(e: &AgentEvent) -> bool {
    matches!(e, AgentEvent::TurnEnded { .. })
}

pub(crate) async fn loaded_usage(events: &mut mpsc::Receiver<AgentEvent>) -> UsageState {
    match until_event(
        events,
        &mut Vec::new(),
        "the loaded usage",
        |e| matches!(e, AgentEvent::PlanUsageChanged { state, .. } if !state.loading),
    )
    .await
    {
        AgentEvent::PlanUsageChanged { state, .. } => state,
        other => panic!("{other:?}"),
    }
}

impl ScratchDir {
    pub(crate) fn workspace(&self) -> Workspace {
        Workspace {
            root: self.dir("work"),
            home: Some(self.root.clone()),
            temp: self.dir("tmp"),
        }
    }
}

pub(crate) fn refusing_addr() -> (tokio::net::TcpSocket, std::net::SocketAddr) {
    let socket = tokio::net::TcpSocket::new_v4().unwrap();
    socket.bind("127.0.0.1:0".parse().unwrap()).unwrap();
    let addr = socket.local_addr().unwrap();
    (socket, addr)
}

pub(crate) async fn holding_server(
    head: String,
) -> (std::net::SocketAddr, tokio::task::JoinHandle<()>) {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let server = tokio::spawn(async move {
        let (mut socket, _) = listener.accept().await.unwrap();
        let mut request = [0; 8192];
        let _ = socket.read(&mut request).await.unwrap();
        let _ = socket.write_all(head.as_bytes()).await;
        std::future::pending::<()>().await;
    });
    (addr, server)
}

#[test]
fn a_sweep_removes_only_the_scratch_of_dead_test_processes() {
    let mut child = std::process::Command::new("true").spawn().unwrap();
    child.wait().unwrap();
    let dead = std::env::temp_dir().join(format!("wisp-guard-test-{}-gone", child.id()));
    fs::create_dir_all(dead.join("locked")).unwrap();
    fs::set_permissions(dead.join("locked"), fs::Permissions::from_mode(0o500)).unwrap();
    let live = ScratchDir::new("wisp-guard-test-", "mine");
    fs::set_permissions(live.dir("locked"), fs::Permissions::from_mode(0o500)).unwrap();
    let mine = live.root.clone();
    let _second = ScratchDir::new("wisp-guard-test-", "again");
    assert!(!dead.exists(), "{} survived", dead.display());
    assert!(
        mine.exists(),
        "{} was swept while its test runs",
        mine.display()
    );
    drop(live);
    assert!(!mine.exists());
}

#[test]
fn pick_keeps_each_match_by_reference_and_by_value() {
    let sample = vec![Some("a".to_owned()), None, Some("b".to_owned())];
    let borrowed: Vec<&String> = pick!(&sample, Some(text) => text);
    assert_eq!(borrowed, ["a", "b"]);
    let owned: Vec<String> = pick!(sample, Some(text) => text);
    assert_eq!(owned, ["a", "b"]);
}

pub(crate) fn models(config: &str) -> ModelSettings {
    ModelSettings::new(
        ModelTable::shipped().unwrap(),
        ConfigModels::parse(config).unwrap(),
    )
}

pub(crate) fn user(content: Vec<ContentBlock>) -> Message {
    Message {
        role: Role::User,
        content,
    }
}

pub(crate) fn assistant(content: Vec<ContentBlock>) -> Message {
    Message {
        role: Role::Assistant,
        content,
    }
}

pub(crate) fn absent<'a>(
    env: &'a (dyn Fn(&str) -> Option<String> + Sync),
    file: Option<&'a crate::provider::credential::EnvFile>,
    explicit: bool,
) -> crate::endpoints::Keychain<'a> {
    crate::endpoints::Keychain::Absent(crate::endpoints::Variables::new(env, file, explicit))
}

pub(crate) fn kind_of(event: &LogEvent) -> String {
    serde_json::to_value(event).unwrap()["type"]
        .as_str()
        .unwrap()
        .to_owned()
}

pub(crate) fn retries(events: &[AgentEvent]) -> usize {
    events
        .iter()
        .filter(|e| matches!(e, AgentEvent::Retrying { .. }))
        .count()
}

pub(crate) fn thinking(text: &str) -> ContentBlock {
    ContentBlock::Thinking {
        thinking: text.into(),
        signature: format!("sig-{text}"),
    }
}

pub(crate) fn mail(from: &str, to: &str) -> TreeNews {
    TreeNews::Mail(MailNews {
        id: MailId("m1".to_owned()),
        from: path(from),
        to: path(to),
        kind: MailKind::Message,
        wake: true,
        bytes: 3,
    })
}

pub(crate) fn server_error_step() -> Step {
    Step::from(vec![Ok(ProviderEvent::Sending), Err(server_error())])
}

pub(crate) fn state(agent: &str, usd: f64) -> TreeNews {
    TreeNews::State(StateNews {
        agent: path(agent),
        state: AgentState::Running,
        activity: Activity::Streaming,
        spent_usd: usd,
        lower_bound: false,
    })
}

pub(crate) fn config(text: &str) -> ConfigFile {
    ConfigFile::parse(
        Path::new("/home/u/.config/wisp/config.toml"),
        PathShown::Default,
        text,
    )
    .unwrap()
}

pub(crate) fn catalog_names() -> Vec<String> {
    let (registry, _) = crate::plugin::Registry::new(
        wisp_plugins::bundled(),
        &crate::plugin::PluginConfig::default(),
    );
    wisp_protocol::agents::tools_off_names(&registry.catalog(&registry.default_on(), &[]))
}

pub(crate) fn http_400(message: &str) -> String {
    http::json(
        "400 Bad Request",
        &error_json("invalid_request_error", message),
    )
}

pub(crate) fn http_404_image_input() -> String {
    http::json(
        "404 Not Found",
        &json!({"error": {"message": "No endpoints found that support image input", "code": 404}})
            .to_string(),
    )
}

pub(crate) fn http_529() -> String {
    http::json(
        "529 Overloaded",
        &error_json("overloaded_error", "Overloaded"),
    )
}

pub(crate) fn head_and_messages(body: &str) -> (&str, &str) {
    let m = body.find(r#""messages":"#).unwrap();
    (&body[..m], &body[m..body.len() - 1])
}

pub(crate) fn parse(text: &str) -> Result<ConfigFile, String> {
    ConfigFile::parse(Path::new("/h/config.toml"), PathShown::Default, text)
        .map_err(|e| e.to_string())
}

pub(crate) fn error_json(kind: &str, message: &str) -> String {
    json!({"type": "error", "error": {"type": kind, "message": message}}).to_string()
}

pub(crate) fn path(p: &str) -> AgentPath {
    AgentPath(p.to_owned())
}

pub(crate) fn opaque(raw: Value) -> ContentBlock {
    ContentBlock::opaque("anthropic", raw)
}

pub(crate) fn name(text: &str) -> EndpointName {
    EndpointName::parse(text).unwrap()
}

pub(crate) fn text(t: &str) -> ContentBlock {
    ContentBlock::Text { text: t.into() }
}

pub(crate) fn dir() -> tempfile::TempDir {
    tempfile::tempdir().unwrap()
}

pub(crate) fn search_use(id: &str, query: &str) -> Value {
    json!({"type": "server_tool_use", "id": id, "name": "web_search", "input": {"query": query}})
}

pub(crate) fn records(store: &MemoryStore) -> Vec<LogRecord> {
    store.read("s1").unwrap()
}

pub(crate) fn log_events(store: &MemoryStore) -> Vec<LogEvent> {
    records(store).into_iter().map(|r| r.event).collect()
}

pub(crate) fn streamed(events: &[AgentEvent]) -> String {
    pick!(events, AgentEvent::ToolOutput { text, .. } => text.as_str()).concat()
}

pub(crate) fn setting_text(value: &str) -> SettingValue {
    SettingValue::Text(value.into())
}

pub(crate) fn server_error() -> ProviderError {
    classified(
        ErrorKind::Server,
        "server error",
        FailurePhase::Response,
        RetryClass::Never,
    )
}

pub(crate) fn or_stop<T, E>(result: Result<T, E>, what: &str) -> T {
    result.unwrap_or_else(|_| panic!("{what} failed"))
}

pub(crate) fn signed_in() -> crate::endpoints::ResolvedEndpoint {
    use crate::endpoints::{Choice, EndpointBook, Keychain, StartReason, Variables};
    use crate::secrets::Prefetched;
    let name = or_stop(
        std::env::var("WISP_LIVE_ACCOUNT"),
        "reading WISP_LIVE_ACCOUNT",
    );
    let home = std::env::var_os("HOME").map(|home| PathBuf::from(home).join(".config"));
    let folder = std::env::var_os("XDG_CONFIG_HOME")
        .map(PathBuf::from)
        .filter(|p| p.is_absolute())
        .or(home);
    let path = folder.unwrap_or_default().join("wisp/config.toml");
    let text = or_stop(std::fs::read_to_string(&path), "reading Wisp's config");
    let config = or_stop(
        ConfigFile::parse(&path, PathShown::Default, &text),
        "the config",
    );
    let secrets = Prefetched::load(Some(&config));
    let env = |_: &str| -> Option<String> { None };
    let keychain = Keychain::Absent(Variables::new(&env, None, false));
    let choice = Choice {
        name: &name,
        reason: StartReason::Flag,
    };
    let book = EndpointBook::resolve(Some(&config), &secrets, &keychain, Some(choice));
    let book = or_stop(book, "finding the account");
    let endpoint = book.ready().find(|e| e.name.as_str() == name);
    let endpoint = or_stop(endpoint.ok_or(()), "the account's sign-in");
    endpoint.clone()
}
