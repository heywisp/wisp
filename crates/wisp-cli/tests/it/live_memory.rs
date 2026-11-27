#![cfg(any(target_os = "linux", target_os = "macos"))]
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    reason = "test fixture and assertions"
)]

use std::fmt::Display;
use std::fs;
use std::os::unix::fs::DirBuilderExt;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use futures::{StreamExt, stream, stream::BoxStream};
use jiff::civil::Date;
use serde_json::{Value, json};
use tokio::sync::mpsc;
use wisp_core::agent::{AgentTask, LogContext};
use wisp_core::clock::SystemClock;
use wisp_core::context::ModelSettings;
use wisp_core::endpoints::ProviderKind;
use wisp_core::kit::Choices;
use wisp_core::models::{ConfigModels, ModelTable};
use wisp_core::plugin::{PluginConfig, Registry, Tools};
use wisp_core::pricing::{estimate_usd, model_info};
use wisp_core::provider::anthropic::{AnthropicProvider, Credential};
use wisp_core::provider::{
    ModelCapabilities, PrefixBytes, Provider, ProviderError, ProviderEvent, Request, ServerCalls,
    WirePrint, shipped_capabilities,
};
use wisp_core::session::Redactions;
use wisp_core::test_support::rooted::{Inputs, Logs, Rooted, single_provider, spec, tree};
use wisp_core::workspace::Workspace;
use wisp_protocol::log::CredentialSource;
use wisp_protocol::{
    AgentEvent, Billing, CacheTtl, ClientCommand, CommandOutcome, ContentBlock, Effort, ErrorKind,
    RequestId, Secret, TurnOutcome,
};

use crate::live::{
    affords, live_session, next_event, next_id, provider, scrubbed, secrets, worst_cached_step_usd,
};
use crate::scratch::ScratchDir;
use crate::support::server::{self, Server, text_reply, tool_reply};

const MODEL: &str = "claude-haiku-5-5";
const MAX_TOKENS: u32 = 1024;
const MAX_WIRE_BYTES: usize = 24 * 1024;
const PROMPT_TOKEN_ALLOWANCE: u64 = 2 * MAX_WIRE_BYTES as u64;
const CAP_USD: f64 = 0.15;
const COMPACTIONS: usize = 3;
const SAVE_STEPS: usize = 2;
const REMARK_STEPS: usize = 2;
const MAX_REQUESTS: usize = 3 * COMPACTIONS + 1 + SAVE_STEPS + REMARK_STEPS;
const WORD: &str = "plumferrindle";
const SEED_NAME: &str = "marker-word";
const SAVE_NAME: &str = "fixture-rosette";
const SAVE_FACT: &str = "The imaginary rosette is lavender.";

fn worst_step() -> f64 {
    worst_cached_step_usd(MODEL, 0, PROMPT_TOKEN_ALLOWANCE, 0, MAX_TOKENS)
}

fn calculate_budget() -> f64 {
    MAX_REQUESTS as f64 * worst_step()
}

#[derive(Default)]
struct Capture {
    requests: Vec<Value>,
    attempts: usize,
    allowed: usize,
    spent: f64,
    summaries: Vec<String>,
}

#[derive(Default)]
struct Recording {
    api: Option<AnthropicProvider>,
    capture: Mutex<Capture>,
}

impl Provider for Recording {
    fn kind(&self) -> ProviderKind {
        ProviderKind::Anthropic
    }

    fn capabilities(&self, model: &str) -> Option<ModelCapabilities> {
        match &self.api {
            Some(api) => api.capabilities(model),
            None => shipped_capabilities(model).map(|(_, caps)| caps),
        }
    }

    fn features_documented(&self) -> bool {
        self.api.as_ref().is_some_and(Provider::features_documented)
    }

    fn echoed<'a>(
        &self,
        content: &'a [ContentBlock],
        earlier: &ServerCalls,
    ) -> Vec<&'a ContentBlock> {
        match &self.api {
            Some(api) => api.echoed(content, earlier),
            None => content.iter().collect(),
        }
    }

    fn betas(&self, request: &Request<'_>) -> Vec<String> {
        self.api
            .as_ref()
            .map_or_else(Vec::new, |api| api.betas(request))
    }

    fn keys_cache(&self, beta: &str) -> bool {
        self.api.as_ref().is_none_or(|api| api.keys_cache(beta))
    }

    fn account_bound_thinking(&self, model: &str) -> bool {
        self.api
            .as_ref()
            .is_some_and(|api| api.account_bound_thinking(model))
    }

    fn busy_waits_5xx(&self) -> bool {
        self.api.as_ref().is_none_or(Provider::busy_waits_5xx)
    }

    fn cache_prefix(&self, request: &Request<'_>) -> Option<String> {
        self.api.as_ref()?.cache_prefix(request)
    }

    fn prefix_bytes(&self, request: &Request<'_>) -> Option<PrefixBytes> {
        self.api.as_ref()?.prefix_bytes(request)
    }

    fn wire_print(&self, request: &Request<'_>) -> Option<WirePrint> {
        self.api.as_ref()?.wire_print(request)
    }

    fn reply_print(&self, content: &[ContentBlock]) -> Option<String> {
        self.api.as_ref()?.reply_print(content)
    }

    fn stream(
        &self,
        request: &Request<'_>,
    ) -> BoxStream<'static, Result<ProviderEvent, ProviderError>> {
        let mut capture = self.capture.lock().unwrap();
        capture.attempts += 1;
        let stopped = || {
            ProviderError::new(
                ErrorKind::InvalidRequest,
                "the live memory request guard stopped this call",
            )
        };
        let result = (|| {
            let api = self.api.as_ref().ok_or_else(stopped)?;
            if capture.attempts > capture.allowed
                || capture.attempts > MAX_REQUESTS
                || request.session.model != MODEL
                || request.session.max_tokens > MAX_TOKENS
                || !request.server_tools.is_empty()
                || !affords(capture.spent, worst_step(), CAP_USD)
            {
                return Err(stopped());
            }
            let body = api.request_body(request)?;
            if body.len() > MAX_WIRE_BYTES {
                return Err(stopped());
            }
            let captured = serde_json::from_slice(&body).map_err(|_| {
                ProviderError::new(
                    ErrorKind::InvalidRequest,
                    "the live memory capture was not JSON",
                )
            })?;
            capture.requests.push(captured);
            Ok(api)
        })();
        drop(capture);
        match result {
            Ok(api) => api.stream(request),
            Err(error) => stream::once(async { Err(error) }).boxed(),
        }
    }
}

struct Live {
    commands: mpsc::Sender<ClientCommand>,
    events: mpsc::Receiver<AgentEvent>,
    task: AgentTask,
    recording: Arc<Recording>,
}

async fn preflight_seed_and_show(
    scratch: &ScratchDir,
    recording: Arc<Recording>,
    redactions: Redactions,
) -> Live {
    let data = scratch.root.join("plugin-data");
    fs::DirBuilder::new()
        .recursive(true)
        .mode(0o700)
        .create(data.join("memory/user"))
        .unwrap();
    scratch.write(
        &format!("plugin-data/memory/user/{SEED_NAME}.md"),
        &note_text(
            SEED_NAME,
            &format!("The invented marker word is {WORD}."),
            "2000-01-02",
        ),
    );
    let (registry, problems) = Registry::new(wisp_plugins::bundled(), &PluginConfig::default());
    assert!(problems.is_empty(), "{}", scrubbed(&problems));
    let context = LogContext {
        endpoint: "https://api.anthropic.com".into(),
        endpoint_name: "Anthropic".into(),
        credential_source: CredentialSource::Environment,
        redactions,
    };
    let session = live_session(MODEL, MAX_TOKENS);
    assert_eq!(
        (session.effort, session.cache_ttl),
        (Effort::Low, CacheTtl::FiveMinutes)
    );
    let config = format!(
        "[models.\"{MODEL}\"]\nmax_output = {}\nnative_compaction = false\n",
        session.max_tokens
    );
    let mut kit = Inputs {
        connect: single_provider(recording.clone(), context.clone()),
        tools: Arc::new(Tools::new(Arc::new(registry), ["remember".into()].into())),
        logs: Arc::new(Logs {
            id: "live-memory",
            store: Default::default(),
        }),
        log_context: context,
        workspace: Workspace {
            root: scratch.root.clone(),
            home: Some(scratch.dir("home")),
            temp: scratch.dir("tmp"),
        },
        plugin_data: Some(data),
        clock: Arc::new(SystemClock),
        models: ModelSettings::new(
            ModelTable::shipped().unwrap(),
            ConfigModels::parse(&config).unwrap(),
        ),
        start_layers: Default::default(),
        auto_compact: false,
        user_input: true,
    }
    .kit();
    kit.options.message_threads = false;
    kit.options.refusal_fallback = false;
    let rooted = Rooted::build(
        kit,
        spec(Choices {
            model: session.model,
            effort: session.effort,
            cache_ttl: session.cache_ttl,
            message_time: session.message_time,
            context_note: false,
        }),
        tree(),
    )
    .await
    .unwrap_or_else(|error| panic!("{}", scrubbed(&error)));
    let (commands, mut events, task) = rooted.start();
    commands
        .send(ClientCommand::RunPluginCommand {
            name: "memory:memory".into(),
            args: Secret::new(format!("show you {SEED_NAME}")),
            request: RequestId(1),
        })
        .await
        .unwrap();
    loop {
        if let AgentEvent::CommandDone {
            answers: RequestId(1),
            outcome,
            ..
        } = next_event(&mut events).await
        {
            assert!(
                matches!(&outcome, CommandOutcome::Lines { .. })
                    && serde_json::to_string(&outcome).unwrap().contains(WORD),
                "{}",
                scrubbed(&outcome)
            );
            assert_eq!(
                recording.capture.lock().unwrap().attempts,
                0,
                "the local preflight reached the provider"
            );
            return Live {
                commands,
                events,
                task,
                recording,
            };
        }
    }
}

#[derive(Debug, PartialEq, Eq)]
enum Recall {
    Recalled,
    Missed,
    Inconclusive,
}

fn contains_word(text: &str) -> bool {
    text.to_ascii_lowercase().contains(WORD)
}

fn memory_frame(block: &Value) -> Option<&str> {
    let text = block["text"].as_str()?.trim();
    (block["type"] == "text").then_some(())?;
    ["memory", "memory-update"].into_iter().find_map(|tag| {
        let rest = text.strip_prefix(&format!("<{tag}>"))?;
        let (frame, after) = rest.split_once(&format!("</{tag}>"))?;
        after.is_empty().then_some(frame)
    })
}

fn verify_recall(capture: &Capture, reply: &str) -> Recall {
    if capture
        .summaries
        .iter()
        .any(|summary| contains_word(summary))
    {
        return Recall::Inconclusive;
    }
    let mut carried_marker = false;
    for (i, request) in capture.requests.iter().enumerate() {
        let mut other = request.clone();
        for message in other["messages"].as_array_mut().into_iter().flatten() {
            if message["role"] == "user"
                && let Some(blocks) = message["content"].as_array_mut()
            {
                blocks.retain(|block| {
                    let Some(frame) = memory_frame(block) else {
                        return true;
                    };
                    carried_marker |= i + 1 == capture.requests.len() && contains_word(frame);
                    false
                });
            }
        }
        if contains_word(&other.to_string()) {
            return Recall::Inconclusive;
        }
    }
    if carried_marker && reply.trim().eq_ignore_ascii_case(WORD) {
        Recall::Recalled
    } else {
        Recall::Missed
    }
}

fn note_text(name: &str, fact: &str, day: impl Display) -> String {
    let description = Value::String(fact.to_owned());
    format!("---\nname: {name}\ndescription: {description}\nsaved: {day}\n---\n")
}

fn verify_durable_file(dir: &Path) -> bool {
    let Ok(note) = fs::read_to_string(dir.join(format!("{SAVE_NAME}.md"))) else {
        return false;
    };
    let Some(day) = note
        .lines()
        .nth(3)
        .and_then(|line| line.strip_prefix("saved: "))
        .and_then(|day| day.parse::<Date>().ok())
    else {
        return false;
    };
    day.year() > 0 && note == note_text(SAVE_NAME, SAVE_FACT, day)
}

fn note_files(root: &Path) -> Vec<(PathBuf, Vec<u8>)> {
    let mut files = Vec::new();
    let mut dirs = vec![root.to_owned()];
    while let Some(dir) = dirs.pop() {
        for entry in fs::read_dir(dir).unwrap() {
            let entry = entry.unwrap();
            let kind = entry.file_type().unwrap();
            let path = entry.path();
            if kind.is_dir() {
                dirs.push(path);
            } else if kind.is_file() && path.extension().is_some_and(|ext| ext == "md") {
                files.push((
                    path.strip_prefix(root).unwrap().to_owned(),
                    fs::read(path).unwrap(),
                ));
            }
        }
    }
    files.sort_unstable();
    files
}

#[derive(Default)]
struct Turn {
    reply: String,
    remember_calls: usize,
}

impl Live {
    async fn run(&mut self, command: ClientCommand, steps: usize) -> Turn {
        let start = {
            let mut capture = self.recording.capture.lock().unwrap();
            let start = capture.attempts;
            capture.allowed = start + steps;
            assert!(
                capture.allowed <= MAX_REQUESTS && affords(capture.spent, worst_step(), CAP_USD)
            );
            start
        };
        self.commands.send(command).await.unwrap();
        let mut turn = Turn::default();
        let mut reported_usage = 0;
        loop {
            let event = next_event(&mut self.events).await;
            let (usage, done) = match event {
                AgentEvent::TextDelta { text, .. } => {
                    turn.reply.push_str(&text);
                    continue;
                }
                AgentEvent::ToolCallRequested { tool, .. } if tool == "remember" => {
                    turn.remember_calls += 1;
                    continue;
                }
                AgentEvent::StepEnded { usage, .. } => (usage, false),
                AgentEvent::Compacted { usage, summary, .. } => {
                    assert!(
                        usage.is_some(),
                        "the provider did not report compaction usage"
                    );
                    self.recording
                        .capture
                        .lock()
                        .unwrap()
                        .summaries
                        .extend(summary);
                    (usage, true)
                }
                AgentEvent::TurnEnded {
                    outcome: TurnOutcome::Completed { .. },
                    ..
                } => break,
                AgentEvent::TurnEnded { .. }
                | AgentEvent::CompactionFailed { .. }
                | AgentEvent::Rejected { .. } => panic!("{}", scrubbed(&event)),
                _ => continue,
            };
            if let Some(usage) = usage {
                reported_usage += 1;
                self.recording.capture.lock().unwrap().spent +=
                    estimate_usd(model_info(MODEL).unwrap(), &usage, CacheTtl::FiveMinutes);
            }
            if done {
                break;
            }
        }
        let capture = self.recording.capture.lock().unwrap();
        let attempts = capture.attempts - start;
        assert!(
            (1..=steps).contains(&attempts),
            "unexpected number of provider steps"
        );
        assert_eq!(
            reported_usage, attempts,
            "the provider did not report usage for every completed request"
        );
        assert_eq!(
            capture.requests.len(),
            capture.attempts,
            "a provider step was refused locally"
        );
        assert!(capture.spent <= CAP_USD, "the run passed its spend limit");
        turn
    }

    async fn say(&mut self, text: impl Into<String>, steps: usize) -> Turn {
        self.run(ClientCommand::send(next_id(), text), steps).await
    }

    async fn close(self) {
        let Live {
            commands,
            mut events,
            task,
            ..
        } = self;
        drop(commands);
        let (_, joined) = tokio::join!(
            async { while events.recv().await.is_some() {} },
            task.join()
        );
        assert!(joined);
    }
}

#[tokio::test(flavor = "multi_thread")]
#[ignore = "live: costs real money; run by hand with credentials in the environment"]
async fn live_memory_recall_after_three_compactions() {
    assert!(affords(0.0, calculate_budget(), CAP_USD));
    let api = provider();
    assert!(api.official() && api.billing() == Billing::Metered);
    assert!(
        api.capabilities(MODEL)
            .is_some_and(|caps| caps.takes_effort(Effort::Low))
    );
    let recording = Arc::new(Recording {
        api: Some(api),
        ..Default::default()
    });
    let scratch = ScratchDir::new("wisp-live-memory-", "recall");
    let notes = scratch.root.join("plugin-data/memory");
    let mut live = preflight_seed_and_show(&scratch, recording.clone(), secrets()).await;
    let prompts: [[&str; 2]; COMPACTIONS] = [
        [
            "Name a polygon with three sides.",
            "Name one kind of cloud.",
        ],
        [
            "Name a planet with rings.",
            "Name a shape with one curved edge.",
        ],
        ["Name a tree with needles.", "Name a flightless bird."],
    ];
    for (i, pair) in prompts.into_iter().enumerate() {
        for prompt in pair {
            live.say(prompt, 1).await;
        }
        live.run(
            ClientCommand::Compact {
                focus: None,
                request: RequestId(i as u64 + 2),
            },
            1,
        )
        .await;
    }
    let recall_turn = live
        .say(
            "What is the invented marker word? Reply with just the word.",
            1,
        )
        .await;
    let recall = verify_recall(&recording.capture.lock().unwrap(), &recall_turn.reply);
    let save = live.say(
        format!("Use remember to save this exact fact for you under {SAVE_NAME}, with no body: {SAVE_FACT}"),
        SAVE_STEPS,
    ).await;
    let saved = save.remember_calls == 1 && verify_durable_file(&notes.join("user"));
    let before = note_files(&notes);
    let remark = live
        .say(
            "Incidentally, the imaginary bunting is silver.",
            REMARK_STEPS,
        )
        .await;
    let unchanged = before == note_files(&notes) && remark.remember_calls == 0;
    let score = match recall {
        Recall::Recalled => "1/1",
        Recall::Missed => "0/1",
        Recall::Inconclusive => "0/0 (inconclusive: answer leaked; discarded)",
    };
    let (spent, steps) = {
        let capture = recording.capture.lock().unwrap();
        (capture.spent, capture.requests.len())
    };
    eprintln!(
        "live: Anthropic {MODEL}; ${spent:.4}; {steps} steps; recall {score}; explicit save {saved}; unsolicited remark unchanged {unchanged}"
    );
    live.close().await;
    assert!(saved && unchanged);
}

#[test]
fn budget_covers_every_request_and_stops_at_the_cap() {
    let budget = calculate_budget();
    assert!(affords(0.0, budget, CAP_USD));
    assert!(!affords(budget, worst_step(), budget));
    assert!(!affords(CAP_USD, worst_step(), CAP_USD));
}

#[tokio::test]
async fn preflight_is_local_with_the_bound_root() {
    let scratch = ScratchDir::new("wisp-live-memory-", "preflight");
    preflight_seed_and_show(
        &scratch,
        Arc::new(Recording::default()),
        Redactions::default(),
    )
    .await
    .close()
    .await;
}

#[test]
fn recall_scores_only_when_the_answer_is_exclusive_to_memory() {
    let mut capture = Capture::default();
    for tag in ["memory", "memory-update"] {
        capture.requests.push(json!({"messages":[{"role":"user","content":[{"type":"text","text":format!("<{tag}>\n{WORD}\n</{tag}>")}]}]}));
    }
    assert_eq!(
        verify_recall(&capture, &format!(" {}\n", WORD.to_ascii_uppercase())),
        Recall::Recalled
    );
    assert_eq!(verify_recall(&capture, "not sure"), Recall::Missed);
    assert_eq!(verify_recall(&capture, &format!("{WORD}s")), Recall::Missed);
    capture.summaries.push(format!(
        "A summary mentioned {}.",
        WORD.to_ascii_uppercase()
    ));
    assert_eq!(verify_recall(&capture, WORD), Recall::Inconclusive);
    capture.summaries.clear();
    for other in [
        json!({"messages":[{"role":"assistant","content":[{"type":"compaction","content":WORD.to_ascii_uppercase()}]}]}),
        json!({"system":[{"type":"text","text":WORD}]}),
    ] {
        capture.requests.push(other);
        assert_eq!(verify_recall(&capture, WORD), Recall::Inconclusive);
        capture.requests.pop();
    }
    capture.requests.push(json!({"messages":[{"role":"user","content":[{"type":"text","text":"What is the marker?"}]}]}));
    assert_eq!(verify_recall(&capture, WORD), Recall::Missed);
}

fn loopback_recording(server: &Server) -> Arc<Recording> {
    let api = AnthropicProvider::new(&Credential::ApiKey("memory-CANARY".into()), &server.url())
        .unwrap()
        .on_official_origin();
    Arc::new(Recording {
        api: Some(api),
        ..Default::default()
    })
}

#[tokio::test]
#[should_panic(expected = "TurnEnded { outcome: Error { kind: Auth")]
async fn a_step_without_usage_does_not_mask_the_auth_error() {
    let server = Server::replying(server::json(
        "401 Unauthorized",
        r#"{"type":"error","error":{"type":"authentication_error","message":"Fixture credentials refused."}}"#,
    ));
    let scratch = ScratchDir::new("wisp-live-memory-", "auth-error");
    let mut live =
        preflight_seed_and_show(&scratch, loopback_recording(&server), Redactions::default()).await;
    live.say("Name a kind of cloud.", 1).await;
}

#[tokio::test]
async fn durable_save_and_file_inventory_detect_writes() {
    const SAVE_CALL: &str = "toolu_fixture_save";
    let server = Server::start(|request| {
        if request.body.contains(SAVE_CALL) {
            text_reply("msg_saved", MODEL, "Saved.", (1, 5))
        } else {
            tool_reply(
                SAVE_CALL,
                "remember",
                &json!({"action":"save","scope":"you","name":SAVE_NAME,"fact":SAVE_FACT}),
            )
        }
    });
    let recording = loopback_recording(&server);
    let scratch = ScratchDir::new("wisp-live-memory-", "files");
    let mut live =
        preflight_seed_and_show(&scratch, recording.clone(), Redactions::default()).await;
    assert_eq!(server.requests_received(), 0);
    let notes = scratch.root.join("plugin-data/memory");
    let user = notes.join("user");
    let before = note_files(&notes);
    assert!(!verify_durable_file(&user));
    let save = live.say("Save the fixture note.", SAVE_STEPS).await;
    assert_eq!(save.remember_calls, 1);
    assert!(verify_durable_file(&user));
    let saved = note_files(&notes);
    assert_ne!(before, saved);
    let target = user.join(format!("{SAVE_NAME}.md"));
    let raw = fs::read_to_string(&target).unwrap();
    fs::write(&target, format!("{raw}Unexpected body.\n")).unwrap();
    assert!(!verify_durable_file(&user));
    assert_ne!(
        saved,
        note_files(&notes),
        "overwriting an existing note must change its snapshot"
    );
    fs::write(&target, note_text(SAVE_NAME, SAVE_FACT, "2000-02-30")).unwrap();
    assert!(!verify_durable_file(&user));
    let wire: Vec<Value> = server
        .bodies()
        .iter()
        .map(|body| serde_json::from_str(body).unwrap())
        .collect();
    assert!(wire[0].get("cache_control").is_some());
    assert_eq!(recording.capture.lock().unwrap().requests, wire);
    live.close().await;
}
