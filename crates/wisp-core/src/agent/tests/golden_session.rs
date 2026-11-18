use std::collections::VecDeque;
use std::fs;
use std::path::PathBuf;

use jiff::tz::TimeZone;
use tokio::task::JoinHandle;
use wisp_protocol::context::{ReportBasis, ReportDetail};
use wisp_protocol::log::LogRecord;
use wisp_protocol::{ImageData, InputId, MessageTime, RequestId};

use super::*;
use crate::clock::{Clock, ShiftedClock};
use crate::context::ModelSettings;
use crate::provider::anthropic::{AnthropicProvider, Credential};
use crate::session::{MemoryStore, SessionLog, SessionStore, rebuild_checked};
use crate::test_guard::http::event;
use crate::test_guard::{RED_PIXEL as PIXEL, models};
#[cfg(unix)]
use crate::test_guard::{ScratchDir, http, until, until_event};
use crate::workspace::Workspace;

#[cfg(unix)]
mod binding;
mod chat;
mod heads;
mod history;
#[cfg(unix)]
mod hooked;
mod inputs;
#[cfg(unix)]
mod instructions;
#[cfg(unix)]
mod jobs;
mod keep_warm;
#[cfg(unix)]
mod kit;
#[cfg(unix)]
mod paths;
mod search;
#[cfg(unix)]
mod sections;
mod shrink;
mod threads;
#[cfg(unix)]
mod tools;

const FIXTURES: &str = "tests/fixtures/requests/session";
const CASES: &[&str] = &[
    "compact_at_trigger",
    "compact_auto_native",
    "compact_auto_wisp",
    "compact_fallback",
    "compact_head_tools",
    "compact_overflow_cut_off",
    "compact_pointers",
    "compact_pointers_chat",
    "compact_swap_between_turns",
    "compact_swap_in_turn",
    "compact_testkit_strategy",
    "compact_under_trigger",
    "context_after_resume",
    "context_counted",
    "effort_change",
    "environment_removed",
    "found_claude",
    "found_copies",
    "found_openai",
    "found_resumed",
    "found_rewound",
    "found_steered",
    "found_unlisted",
    "hook_compact_early",
    "hook_compact_veto",
    "hook_compact_veto_ceiling",
    "hook_continue",
    "hook_head_addition",
    "hook_input_rewrite",
    "hook_instruction_rewrite_native",
    "hook_instruction_rewrite_wisp",
    "hook_result_rewrite",
    "hook_resume",
    "hook_tool_block",
    "idle_wake",
    "inputs_multi_image",
    "instructions_default",
    "instructions_delta_edit",
    "instructions_delta_head",
    "instructions_delta_resume",
    "instructions_delta_retry",
    "instructions_empty",
    "instructions_extra_alone",
    "instructions_extra_project",
    "instructions_project",
    "instructions_removed",
    "instructions_resume",
    "instructions_root",
    "job_wake",
    "keep_warm_refresh",
    "mcp_found_use",
    "mcp_found_value",
    "process_rewound",
    "process_typed",
    "resume",
    "retry",
    "retry_first_turn_tools",
    "retry_notice",
    "rewind",
    "rewind_after_resume_tools",
    "rewind_to_start_tools",
    "secret_typed",
    "section_changes",
    "section_head",
    "section_retry",
    "section_rewind_resume",
    "skill_found_read",
    "threads",
    "time_local_note",
    "time_local_quiet",
    "time_off_note",
    "time_off_quiet",
    "time_utc_note",
    "time_utc_quiet",
    "tool_on_by_reference",
    "tools_default",
    "web_search_native",
    "web_search_unreadable",
];

fn fixed_clock() -> ShiftedClock {
    ShiftedClock::at(
        "2026-10-25T09:41:27Z".parse().unwrap(),
        Some(TimeZone::posix("IST-5:30").unwrap()),
    )
}

fn step_clock() -> ShiftedClock {
    ShiftedClock::at(
        "2026-10-22T03:58:13Z".parse().unwrap(),
        Some(TimeZone::posix("EST5EDT,M3.2.0,M11.1.0").unwrap()),
    )
}

enum Part {
    Think,
    Text(&'static str),
    #[cfg(unix)]
    Call(&'static str, &'static str, Value),
    Search(&'static str, Value, Value),
    Cited(&'static str, Value),
    Compaction(&'static str),
    Opaque(Value),
}

#[cfg(unix)]
fn call(id: &'static str, name: &'static str, input: Value) -> Part {
    Part::Call(id, name, input)
}

fn block_start(index: usize, block: &Value) -> String {
    event(
        "content_block_start",
        json!({"type": "content_block_start", "index": index, "content_block": block}),
    )
}

fn block_delta(index: usize, delta: &Value) -> String {
    event(
        "content_block_delta",
        json!({"type": "content_block_delta", "index": index, "delta": delta}),
    )
}

fn block_stop(index: usize) -> String {
    event(
        "content_block_stop",
        json!({"type": "content_block_stop", "index": index}),
    )
}

fn part_events(index: &mut usize, part: &Part) -> String {
    let at = *index;
    *index += 1;
    let body = match part {
        Part::Think => [
            block_start(
                at,
                &json!({"type": "thinking", "thinking": "", "signature": ""}),
            ),
            block_delta(
                at,
                &json!({"type": "thinking_delta", "thinking": format!("Thought {at}.")}),
            ),
            block_delta(
                at,
                &json!({"type": "signature_delta", "signature": format!("c2lnbmF0dXJl{at}==")}),
            ),
        ]
        .concat(),
        Part::Text(text) => [
            block_start(at, &json!({"type": "text", "text": ""})),
            block_delta(at, &json!({"type": "text_delta", "text": text})),
        ]
        .concat(),
        #[cfg(unix)]
        Part::Call(id, name, input) => [
            block_start(
                at,
                &json!({"type": "tool_use", "id": id, "name": name, "input": {}}),
            ),
            block_delta(
                at,
                &json!({"type": "input_json_delta", "partial_json": input.to_string()}),
            ),
        ]
        .concat(),
        Part::Search(id, input, results) => {
            let result = *index;
            *index += 1;
            return [
                block_start(
                    at,
                    &json!({"type": "server_tool_use", "id": id, "name": "web_search", "input": {}}),
                ),
                block_delta(
                    at,
                    &json!({"type": "input_json_delta", "partial_json": input.to_string()}),
                ),
                block_stop(at),
                block_start(
                    result,
                    &json!({"type": "web_search_tool_result", "tool_use_id": id, "content": results}),
                ),
                block_stop(result),
            ]
            .concat();
        }
        Part::Cited(text, citation) => [
            block_start(at, &json!({"type": "text", "text": ""})),
            block_delta(
                at,
                &json!({"type": "citations_delta", "citation": citation}),
            ),
            block_delta(at, &json!({"type": "text_delta", "text": text})),
        ]
        .concat(),
        Part::Compaction(summary) => {
            block_start(at, &json!({"type": "compaction", "content": summary}))
        }
        Part::Opaque(block) => block_start(at, block),
    };
    body + &block_stop(at)
}

fn stream(id: &str, input: u64, parts: &[Part], stop: &str) -> String {
    let mut out = event(
        "message_start",
        json!({"type": "message_start", "message": {
            "id": id, "type": "message", "role": "assistant", "model": DEFAULT_MODEL,
            "content": [], "stop_reason": null, "stop_sequence": null,
            "usage": {"input_tokens": input, "cache_creation_input_tokens": 0, "cache_read_input_tokens": 0, "output_tokens": 1}
        }}),
    );
    let mut index = 0;
    for part in parts {
        out.push_str(&part_events(&mut index, part));
    }
    out.push_str(&event(
        "message_delta",
        json!({"type": "message_delta", "delta": {"stop_reason": stop, "stop_sequence": null}, "usage": {"output_tokens": 20}}),
    ));
    out.push_str(&event("message_stop", json!({"type": "message_stop"})));
    http::sse(&out)
}

struct Reply {
    response: String,
    before: Vec<ClientCommand>,
    gate: Option<Arc<tokio::sync::Semaphore>>,
}

fn says(id: &str, input: u64, text: &'static str) -> Reply {
    Reply {
        response: stream(id, input, &[Part::Think, Part::Text(text)], "end_turn"),
        before: Vec::new(),
        gate: None,
    }
}

#[cfg(unix)]
fn calls(id: &str, input: u64, parts: Vec<Part>) -> Reply {
    Reply {
        response: stream(id, input, &parts, "tool_use"),
        before: Vec::new(),
        gate: None,
    }
}

fn reply(id: &str, input: u64, parts: Vec<Part>, stop: &str) -> Reply {
    Reply {
        response: stream(id, input, &parts, stop),
        before: Vec::new(),
        gate: None,
    }
}

const WRITTEN: &str = "## Goal and standing instructions\nLook around.\n\n## Decisions\nNone yet.\n\n## State of the work\nDone: a look around.\n\n## Files, branches and artefacts\nNone.\n\n## Commands, errors and fixes\nNone.\n\n## Open questions\nNone.";
const GREETED: &str = "## Goal and standing instructions\nSay hello.\n\n## Decisions\nNone yet.\n\n## State of the work\nDone: the greeting.\n\n## Files, branches and artefacts\nNone.\n\n## Commands, errors and fixes\nNone.\n\n## Open questions\nNone.";

fn summary(id: &str, input: u64) -> Reply {
    reply(id, input, vec![Part::Text(WRITTEN)], "end_turn")
}

fn greeted(id: &str, input: u64) -> Reply {
    reply(id, input, vec![Part::Text(GREETED)], "end_turn")
}

#[cfg(unix)]
fn assert_extends(case: &str, bodies: &[String]) {
    for (at, pair) in bodies.windows(2).enumerate() {
        assert!(
            binding::extends(&pair[0], &pair[1]),
            "{case}: request {} does not extend request {at}",
            at + 1
        );
    }
}

fn server_error() -> Reply {
    Reply {
        response: http::json(
            "500 Internal Server Error",
            r#"{"type":"error","error":{"type":"api_error","message":"Internal server error"}}"#,
        ),
        before: Vec::new(),
        gate: None,
    }
}

impl Reply {
    fn then(mut self, command: ClientCommand) -> Self {
        self.before.push(command);
        self
    }

    #[cfg(unix)]
    fn gated(mut self, gate: &Arc<tokio::sync::Semaphore>) -> Self {
        self.gate = Some(gate.clone());
        self
    }
}

#[derive(Default)]
struct Seen {
    bodies: Vec<String>,
    counts: Vec<String>,
    heads: Vec<String>,
}

type Answer = Box<dyn FnMut(&str, &str) -> String + Send>;

#[derive(Default)]
struct Shared {
    script: VecDeque<Reply>,
    seen: Seen,
    inbox: Option<mpsc::WeakSender<ClientCommand>>,
    api: Option<Answer>,
}

#[derive(Clone)]
struct Server {
    base: String,
    shared: Arc<Mutex<Shared>>,
}

impl Server {
    async fn start(script: Vec<Reply>) -> Self {
        Self::serving(Shared {
            script: script.into(),
            ..Shared::default()
        })
        .await
    }

    #[cfg(unix)]
    async fn answering(api: Answer) -> Self {
        Self::serving(Shared {
            api: Some(api),
            ..Shared::default()
        })
        .await
    }

    async fn serving(shared: Shared) -> Self {
        let shared = Arc::new(Mutex::new(shared));
        let serving = shared.clone();
        let base = http::gateway(move |request| answer(request, serving.clone())).await + &root();
        Self { base, shared }
    }

    fn provider(&self) -> Arc<dyn Provider> {
        Arc::new(self.anthropic())
    }

    fn anthropic(&self) -> AnthropicProvider {
        AnthropicProvider::new(&Credential::ApiKey("sk-golden".into()), &self.base)
            .unwrap()
            .on_official_origin()
    }

    fn attach(&self, commands: &mpsc::Sender<ClientCommand>) {
        self.shared.lock().unwrap().inbox = Some(commands.downgrade());
    }

    fn unasked(&self) -> usize {
        self.shared.lock().unwrap().script.len()
    }

    #[cfg(unix)]
    async fn wait_bodies(&self, n: usize) {
        assert!(
            until(|| self.shared.lock().unwrap().seen.bodies.len() >= n).await,
            "the server never saw {n} requests"
        );
    }

    fn seen(&self) -> Seen {
        let shared = self.shared.lock().unwrap();
        Seen {
            bodies: shared.seen.bodies.clone(),
            counts: shared.seen.counts.clone(),
            heads: shared.seen.heads.clone(),
        }
    }
}

fn root() -> String {
    format!("/golden-{}", std::process::id())
}

fn count_answer(body: &str) -> String {
    format!("{{\"input_tokens\":{}}}", 100 + body.len() / 4)
}

async fn answer(request: String, shared: Arc<Mutex<Shared>>) -> http::Reply {
    let (head, body) = request.split_once("\r\n\r\n").unwrap();
    let (head, body) = (head.to_owned(), body.to_owned());
    let path = head.split_whitespace().nth(1).unwrap_or_default();
    if !path.starts_with(&root()) {
        return http::Reply::Send(http::response("404 Not Found", &[], ""));
    }
    let response = if path.ends_with("/count_tokens") {
        let response = http::json("200 OK", &count_answer(&body));
        shared.lock().unwrap().seen.counts.push(body);
        response
    } else {
        let misplaced = serde_json::from_str::<Value>(&body)
            .ok()
            .and_then(|body| crate::agent::testing::misplaced_system(&body));
        let (reply, inbox, answered) = {
            let mut shared = shared.lock().unwrap();
            let answered = shared.api.as_mut().map(|api| api(&head, &body));
            shared.seen.bodies.push(body);
            shared.seen.heads.push(head.clone());
            (shared.script.pop_front(), shared.inbox.clone(), answered)
        };
        if let Some(at) = misplaced {
            let error = json!({"type": "invalid_request_error", "message": format!("messages.{at}: role 'system' must precede an 'assistant' message or end the array; the directive-only form (content: [] with output_config) is accepted at any position")});
            let refusal = json!({"type": "error", "error": error}).to_string();
            return http::Reply::Send(http::json("400 Bad Request", &refusal));
        }
        if let Some(answered) = answered {
            return http::Reply::Send(answered);
        }
        match reply {
            Some(reply) => {
                let commands = inbox.and_then(|i| i.upgrade());
                for command in reply.before {
                    if let Some(commands) = &commands {
                        commands.send(command).await.unwrap();
                    }
                }
                if let Some(gate) = reply.gate {
                    gate.acquire().await.unwrap().forget();
                }
                reply.response
            }
            None => http::json(
                "400 Bad Request",
                r#"{"type":"error","error":{"type":"invalid_request_error","message":"the golden test sent more requests than it scripted"}}"#,
            ),
        }
    };
    http::Reply::Send(response)
}

fn bless() -> bool {
    std::env::var_os("WISP_BLESS").is_some()
}

fn fixture_dir(case: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join(FIXTURES)
        .join(case)
}

fn files(seen: &Seen) -> Vec<(String, String)> {
    let mut counts = seen.counts.clone();
    counts.sort();
    seen.bodies
        .iter()
        .enumerate()
        .map(|(n, body)| (format!("{:02}.json", n + 1), body.clone()))
        .chain(
            counts
                .into_iter()
                .enumerate()
                .map(|(n, body)| (format!("count-{:02}.json", n + 1), body)),
        )
        .collect()
}

fn around(text: &str, at: usize) -> String {
    let end = (at + 120).min(text.len());
    let start = at.saturating_sub(120).min(end);
    String::from_utf8_lossy(&text.as_bytes()[start..end]).into_owned()
}

fn first_difference(saved: &str, now: &str) -> String {
    let at = saved
        .bytes()
        .zip(now.bytes())
        .position(|(a, b)| a != b)
        .unwrap_or(saved.len().min(now.len()));
    format!(
        "first difference at byte {at}\n--- saved\n{}\n--- now\n{}",
        around(saved, at),
        around(now, at)
    )
}

fn check(case: &str, seen: &Seen) {
    let dir = fixture_dir(case);
    let now = files(seen);
    for (name, body) in &now {
        assert!(
            serde_json::from_str::<Value>(body).is_ok(),
            "{case}/{name} is not JSON"
        );
        assert!(!body.contains("sk-golden"), "{case}/{name} holds the key");
    }
    if bless() {
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        for (name, body) in &now {
            fs::write(dir.join(name), body).unwrap();
        }
        return;
    }
    let mut saved: Vec<String> = fs::read_dir(&dir)
        .unwrap_or_else(|_| panic!("{case}: no golden files; run with WISP_BLESS=1"))
        .map(|entry| entry.unwrap().file_name().to_string_lossy().into_owned())
        .collect();
    saved.sort();
    let mut names: Vec<String> = now.iter().map(|(name, _)| name.clone()).collect();
    names.sort();
    assert_eq!(
        saved, names,
        "{case}: the requests sent changed in number; run with WISP_BLESS=1 if that is intended"
    );
    for (name, body) in &now {
        let kept = fs::read_to_string(dir.join(name)).unwrap();
        assert!(
            kept == *body,
            "{case}/{name}: the request changed; run with WISP_BLESS=1 if that is intended\n{}",
            first_difference(&kept, body)
        );
    }
}

struct Env {
    root: PathBuf,
    _scratch: ScratchDir,
}

impl Env {
    fn new(case: &str) -> Self {
        let scratch = ScratchDir::new("wisp-golden-session-", case);
        scratch.dir("work");
        scratch.dir("tmp");
        Self {
            root: scratch.root.clone(),
            _scratch: scratch,
        }
    }

    #[cfg(unix)]
    fn work(&self) -> PathBuf {
        self.root.join("work")
    }

    #[cfg(unix)]
    fn workspace(&self) -> Workspace {
        self.workspace_at(self.work())
    }

    #[cfg(unix)]
    fn workspace_at(&self, root: PathBuf) -> Workspace {
        Workspace {
            root,
            home: Some(self.root.clone()),
            temp: self.root.join("tmp"),
        }
    }
}

#[derive(Clone)]
struct Located {
    store: MemoryStore,
    dir: PathBuf,
}

impl SessionStore for Located {
    fn append(&mut self, session: &str, record: &LogRecord) -> std::io::Result<()> {
        self.store.append(session, record)
    }

    fn sync(&mut self, session: &str) -> std::io::Result<()> {
        self.store.sync(session)
    }

    fn list(&self) -> std::io::Result<Vec<String>> {
        self.store.list()
    }

    fn read(&self, session: &str) -> std::io::Result<Vec<LogRecord>> {
        self.store.read(session)
    }

    fn put_blob(
        &mut self,
        sha256: &str,
        media_type: &wisp_protocol::Mime,
        bytes: &[u8],
    ) -> std::io::Result<()> {
        self.store.put_blob(sha256, media_type, bytes)
    }

    fn get_blob(
        &self,
        sha256: &str,
        media_type: &wisp_protocol::Mime,
    ) -> std::io::Result<crate::session::Blob> {
        self.store.get_blob(sha256, media_type)
    }

    fn location(&self, session: &str) -> Option<PathBuf> {
        Some(self.dir.join(format!("{session}.jsonl")))
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Tools {
    Chat,
    #[cfg(unix)]
    Default,
    #[cfg(unix)]
    Jobs,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Clocks {
    Fixed,
    Stepped,
}

#[derive(Clone, Copy)]
struct Setup {
    time: MessageTime,
    note: bool,
    tools: Tools,
    clock: Clocks,
    models: Option<&'static str>,
    step_retries: Option<u32>,
    search: bool,
    located: bool,
    background_tokens: Option<u64>,
    keep_warm: Option<u32>,
    threads: bool,
    hooks: Option<fn() -> (String, crate::plugin::Make)>,
    strategy: Option<&'static str>,
    workspace: bool,
    compaction: bool,
    #[cfg(unix)]
    start: Option<&'static str>,
    #[cfg(unix)]
    instructions: bool,
    #[cfg(unix)]
    removed: bool,
    #[cfg(unix)]
    extra: Option<crate::instructions::Extra>,
}

impl Default for Setup {
    fn default() -> Self {
        Self {
            time: MessageTime::Local,
            note: true,
            tools: Tools::Chat,
            clock: Clocks::Fixed,
            models: None,
            step_retries: None,
            search: false,
            located: false,
            background_tokens: None,
            keep_warm: None,
            threads: false,
            hooks: None,
            strategy: None,
            workspace: true,
            compaction: true,
            #[cfg(unix)]
            start: None,
            #[cfg(unix)]
            instructions: false,
            #[cfg(unix)]
            removed: false,
            #[cfg(unix)]
            extra: None,
        }
    }
}

#[cfg(unix)]
fn registry(tools: Tools) -> Arc<crate::plugin::Registry> {
    use crate::plugin::{PluginConfig, Registry};
    let timing = if matches!(tools, Tools::Jobs) {
        crate::tool::testkit::timing(300, 3_000)
    } else {
        wisp_plugin_shell::Timing::default()
    };
    let sources = crate::plugin::testing::bundled_but_idle()
        .into_iter()
        .map(|b| {
            let make: crate::plugin::Make = if b.manifest == wisp_plugin_shell::MANIFEST {
                crate::tool::testkit::shell_make(timing)
            } else {
                Arc::new(move |host| (b.instantiate)(host))
            };
            (b.manifest.to_owned(), make)
        })
        .collect();
    let (registry, problems) = Registry::from_sources(sources, &PluginConfig::default());
    assert!(problems.is_empty(), "{problems:?}");
    Arc::new(registry)
}

#[cfg(unix)]
fn plugin_tools(tools: Tools) -> crate::plugin::Tools {
    let on = crate::tool::testkit::ALL
        .iter()
        .copied()
        .chain([crate::search::WEB_SEARCH])
        .map(str::to_owned)
        .collect();
    crate::plugin::Tools::new(registry(tools), on)
}

#[cfg(unix)]
fn started(tools: Tools) -> crate::plugin::Started {
    use crate::kit::BuildTools;
    use futures::FutureExt;
    plugin_tools(tools)
        .start(
            crate::kit::ROOT,
            Arc::new(crate::tool::testkit::fake_capture_host()),
        )
        .now_or_never()
        .unwrap()
}

#[cfg(unix)]
fn toolbox(tools: Tools) -> crate::tool::Toolbox {
    started(tools).toolbox
}

fn background_layers(tokens: u64) -> crate::settings::host::StartLayers {
    use wisp_protocol::settings::{Place, SettingValue, SitePath, find};
    let mut layers = crate::settings::host::StartLayers::default();
    let spec = find("context.background_tokens").unwrap();
    layers.saved.insert(
        SitePath::new(Place::Global, spec),
        SettingValue::Tokens(tokens),
        1,
    );
    layers
}

struct Rig {
    case: &'static str,
    server: Server,
    env: Env,
    store: MemoryStore,
    setup: Setup,
    steps: Arc<ShiftedClock>,
    sidebar: wisp_plugin_testkit::hooks::Calls,
}

impl Rig {
    async fn new(case: &'static str, setup: Setup, script: Vec<Reply>) -> Self {
        assert!(CASES.contains(&case), "{case} is not in CASES");
        let rig = Self {
            case,
            server: Server::start(script).await,
            env: Env::new(case),
            store: MemoryStore::default(),
            setup,
            steps: Arc::new(step_clock()),
            sidebar: wisp_plugin_testkit::hooks::Calls::default(),
        };
        #[cfg(unix)]
        fs::create_dir_all(rig.start_dir()).unwrap();
        rig
    }

    #[cfg(unix)]
    fn start_dir(&self) -> PathBuf {
        match self.setup.start {
            Some(sub) => self.env.work().join(sub),
            None => self.env.work(),
        }
    }

    #[cfg(unix)]
    fn instructions(&self) -> bool {
        self.setup.instructions
    }

    #[cfg(not(unix))]
    fn instructions(&self) -> bool {
        false
    }

    #[cfg(unix)]
    fn repository(&self) {
        crate::instructions::make_repository(&self.env.work()).unwrap();
    }

    #[cfg(unix)]
    fn project(&self) {
        self.repository();
        fs::write(
            self.env.work().join("AGENTS.md"),
            "Answer briefly.\nRead @notes.md before you start.\n",
        )
        .unwrap();
        fs::write(self.env.work().join("notes.md"), "Keep notes short.\n").unwrap();
    }

    fn log_store(&self) -> Box<dyn SessionStore> {
        if self.setup.located {
            Box::new(Located {
                store: self.store.clone(),
                dir: self.env.root.join("sessions"),
            })
        } else {
            Box::new(self.store.clone())
        }
    }

    fn agent(&self, log: SessionLog) -> Agent {
        let mut session = default_session();
        session.message_time = self.setup.time;
        session.context_note = self.setup.note;
        let clock: Arc<dyn Clock> = match self.setup.clock {
            Clocks::Fixed => Arc::new(fixed_clock()),
            Clocks::Stepped => self.steps.clone(),
        };
        let policy = crate::agent::retry::RetryPolicy {
            step_retries: self
                .setup
                .step_retries
                .or(crate::agent::retry::quick().step_retries),
            ..crate::agent::retry::quick()
        };
        let provider: Arc<dyn Provider> = match self.setup.threads {
            true => Arc::new(self.server.anthropic().with_threads()),
            false => self.server.provider(),
        };
        let mut agent = Agent::new(provider, session)
            .with_clock(clock)
            .with_retry_policy(policy)
            .with_user_input(true)
            .with_log(log, log_context());
        if self.setup.search {
            agent = agent.with_web_search(true);
        }
        if let Some(config) = self.setup.models {
            agent = agent.with_model_settings(models(config));
        }
        if let Some(tokens) = self.setup.background_tokens {
            agent = agent.with_start_layers(background_layers(tokens));
        }
        if let Some(limit) = self.setup.keep_warm {
            agent = agent.with_keep_warm(limit);
        }
        {
            let mut sources = crate::plugin::testing::turn_line_sources();
            if !self.setup.workspace {
                sources.retain(|(manifest, _)| manifest != wisp_plugin_workspace::MANIFEST);
            }
            if !self.setup.compaction {
                sources.retain(|(manifest, _)| manifest != wisp_plugin_compaction::MANIFEST);
            }
            #[cfg(unix)]
            if self.setup.instructions && !self.setup.removed {
                sources.extend(crate::plugin::testing::instructions_source());
            }
            sources.extend(self.setup.hooks.map(|hooks| hooks()));
            sources.push(sidebar(&self.sidebar));
            let (registry, problems) = crate::plugin::Registry::from_sources(
                sources,
                &crate::plugin::PluginConfig::default(),
            );
            assert!(problems.is_empty(), "{problems:?}");
            let started = futures::FutureExt::now_or_never(registry.start(
                &std::collections::BTreeSet::new(),
                Arc::new(crate::plugin::KernelHost::new()),
            ))
            .unwrap();
            agent = agent.with_hooks(started.hooks, started.host);
        }
        if let Some(id) = self.setup.strategy {
            agent.settings.session.push(crate::settings::Given {
                target: wisp_protocol::settings::Target::Name(
                    wisp_protocol::settings::find("context.strategy").unwrap(),
                ),
                value: wisp_protocol::settings::SettingValue::Text(id.to_owned()),
                origin: "session".to_owned(),
            });
        }
        match self.setup.tools {
            Tools::Chat => agent,
            #[cfg(unix)]
            tools => {
                let agent = agent
                    .with_tools(toolbox(tools), self.env.workspace_at(self.start_dir()))
                    .with_platform("Linux")
                    .with_resources(None);
                if self.setup.instructions {
                    agent
                        .with_instruction_parts(crate::agent::InstructionParts {
                            source: wisp_protocol::InstructionsSource::Auto,
                            warn_tokens: crate::instructions::DEFAULT_WARN_TOKENS,
                            extra: self.setup.extra,
                            preset: None,
                        })
                        .with_config_dir(Some(&self.env.root.join("config")))
                } else {
                    agent
                }
            }
        }
    }

    fn start(&self) -> Live {
        let log = SessionLog::new(self.log_store(), "s1");
        let mut agent = self.agent(log);
        if self.instructions() {
            (agent, _) = futures::FutureExt::now_or_never(agent.with_start_sections()).unwrap();
        }
        Live::start(agent, self)
    }

    fn resume(&self) -> Live {
        let records = records(&self.store);
        let seq = u64::try_from(records.len()).unwrap();
        let log = SessionLog::continuing(self.log_store(), "s1", seq);
        let agent = if self.instructions() {
            futures::FutureExt::now_or_never(self.agent(log).resume_records_rendered(&records))
                .unwrap()
                .unwrap()
        } else {
            self.agent(log).resume(&records, 0).unwrap()
        };
        Live::start(agent, self)
    }

    fn assert_rebuilds(&self, agent: &Agent) {
        let records = records(&self.store);
        let rebuilt = rebuild_checked(&self.store, &records, None)
            .unwrap()
            .unwrap()
            .messages;
        assert_eq!(
            serde_json::to_string(&rebuilt).unwrap(),
            serde_json::to_string(&*agent.history()).unwrap(),
            "{}: the history rebuilt from the log differs from the live one",
            self.case
        );
    }

    fn check(&self) {
        check(self.case, &self.server.seen());
        assert!(
            !self.sidebar.renders().is_empty(),
            "{}: the shown section never rendered",
            self.case
        );
        assert_eq!(
            self.server.unasked(),
            0,
            "{}: scripted replies were never asked for",
            self.case
        );
    }
}

const SIDEBAR: &str = "golden-sidebar/sec";

fn sidebar(calls: &wisp_plugin_testkit::hooks::Calls) -> (String, crate::plugin::Make) {
    use wisp_plugin_testkit::hooks as kit;
    let body = kit::section("sec", &kit::every_point());
    let make: crate::plugin::Make = kit::sections(kit::none(), kit::busy(), calls.clone());
    (kit::slotted("golden-sidebar", &body), make)
}

struct Live {
    commands: Option<mpsc::Sender<ClientCommand>>,
    events: mpsc::Receiver<AgentEvent>,
    seen: Vec<AgentEvent>,
    task: JoinHandle<Agent>,
    steps: Arc<ShiftedClock>,
}

impl Live {
    fn start(agent: Agent, rig: &Rig) -> Self {
        Self::on(agent, &rig.server, rig.steps.clone())
    }

    fn on(mut agent: Agent, server: &Server, steps: Arc<ShiftedClock>) -> Self {
        let (commands, mut inbox) = mpsc::channel(64);
        let (sink, events) = mpsc::channel(8_192);
        let shown = vec![SIDEBAR.to_owned()];
        commands
            .try_send(ClientCommand::SlotsShown {
                sections: shown.clone(),
                pages: shown,
            })
            .unwrap();
        server.attach(&commands);
        let task = tokio::spawn(async move {
            agent.serve(&mut inbox, &sink).await;
            agent
        });
        Self {
            commands: Some(commands),
            events,
            seen: Vec::new(),
            task,
            steps,
        }
    }

    async fn send(&self, command: ClientCommand) {
        if matches!(
            command,
            ClientCommand::Send { .. } | ClientCommand::Retry { .. }
        ) {
            self.steps.advance(jiff::SignedDuration::from_mins(1));
        }
        if let Some(commands) = &self.commands {
            commands.send(command).await.unwrap();
        }
    }

    async fn until(&mut self, pick: impl Fn(&AgentEvent) -> bool) -> AgentEvent {
        until_event(&mut self.events, &mut self.seen, "the event", pick).await
    }

    async fn counted_report(&mut self, n: u64) {
        self.send(ClientCommand::ContextReport {
            detail: ReportDetail::All,
            request: request(n),
        })
        .await;
        let settled = self
            .until(|e| {
                matches!(e, AgentEvent::ContextReport(r)
                    if r.answers == request(n) && r.basis != ReportBasis::Counting)
            })
            .await;
        let AgentEvent::ContextReport(report) = settled else {
            unreachable!()
        };
        assert_eq!(report.basis, ReportBasis::Counted, "{report:?}");
    }

    async fn compacted(&mut self, n: u64) {
        self.send(ClientCommand::Compact {
            focus: None,
            request: request(n),
        })
        .await;
        let done = self
            .until(|e| match e {
                AgentEvent::Compacted { answers, .. }
                | AgentEvent::CompactionFailed { answers, .. } => *answers == Some(request(n)),
                AgentEvent::Rejected { request: r, .. } => *r == Some(request(n)),
                _ => false,
            })
            .await;
        assert!(matches!(done, AgentEvent::Compacted { .. }), "{done:?}");
    }

    async fn ended(&mut self) -> TurnOutcome {
        match self
            .until(|e| matches!(e, AgentEvent::TurnEnded { .. }))
            .await
        {
            AgentEvent::TurnEnded { outcome, .. } => outcome,
            _ => unreachable!(),
        }
    }

    async fn turn(&mut self, id: &str, text: &str) -> TurnOutcome {
        self.send(ClientCommand::send(id, text)).await;
        self.ended().await
    }

    async fn completed(&mut self, id: &str, text: &str) {
        let outcome = self.turn(id, text).await;
        assert!(
            matches!(outcome, TurnOutcome::Completed { .. }),
            "{outcome:?}\n{:#?}",
            self.seen
        );
    }

    async fn close(mut self) -> (Agent, Vec<AgentEvent>) {
        drop(self.commands.take());
        let agent = within("the agent's end", self.task).await.unwrap();
        while let Ok(event) = self.events.try_recv() {
            self.seen.push(event);
        }
        (agent, self.seen)
    }
}

fn image() -> ImageData {
    ImageData { data: PIXEL.into() }
}

fn with_images(id: &str, text: &str, images: Vec<ImageData>) -> ClientCommand {
    ClientCommand::Send {
        id: InputId(id.into()),
        text: text.into(),
        images,
        mode: wisp_protocol::DeliveryMode::Steer,
        turn: None,
        replaces: None,
    }
}

#[cfg(unix)]
fn answer_first(call_id: &str) -> ClientCommand {
    ClientCommand::Answer {
        call_id: call_id.into(),
        answers: vec![wisp_protocol::Answer::choose(0)],
    }
}

fn request(n: u64) -> RequestId {
    RequestId(n)
}

#[test]
fn the_fixed_clock_sits_on_a_time_with_seconds() {
    let now = fixed_clock().now();
    let offset = fixed_clock().local_zone().unwrap().to_offset(now).seconds();
    let at = wisp_plugin_api::host::Instant {
        seconds: now.as_second(),
        nanos: u32::try_from(now.subsec_nanosecond()).unwrap(),
    };
    assert_eq!(
        wisp_plugin_time::stamp(at, offset).unwrap(),
        "Sent: Sunday 2026-10-25 15:11:27 UTC+05:30\n"
    );
}

#[test]
fn the_fixture_folders_are_exactly_the_cases() {
    let mut folders: Vec<String> =
        fs::read_dir(PathBuf::from(env!("CARGO_MANIFEST_DIR")).join(FIXTURES))
            .unwrap()
            .map(|entry| entry.unwrap().file_name().to_string_lossy().into_owned())
            .collect();
    folders.sort();
    if bless() {
        let unknown: Vec<&String> = folders
            .iter()
            .filter(|f| !CASES.contains(&f.as_str()))
            .collect();
        assert!(unknown.is_empty(), "folders of no case: {unknown:?}");
    } else {
        assert_eq!(folders, CASES);
    }
}

#[cfg(unix)]
fn too_long() -> Reply {
    Reply {
        response: http::json(
            "400 Bad Request",
            r#"{"type":"error","error":{"type":"invalid_request_error","message":"prompt is too long"}}"#,
        ),
        before: Vec::new(),
        gate: None,
    }
}

fn reply_error(status: &str, kind: &str, message: &str) -> Reply {
    Reply {
        response: http::json(
            status,
            &format!(r#"{{"type":"error","error":{{"type":"{kind}","message":"{message}"}}}}"#),
        ),
        before: Vec::new(),
        gate: None,
    }
}
