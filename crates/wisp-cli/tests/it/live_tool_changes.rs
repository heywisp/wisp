#![cfg(unix)]
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    reason = "a live check fails the test on any setup or network error"
)]

use crate::live;

use std::collections::BTreeSet;
use std::sync::Arc;

use crate::scratch::ScratchDir;
use futures::StreamExt;
use live::{TTL, live_session, next_event, provider, worst_usd};
use tokio::sync::mpsc;
use wisp_core::agent::{self, Agent, LogContext};
use wisp_core::plugin::{KernelHost, PluginConfig, Registry};
use wisp_core::provider::anthropic::{AnthropicProvider, INLINE_TOOLS_BETA, sent_prefix};
use wisp_core::provider::{
    CompactionAsk, FeatureUse, Features, Provider, ProviderEvent, Request, RequestOptions,
    TailMarker, UNRENDERED,
};
use wisp_core::session::{MemoryStore, Redactions, SessionLog, SessionStore};
use wisp_core::settings::Given;
use wisp_core::settings::host::StartLayers;
use wisp_core::tool::ToolSpec;
use wisp_core::workspace::Workspace;
use wisp_protocol::log::{CredentialSource, LogEvent};
use wisp_protocol::settings::{SettingChange, SettingValue, Target, find};
use wisp_protocol::{
    AgentEvent, ClientCommand, ContentBlock, Message, RequestId, ToolDefinition, ToolRef,
    TurnOutcome, Usage,
};

const HAIKU: &str = "claude-haiku-5-5";
const SONNET: &str = "claude-sonnet-5-5";
const CAP_USD: f64 = 0.10;
const DEFERRED_CAP_USD: f64 = 0.05;
const MAX_TOKENS: u32 = 64;
const PADDING_WORDS: usize = 4_500;
const SESSION_TOOLS: [&str; 2] = ["view_file", "write_todos"];

fn padding() -> String {
    padding_of([
        "amber", "birch", "cedar", "delta", "ember", "fjord", "grove", "heath",
    ])
}

fn padding_of(words: [&str; 8]) -> String {
    let text: Vec<&str> = (0..PADDING_WORDS).map(|n| words[n % words.len()]).collect();
    format!(
        "Below is filler for a cache test; ignore it and answer each later message in one short sentence.\n\n{}",
        text.join(" ")
    )
}

fn session(model: &str) -> wisp_protocol::SessionConfig {
    let mut session = live_session(model, MAX_TOKENS);
    session.system = Some("You are a terse assistant in a cache test.".into());
    session
}

fn spec(name: &str, description: &str) -> ToolSpec {
    ToolSpec {
        description: description.to_owned(),
        input_schema: serde_json::json!({"type": "object", "properties": {"path": {"type": "string"}}}),
        name: name.to_owned(),
    }
}

#[derive(Debug, Default, Clone, Copy)]
struct Measured {
    uncached: u64,
    read: u64,
    cached: u64,
    prompt: u64,
    usd: f64,
}

async fn measure(provider: &AnthropicProvider, request: &Request<'_>) -> Measured {
    try_measure(provider, request)
        .await
        .unwrap_or_else(|error| {
            eprintln!(
                "live provider error type: {}; message: {}",
                live::scrubbed(&error.detail.error_type.as_deref().unwrap_or("unknown")),
                live::scrubbed(&error.message)
            );
            panic!(
                "live request refused: {:?}, HTTP {:?}, possibly billed {}",
                error.kind, error.detail.status, error.possibly_billed
            )
        })
}

async fn try_measure(
    provider: &dyn Provider,
    request: &Request<'_>,
) -> Result<Measured, wisp_core::provider::ProviderError> {
    let mut stream = provider.stream(request);
    let mut assembled = wisp_core::provider::Assembler::default();
    while let Some(event) = stream.next().await {
        assembled.apply(event?)?;
    }
    let usage = assembled.finish()?.usage;
    if usage.prompt_tokens() == 0 {
        return Err(wisp_core::provider::ProviderError::malformed(
            "the live response reported no prompt usage",
        ));
    }
    let info = wisp_core::pricing::model_info(&request.session.model).unwrap();
    Ok(Measured {
        uncached: usage.input_tokens,
        read: usage.cache_read_input_tokens,
        cached: usage.cache_read_input_tokens + usage.cache_creation_input_tokens,
        prompt: usage.prompt_tokens(),
        usd: wisp_core::pricing::estimate_usd(info, &usage, TTL),
    })
}

async fn step_zero(model: &str) -> (Measured, Measured) {
    let provider = provider();
    let session = session(model);
    let tools = vec![spec("view_file", "Read a file.")];
    let messages = vec![Message::user_text(padding())];
    let without = Request {
        session: &session,
        tools: &tools,
        deferred: &[],
        by_value: &[],
        server_tools: &[],
        messages: &messages,
        previous_response_id: None,
        features: Features::default(),
        options: RequestOptions::default(),
        rendering: &UNRENDERED,
        compaction: None,
    };
    let with = Request {
        features: Features {
            inline_tools: Some(FeatureUse::Established),
            documented: true,
            ..Features::default()
        },
        ..without
    };
    assert!(
        !provider
            .betas(&without)
            .iter()
            .any(|b| b == INLINE_TOOLS_BETA)
    );
    assert!(provider.betas(&with).iter().any(|b| b == INLINE_TOOLS_BETA));
    let first = measure(&provider, &without).await;
    let second = measure(&provider, &with).await;
    eprintln!(
        "live step 0 on {model}: without the beta prompt {} read {} cached {}; with it prompt {} read {}",
        first.prompt, first.read, first.cached, second.prompt, second.read
    );
    (first, second)
}

const PROMPT_TOKENS: u64 = 12_000;

fn worst_step_usd(model: &str, written: u64, read: u64) -> f64 {
    worst_usd(model, read, 0, written, MAX_TOKENS)
}

#[test]
fn every_step_written_cold_still_fits_under_the_cap() {
    let step_zero: f64 = [HAIKU, SONNET]
        .iter()
        .map(|model| 2.0 * worst_step_usd(model, PROMPT_TOKENS, 0))
        .sum();
    let session = 7.0 * worst_step_usd(HAIKU, PROMPT_TOKENS, 0);
    let sizes = 10.0 * worst_step_usd(HAIKU, PROMPT_TOKENS, 0);
    let same_name = 3.0 * worst_step_usd(HAIKU, PROMPT_TOKENS, 0);
    let worst = step_zero + session + sizes + same_name;
    assert!(worst <= CAP_USD, "${worst:.4}");
    let accepted = 2.0 * worst_step_usd(HAIKU, PROMPT_TOKENS, 0);
    assert!(accepted <= CAP_USD, "${accepted:.4}");
    let tail = 8.0 * worst_step_usd(HAIKU, PROMPT_TOKENS, 0);
    assert!(tail <= CAP_USD, "${tail:.4}");
    let deferred = 7.0 * worst_step_usd(HAIKU, PROMPT_TOKENS, 0);
    assert!(deferred <= DEFERRED_CAP_USD, "${deferred:.4}");
}

fn tools_flag(names: &[&str]) -> Given {
    Given {
        target: Target::Name(find("tools").unwrap()),
        value: SettingValue::List(names.iter().map(|n| (*n).to_owned()).collect()),
        origin: "--tools".into(),
    }
}

async fn started_specs(names: &[&str]) -> Vec<ToolSpec> {
    let (registry, problems) = Registry::new(wisp_plugins::bundled(), &PluginConfig::default());
    assert!(problems.is_empty(), "{problems:?}");
    let on: BTreeSet<String> = names.iter().map(|n| (*n).to_owned()).collect();
    let started = registry.start(&on, Arc::new(KernelHost::new())).await;
    started.toolbox.specs().to_vec()
}

fn log_context() -> LogContext {
    LogContext {
        endpoint: "live".into(),
        endpoint_name: "env".into(),
        credential_source: CredentialSource::Environment,
        redactions: Redactions::default(),
    }
}

async fn switching_agent(scratch: &ScratchDir, store: &MemoryStore) -> Agent {
    let (registry, problems) = Registry::new(wisp_plugins::bundled(), &PluginConfig::default());
    assert!(problems.is_empty(), "{problems:?}");
    let start = SESSION_TOOLS;
    let on: BTreeSet<String> = start.iter().map(|n| (*n).to_owned()).collect();
    let started = registry.start(&on, Arc::new(KernelHost::new())).await;
    Agent::new(Arc::new(provider()), session(HAIKU))
        .with_log(
            SessionLog::new(Box::new(store.clone()), "live"),
            log_context(),
        )
        .with_catalog(started.catalog)
        .with_hooks(started.hooks, started.host)
        .with_tools(
            started.toolbox,
            Workspace {
                root: scratch.root.clone(),
                home: None,
                temp: scratch.dir("tmp"),
            },
        )
        .with_start_layers(StartLayers {
            full_screen: true,
            flags: vec![tools_flag(&start)],
            ..StartLayers::default()
        })
}

struct Live {
    commands: mpsc::Sender<ClientCommand>,
    events: mpsc::Receiver<AgentEvent>,
    spent: f64,
    steps: Vec<Measured>,
}

impl Live {
    async fn tools(&mut self, request: u64, names: &[&str]) {
        let names = names.iter().map(|n| (*n).to_owned()).collect();
        self.commands
            .send(ClientCommand::ChangeSettings {
                request: RequestId(request),
                changes: vec![SettingChange::named("tools", SettingValue::List(names)).unwrap()],
                save: false,
            })
            .await
            .unwrap();
        loop {
            match next_event(&mut self.events).await {
                AgentEvent::SettingsChanged { answers, .. }
                    if answers == Some(RequestId(request)) =>
                {
                    return;
                }
                AgentEvent::Rejected {
                    request: r, reason, ..
                } if r == Some(RequestId(request)) => {
                    panic!("the change was refused: {reason}")
                }
                _ => {}
            }
        }
    }

    async fn turn(&mut self, id: &str, text: &str) -> Vec<AgentEvent> {
        self.commands
            .send(ClientCommand::send(id, text))
            .await
            .unwrap();
        let mut seen = Vec::new();
        loop {
            let event = next_event(&mut self.events).await;
            if let AgentEvent::StepEnded {
                cost_usd, usage, ..
            } = &event
            {
                self.spent += cost_usd.unwrap_or(0.0);
                let usage = usage.unwrap_or_default();
                self.steps.push(Measured {
                    uncached: usage.input_tokens,
                    read: usage.cache_read_input_tokens,
                    cached: usage.cache_read_input_tokens + usage.cache_creation_input_tokens,
                    prompt: usage.prompt_tokens(),
                    usd: cost_usd.unwrap_or(0.0),
                });
            }
            if let AgentEvent::TurnEnded { outcome, .. } = &event {
                assert!(
                    matches!(outcome, TurnOutcome::Completed { .. }),
                    "{id}: {outcome:?}"
                );
                seen.push(event);
                return seen;
            }
            seen.push(event);
        }
    }
}

#[tokio::test(flavor = "multi_thread")]
#[ignore = "live: costs real money; run by hand with credentials in the environment"]
async fn live_tool_changes_keep_the_cache_on_a_capable_model() {
    let mut spent = 0.0;
    for model in [HAIKU, SONNET] {
        let (first, second) = step_zero(model).await;
        spent += first.usd + second.usd;
        eprintln!(
            "live step 0 on {model}: the beta's appearance {}",
            if second.read >= first.cached {
                "kept the cache"
            } else {
                "missed"
            }
        );
    }

    let scratch = ScratchDir::new("wisp-live-", "tool-changes");
    let store = MemoryStore::default();
    let agent = switching_agent(&scratch, &store).await;
    let (commands, events, _task) = agent::spawn(agent);
    let mut live = Live {
        commands,
        events,
        spent: 0.0,
        steps: Vec::new(),
    };
    live.turn("live-tools-0", &padding()).await;
    let plan: [(&str, &[&str]); 5] = [
        ("removes view_file", &["write_todos"]),
        ("turns show_diff on by value", &["write_todos", "show_diff"]),
        ("turns show_diff off by name", &["write_todos"]),
        (
            "turns show_diff on again by value",
            &["write_todos", "show_diff"],
        ),
        (
            "turns view_file back on by name",
            &["write_todos", "show_diff", "view_file"],
        ),
    ];
    for (n, (what, names)) in plan.iter().enumerate() {
        let step = n + 1;
        live.tools(u64::try_from(step).unwrap(), names).await;
        let events = live.turn(&format!("live-tools-{step}"), "Say ok.").await;
        let changed = events
            .iter()
            .any(|e| matches!(e, AgentEvent::ToolsChanged { .. }));
        assert!(changed, "step {step} ({what}) changed no tools");
    }
    live.commands.send(ClientCommand::Cancel).await.unwrap();
    spent += live.spent;

    let tails = tool_messages(&store);
    assert_eq!(tails.len(), plan.len(), "one tool message per step");
    let (sizes, measured) = system_sizes(&tails).await;
    spent += measured;
    for (n, (what, _)) in plan.iter().enumerate() {
        let step = n + 1;
        let now = live.steps[step];
        let before = live.steps[step - 1];
        let previous = if step == 1 { 0 } else { sizes[step - 2] };
        let short = before.cached.saturating_sub(now.read);
        eprintln!(
            "live step {step} ({what}): prompt {} read {} after {} read and {} written; short {short}, its own tool message {} tokens, the previous one's {previous}",
            now.prompt,
            now.read,
            before.read,
            before.cached - before.read,
            sizes[n]
        );
        if previous > 0 {
            assert!(
                now.read >= before.read,
                "step {step} ({what}) lost history the previous request had read: read {} of {}",
                now.read,
                before.read
            );
        } else {
            assert!(
                now.read >= before.cached,
                "step {step} ({what}) missed: read {} of {} cached",
                now.read,
                before.cached
            );
        }
    }

    eprintln!("live: spent ${spent:.4}");
    assert!(spent <= CAP_USD, "spent ${spent:.4}");
}

#[tokio::test(flavor = "multi_thread")]
#[ignore = "live: costs real money; run by hand with credentials in the environment"]
async fn live_a_same_name_addition_by_value_is_accepted_and_reads_the_cache() {
    let provider = provider();
    let session = session(HAIKU);
    let tools = vec![spec("view_file", "Read a file.")];
    let mut messages = vec![Message::user_text(padding())];
    let features = Features {
        inline_tools: Some(FeatureUse::Established),
        documented: true,
        ..Features::default()
    };
    let first = measure(
        &provider,
        &Request {
            session: &session,
            tools: &tools,
            deferred: &[],
            by_value: &[],
            server_tools: &[],
            messages: &messages,
            previous_response_id: None,
            features,
            options: RequestOptions::default(),
            rendering: &UNRENDERED,
            compaction: None,
        },
    )
    .await;
    messages.push(Message {
        role: wisp_protocol::Role::Assistant,
        content: vec![ContentBlock::Text { text: "Ok.".into() }],
    });
    messages.push(Message::user_text("Say ok."));
    messages.push(
        Message::turn_system(
            None,
            &[ContentBlock::ToolAddition {
                tool: ToolRef::ToolDefinition {
                    definition: ToolDefinition {
                        description: "Read a file from the workspace.".into(),
                        input_schema: serde_json::json!({"type": "object", "properties": {"path": {"type": "string"}}}),
                        name: "view_file".into(),
                    },
                },
            }],
        )
        .unwrap(),
    );
    let second = measure(
        &provider,
        &Request {
            session: &session,
            tools: &tools,
            deferred: &[],
            by_value: &[],
            server_tools: &[],
            messages: &messages,
            previous_response_id: None,
            features,
            options: RequestOptions::default(),
            rendering: &UNRENDERED,
            compaction: None,
        },
    )
    .await;
    ok_turn(&mut messages);
    let third = measure(
        &provider,
        &Request {
            session: &session,
            tools: &tools,
            deferred: &[],
            by_value: &[],
            server_tools: &[],
            messages: &messages,
            previous_response_id: None,
            features,
            options: RequestOptions::default(),
            rendering: &UNRENDERED,
            compaction: None,
        },
    )
    .await;
    eprintln!(
        "live step 6: prompt {} read {} after {} cached; then prompt {} read {} after {} read",
        second.prompt, second.read, first.cached, third.prompt, third.read, second.read
    );
    assert!(
        second.read >= first.cached,
        "step 6 missed: read {} of {} cached",
        second.read,
        first.cached
    );
    assert!(
        third.read >= second.read,
        "step 6 lost history the addition's request had read: read {} of {}",
        third.read,
        second.read
    );
    let spent = first.usd + second.usd + third.usd;
    assert!(spent <= CAP_USD, "spent ${spent:.4}");
}

#[tokio::test(flavor = "multi_thread")]
#[ignore = "live: costs real money; run by hand with credentials in the environment"]
async fn live_a_text_system_message_and_a_marker_on_a_tool_removal_are_accepted() {
    let provider = provider();
    let session = session(HAIKU);
    let tools = vec![spec("view_file", "Read a file.")];
    let mut messages = vec![Message::user_text("Say ok.")];
    messages.push(
        Message::turn_system(
            None,
            &[ContentBlock::Text {
                text: "The show_diff tool shows the user a diff.".into(),
            }],
        )
        .unwrap(),
    );
    let text = measure(&provider, &plain_request(&session, &tools, &messages, None)).await;
    messages.push(Message::user_text("Say ok once more."));
    let steered = measure(&provider, &plain_request(&session, &tools, &messages, None)).await;
    ok_turn(&mut messages);
    messages.push(
        Message::turn_system(
            None,
            &[ContentBlock::ToolRemoval {
                tool: ToolRef::ToolReference {
                    name: "view_file".into(),
                },
            }],
        )
        .unwrap(),
    );
    let ask = CompactionAsk {
        native_instructions: None,
        tail: Some(TailMarker {
            view_len: messages.len(),
            ttl: TTL,
            prefix: sent_prefix(&plain_request(&session, &tools, &messages, None)).unwrap(),
            last_blocks: None,
        }),
    };
    let marked = measure(
        &provider,
        &plain_request(&session, &tools, &messages, Some(&ask)),
    )
    .await;
    eprintln!(
        "live: a text system message took prompt {}, with a user message after it {}; a marked tool removal took prompt {}",
        text.prompt, steered.prompt, marked.prompt
    );
    let spent = text.usd + steered.usd + marked.usd;
    assert!(spent <= CAP_USD, "spent ${spent:.4}");
}

fn tool_messages(store: &MemoryStore) -> Vec<Message> {
    store
        .read("live")
        .unwrap()
        .into_iter()
        .filter_map(|r| match r.event {
            LogEvent::TurnStarted { tool_message, .. } if !tool_message.is_empty() => {
                Message::turn_system(None, &tool_message)
            }
            _ => None,
        })
        .collect()
}

fn ok_turn(messages: &mut Vec<Message>) {
    messages.push(Message {
        role: wisp_protocol::Role::Assistant,
        content: vec![ContentBlock::Text { text: "Ok.".into() }],
    });
    messages.push(Message::user_text("Say ok."));
}

fn inline() -> Features {
    Features {
        inline_tools: Some(FeatureUse::Established),
        documented: true,
        ..Features::default()
    }
}

fn plain_request<'a>(
    session: &'a wisp_protocol::SessionConfig,
    tools: &'a [ToolSpec],
    messages: &'a [Message],
    compaction: Option<&'a CompactionAsk>,
) -> Request<'a> {
    Request {
        session,
        tools,
        deferred: &[],
        by_value: &[],
        server_tools: &[],
        messages,
        previous_response_id: None,
        features: inline(),
        options: RequestOptions {
            refusal_fallback: false,
            debug_cache: false,
            message_threads: true,
        },
        rendering: &UNRENDERED,
        compaction,
    }
}

async fn system_sizes(tails: &[Message]) -> (Vec<u64>, f64) {
    let provider = provider();
    let session = session(HAIKU);
    let tools = started_specs(&SESSION_TOOLS).await;
    let mut base = vec![Message::user_text(padding())];
    ok_turn(&mut base);
    let mut spent = 0.0;
    let mut sizes = Vec::new();
    for tail in tails {
        let plain = measure(&provider, &plain_request(&session, &tools, &base, None)).await;
        base.push(tail.clone());
        let with = measure(&provider, &plain_request(&session, &tools, &base, None)).await;
        spent += plain.usd + with.usd;
        sizes.push(with.prompt.saturating_sub(plain.prompt));
        ok_turn(&mut base);
    }
    eprintln!("live: the steps' tool messages measure {sizes:?} tokens");
    (sizes, spent)
}

struct Branch {
    system: u64,
    earlier: u64,
    cached: u64,
    read: u64,
    usd: f64,
}

async fn branch(words: [&str; 8], explicit: bool) -> Branch {
    let provider = provider();
    let session = session(HAIKU);
    let tools = started_specs(&SESSION_TOOLS).await;
    let mut messages = vec![Message::user_text(padding_of(words))];
    ok_turn(&mut messages);
    let plain = measure(&provider, &plain_request(&session, &tools, &messages, None)).await;
    let user_end = messages.len();
    ok_turn(&mut messages);
    messages.push(
        Message::turn_system(
            None,
            &[ContentBlock::ToolRemoval {
                tool: ToolRef::ToolReference {
                    name: "view_file".into(),
                },
            }],
        )
        .unwrap(),
    );
    let without_tail = &messages[..messages.len() - 1];
    let ask = explicit.then(|| CompactionAsk {
        native_instructions: None,
        tail: Some(TailMarker {
            view_len: without_tail.len(),
            ttl: TTL,
            prefix: sent_prefix(&plain_request(&session, &tools, without_tail, None)).unwrap(),
            last_blocks: None,
        }),
    });
    let tail = measure(
        &provider,
        &plain_request(&session, &tools, &messages, ask.as_ref()),
    )
    .await;
    let bare = user_end + 2;
    ok_turn(&mut messages);
    let next = measure(&provider, &plain_request(&session, &tools, &messages, None)).await;
    let alone = measure(
        &provider,
        &plain_request(&session, &tools, &messages[..bare], None),
    )
    .await;
    Branch {
        system: tail.prompt.saturating_sub(alone.prompt),
        earlier: tail.read,
        cached: tail.cached,
        read: next.read,
        usd: plain.usd + tail.usd + next.usd + alone.usd,
    }
}

#[tokio::test(flavor = "multi_thread")]
#[ignore = "live: costs real money; run by hand with credentials in the environment"]
async fn live_a_tool_message_at_the_tail_costs_at_most_its_own_tokens_on_the_next_request() {
    let automatic = branch(
        [
            "amber", "birch", "cedar", "delta", "ember", "fjord", "grove", "heath",
        ],
        false,
    )
    .await;
    let on_user = branch(
        [
            "inlet", "juniper", "kelp", "larch", "maple", "nettle", "oak", "pine",
        ],
        true,
    )
    .await;
    for (name, b) in [
        ("breakpoint on the tool message (automatic)", &automatic),
        ("breakpoint on the user message's last block", &on_user),
    ] {
        eprintln!(
            "live tail, {name}: tool message {} tokens; read {} and cached {} then read {} ({} short); ${:.6}",
            b.system,
            b.earlier,
            b.cached,
            b.read,
            b.cached.saturating_sub(b.read),
            b.usd
        );
    }
    assert!(
        automatic.read >= automatic.earlier,
        "the next request lost history the tail request had read: {} of {}",
        automatic.read,
        automatic.earlier
    );
    let spent = automatic.usd + on_user.usd;
    assert!(spent <= CAP_USD, "spent ${spent:.4}");
}

fn deferring() -> Features {
    Features {
        deferred_tools: Some(FeatureUse::Established),
        ..inline()
    }
}

fn deferred_request<'a>(
    session: &'a wisp_protocol::SessionConfig,
    tools: &'a [ToolSpec],
    deferred: &'a [ToolSpec],
    messages: &'a [Message],
) -> Request<'a> {
    Request {
        deferred,
        features: deferring(),
        ..plain_request(session, tools, messages, None)
    }
}

fn line(name: &str, m: Measured) -> String {
    format!(
        "{name}: prompt {} uncached {} read {} written {} ${:.6}",
        m.prompt,
        m.uncached,
        m.read,
        m.cached - m.read,
        m.usd
    )
}

fn surfacing(name: &str) -> Message {
    Message::turn_system(
        None,
        &[ContentBlock::ToolAddition {
            tool: ToolRef::ToolReference { name: name.into() },
        }],
    )
    .unwrap()
}

#[tokio::test(flavor = "multi_thread")]
#[ignore = "live: costs real money; run by hand with credentials in the environment"]
async fn live_a_deferred_tool_costs_no_more_than_declaring_it_and_surfaces_from_the_cache() {
    let provider = provider();
    let session = session(HAIKU);
    let base = started_specs(&["view_file"]).await;
    let both = started_specs(&["view_file", "show_diff"]).await;
    let deferred: Vec<ToolSpec> = both
        .iter()
        .filter(|s| s.name == "show_diff")
        .cloned()
        .collect();
    assert_eq!(deferred.len(), 1);
    let opening = |tag: &str| {
        let mut messages = vec![Message::user_text(format!("Run {tag}. {}", padding()))];
        let first = messages.clone();
        ok_turn(&mut messages);
        (first, messages)
    };
    let mut spent = 0.0;
    let mut runs = Vec::new();
    for (tag, tools, deferred) in [
        ("A", &base, &[][..]),
        ("B", &both, &[][..]),
        ("C", &base, &deferred[..]),
    ] {
        let (first, second) = opening(tag);
        let one = measure(
            &provider,
            &deferred_request(&session, tools, deferred, &first),
        )
        .await;
        let two = measure(
            &provider,
            &deferred_request(&session, tools, deferred, &second),
        )
        .await;
        spent += one.usd + two.usd;
        eprintln!("{}", line(&format!("live deferred, run {tag} first"), one));
        eprintln!("{}", line(&format!("live deferred, run {tag} second"), two));
        runs.push((one, two, second));
    }
    let (without, declared, deferring_run) = (runs[0].1, runs[1].1, runs[2].1);
    let mut surfaced_messages = runs[2].2.clone();
    surfaced_messages.push(surfacing("show_diff"));
    let surfaced = measure(
        &provider,
        &deferred_request(&session, &base, &deferred, &surfaced_messages),
    )
    .await;
    spent += surfaced.usd;
    eprintln!("{}", line("live deferred, run C surfacing", surfaced));
    eprintln!(
        "live deferred: the deferred declaration adds {} prompt tokens over none and {} over declaring it",
        deferring_run.prompt as i64 - without.prompt as i64,
        deferring_run.prompt as i64 - declared.prompt as i64
    );
    eprintln!("live deferred: spent ${spent:.4}");
    assert!(spent <= DEFERRED_CAP_USD, "spent ${spent:.4}");
    assert!(
        deferring_run.uncached <= without.uncached,
        "a deferred declaration adds uncached input per request: {} against {}",
        deferring_run.uncached,
        without.uncached
    );
    assert!(
        deferring_run.usd <= declared.usd,
        "a deferred declaration costs more per request than declaring the tool: ${:.6} against ${:.6}",
        deferring_run.usd,
        declared.usd
    );
    assert!(
        surfaced.read >= deferring_run.cached,
        "the surfacing request missed the cache: read {} of {}",
        surfaced.read,
        deferring_run.cached
    );
}

fn lookup(input_schema: serde_json::Value) -> ToolSpec {
    ToolSpec {
        description: "Look up today's weather for a city.".into(),
        input_schema,
        name: "lookup".into(),
    }
}

#[tokio::test(flavor = "multi_thread")]
#[ignore = "live: costs real money; run by hand with credentials in the environment"]
async fn live_a_same_name_addition_by_value_replaces_a_deferred_declaration() {
    let provider = provider();
    let mut session = session(HAIKU);
    session.max_tokens = 256;
    let tools = vec![spec("view_file", "Read a file.")];
    let old = lookup(serde_json::json!({
        "type": "object",
        "properties": {"city": {"type": "string"}},
        "required": ["city"]
    }));
    let new = lookup(serde_json::json!({
        "type": "object",
        "properties": {
            "city": {"type": "string"},
            "units": {"type": "string", "enum": ["celsius", "fahrenheit"]}
        },
        "required": ["city", "units"]
    }));
    let messages = vec![
        Message::user_text("Call the lookup tool for Paris now, with no other text."),
        Message::turn_system(
            None,
            &[ContentBlock::ToolAddition {
                tool: ToolRef::ToolDefinition {
                    definition: ToolDefinition {
                        description: new.description.clone(),
                        input_schema: new.input_schema.clone(),
                        name: new.name.clone(),
                    },
                },
            }],
        )
        .unwrap(),
    ];
    let deferred = [old];
    let request = deferred_request(&session, &tools, &deferred, &messages);
    let mut stream = provider.stream(&request);
    let mut usage = Usage::default();
    let mut called = None;
    let mut input = String::new();
    while let Some(event) = stream.next().await {
        match event.unwrap_or_else(|e| panic!("refused: {e:?}")) {
            ProviderEvent::MessageStart { usage: start, .. } => usage = start,
            ProviderEvent::MessageDelta { usage: delta, .. } => usage = delta,
            ProviderEvent::BlockStart {
                block: ContentBlock::ToolUse { name, .. },
                ..
            } => called = Some(name),
            ProviderEvent::BlockDelta {
                delta: wisp_core::provider::Delta::InputJson(part),
                ..
            } => input.push_str(&part),
            _ => {}
        }
    }
    let info = wisp_core::pricing::model_info(HAIKU).unwrap();
    let spent = wisp_core::pricing::estimate_usd(info, &usage, TTL);
    eprintln!("live upgraded deferred: called {called:?} with {input} for ${spent:.6}");
    assert_eq!(called.as_deref(), Some("lookup"));
    let input: serde_json::Value = serde_json::from_str(&input).unwrap();
    assert!(input.get("units").is_some(), "{input}");
    assert!(spent <= 0.02, "spent ${spent:.4}");
}

async fn preload_tools(value: &str) -> wisp_core::tool::Toolbox {
    use wisp_core::kit::BuildTools;
    let (registry, problems) = Registry::new(wisp_plugins::bundled(), &PluginConfig::default());
    assert!(problems.is_empty(), "{problems:?}");
    let registry = Arc::new(registry);
    let started = wisp_core::plugin::Tools::new(registry.clone(), registry.default_on())
        .with_preload(vec![value.to_owned()])
        .start("/", Arc::new(KernelHost::new()))
        .await;
    started.toolbox
}

fn gateway(kind: wisp_core::endpoints::ProviderKind) -> Arc<dyn Provider> {
    use wisp_core::endpoints::{EndpointName, PricingMode, ResolvedEndpoint, UrlSource};
    use wisp_core::provider::credential::{Credential, Source};
    let endpoint = ResolvedEndpoint {
        client_version: None,
        name: EndpointName::parse("live-preload").unwrap(),
        provider: kind,
        credential: Credential::AuthToken(
            std::env::var("MANIFEST_API_KEY").expect("test gateway key is required"),
        ),
        base_url: std::env::var("MANIFEST_BASE_URL").expect("test gateway URL is required"),
        source: Source::Environment,
        key_env: None,
        url_source: UrlSource::Official,
        pricing: PricingMode::List,
    };
    match kind {
        wisp_core::endpoints::ProviderKind::OpenAi => {
            Arc::new(wisp_core::provider::openai::OpenAiProvider::from_endpoint(&endpoint).unwrap())
        }
        wisp_core::endpoints::ProviderKind::Gemini => {
            Arc::new(wisp_core::provider::gemini::GeminiProvider::from_endpoint(&endpoint).unwrap())
        }
        _ => unreachable!(),
    }
}

#[tokio::test(flavor = "multi_thread")]
#[ignore = "live: costs real money; run by hand with the project's test credentials"]
async fn live_preload_core_and_none_measure_real_prompt_tokens_per_provider() {
    use wisp_core::endpoints::ProviderKind;
    let mut spent = 0.0;
    for (name, model, provider) in [
        ("Claude", HAIKU, Arc::new(provider()) as Arc<dyn Provider>),
        ("OpenAI", "gpt-5.6-luna", gateway(ProviderKind::OpenAi)),
        (
            "Gemini",
            "gemini-3.1-flash-lite",
            gateway(ProviderKind::Gemini),
        ),
    ] {
        let mut prompts = Vec::new();
        for value in ["core", "none"] {
            let mut session = session(model);
            let toolbox = preload_tools(value).await;
            session.system = Some(toolbox.system_lines().join("\n"));
            let tools = toolbox.specs();
            let messages = vec![Message::user_text("Reply OK only. Do not call any tools.")];
            let request = plain_request(&session, tools, &messages, None);
            let run = tokio::time::timeout(
                std::time::Duration::from_secs(90),
                try_measure(provider.as_ref(), &request),
            )
            .await;
            let unavailable = match run {
                Ok(Ok(measured)) => {
                    spent += measured.usd;
                    eprintln!(
                        "{}",
                        line(&format!("live preload {name} {value}"), measured)
                    );
                    prompts.push(measured.prompt);
                    false
                }
                Ok(Err(error)) => {
                    eprintln!(
                        "live preload {name} {value}: {:?}, HTTP {:?}, possibly billed {}",
                        error.kind, error.detail.status, error.possibly_billed
                    );
                    assert!(
                        !matches!(error.detail.status, Some(401 | 403)),
                        "live authentication refused; no further live requests; visible estimated spend ${spent:.6}"
                    );
                    assert!(
                        name != "Claude",
                        "direct Claude benchmark unavailable: {:?}, HTTP {:?}; visible estimated spend ${spent:.6}",
                        error.kind,
                        error.detail.status
                    );
                    true
                }
                Err(_) => {
                    eprintln!("live preload {name} {value}: timeout; usage unavailable");
                    true
                }
            };
            if unavailable {
                let bytes = provider
                    .prefix_bytes(&request)
                    .expect("the provider exposes its tools prefix")
                    .tools
                    .len();
                eprintln!(
                    "live preload {name} {value}: unavailable; local {} tool definitions, {bytes} request-array bytes; no usage reported",
                    tools.len()
                );
            }
        }
        assert_eq!(
            prompts.len(),
            2,
            "{name}: both token measurements are required; visible estimated spend ${spent:.6}"
        );
        assert!(prompts[0] > prompts[1], "{name}: {prompts:?}");
    }
    eprintln!("live preload: estimated spend ${spent:.6}");
    assert!(spent <= CAP_USD, "spent ${spent:.6}");
}

#[tokio::test(flavor = "multi_thread")]
#[ignore = "live: costs real money; run by hand with credentials in the environment"]
async fn live_preload_measures_the_per_tool_cost_of_deferred_declarations() {
    let provider = provider();
    let session = session(HAIKU);
    let base = preload_tools("none").await.specs().to_vec();
    let held: Vec<ToolSpec> = preload_tools("core")
        .await
        .specs()
        .to_vec()
        .into_iter()
        .filter(|tool| !base.iter().any(|loaded| loaded.name == tool.name))
        .collect();
    assert!(held.len() > 1);
    let messages = vec![Message::user_text("Reply OK only. Do not call any tools.")];
    let mut prompts = Vec::new();
    let mut spent = 0.0;
    for n in [0, 1, held.len()] {
        let measured = measure(
            &provider,
            &deferred_request(&session, &base, &held[..n], &messages),
        )
        .await;
        spent += measured.usd;
        prompts.push(measured.prompt);
        eprintln!("{}", line(&format!("live preload deferred {n}"), measured));
    }
    eprintln!(
        "live preload deferred: {} declarations add {} prompt tokens; one adds {}; estimated spend ${spent:.6}",
        held.len(),
        prompts[2] - prompts[0],
        prompts[1] - prompts[0]
    );
    assert!(
        prompts[2] >= prompts[1] && prompts[1] >= prompts[0],
        "{prompts:?}"
    );
    assert!(spent <= DEFERRED_CAP_USD, "spent ${spent:.6}");
}

async fn found_after_results(provider: &dyn Provider, model: &str) {
    let base = [spec("search", "Find a tool.")];
    let late = spec(
        "late_probe",
        "Call this harmless test tool with path set to ok.",
    );
    let messages = [
        Message::user_text("Find the test tool and call it once with path ok."),
        Message {
            role: wisp_protocol::Role::Assistant,
            content: vec![ContentBlock::ToolUse {
                id: "search_probe".into(),
                name: "search".into(),
                input: serde_json::json!({"path":"test"}),
            }],
        },
        Message {
            role: wisp_protocol::Role::User,
            content: vec![ContentBlock::ToolResult {
                tool_use_id: "search_probe".into(),
                content: vec![ContentBlock::Text {
                    text: "tool late_probe — test — call it with path ok".into(),
                }],
                is_error: false,
            }],
        },
        Message::turn_system(
            None,
            &[ContentBlock::ToolAddition {
                tool: ToolRef::ToolDefinition {
                    definition: ToolDefinition {
                        name: late.name,
                        description: late.description,
                        input_schema: late.input_schema,
                    },
                },
            }],
        )
        .unwrap(),
    ];
    let mut session = session(model);
    session.effort = wisp_protocol::Effort::Low;
    let request = Request {
        features: inline(),
        ..plain_request(&session, &base, &messages, None)
    };
    let mut assembled = wisp_core::provider::Assembler::default();
    let result = tokio::time::timeout(std::time::Duration::from_secs(90), async {
        let mut stream = provider.stream(&request);
        while let Some(event) = stream.next().await {
            assembled.apply(event?)?;
        }
        assembled.finish()
    })
    .await
    .expect("one short live probe timed out");
    match result {
        Ok(response) => {
            assert!(
                response.usage.prompt_tokens() > 0,
                "the live response reported no prompt usage"
            );
            assert!(response.content.iter().any(|block| matches!(block, ContentBlock::ToolUse { name, input, .. } if name == "late_probe" && input["path"] == "ok")), "the added tool was not called");
            eprintln!(
                "live found after results: model {model}, estimated spend ${:.6}",
                wisp_core::pricing::estimate_usd(
                    wisp_core::pricing::model_info(model).unwrap(),
                    &response.usage,
                    TTL
                )
            );
        }
        Err(error) => {
            eprintln!(
                "live provider error type: {}; message: {}",
                live::scrubbed(&error.detail.error_type.as_deref().unwrap_or("unknown")),
                live::scrubbed(&error.message)
            );
            panic!(
                "live found-tool request refused: {:?}, HTTP {:?}",
                error.kind, error.detail.status
            );
        }
    }
}

#[tokio::test(flavor = "multi_thread")]
#[ignore = "live: one short request, costs real money; requires test credentials"]
async fn live_found_tool_after_claude_results() {
    let provider = AnthropicProvider::new(
        &wisp_core::provider::anthropic::Credential::ApiKey(
            std::env::var("ANTHROPIC_API_KEY").expect("the project test API key is required"),
        ),
        wisp_core::provider::anthropic::DEFAULT_BASE_URL,
    )
    .unwrap();
    found_after_results(&provider, "claude-opus-5-5").await;
}

#[tokio::test(flavor = "multi_thread")]
#[ignore = "live: test gateway compatibility only; a pass fills no official API or subscription row"]
async fn live_found_tool_after_openai_outputs() {
    found_after_results(
        gateway(wisp_core::endpoints::ProviderKind::OpenAi).as_ref(),
        "gpt-6.1-sol",
    )
    .await;
    eprintln!("test gateway only: no official API or subscription capability row is proven");
}

#[allow(
    dead_code,
    reason = "the shared local MCP fixture also supports sign-in tests"
)]
#[path = "../../../wisp-core/tests/support/mcp-http.rs"]
mod managed_server;

#[tokio::test(flavor = "multi_thread")]
#[ignore = "live: two short turns on one model, costs real money; requires test credentials"]
async fn live_manage_adds_a_server_and_turns_it_off_after_finding_manage() {
    let scratch = ScratchDir::new("wisp-live-", "manage-server");
    let local = managed_server::Server::start(false, None).await;
    let mcp = wisp_core::mcp::Service::new(
        Default::default(),
        scratch.root.clone(),
        Some(scratch.dir("mcp")),
    )
    .with_config(
        scratch.root.join("config.toml"),
        wisp_core::endpoints::PathShown::Given,
    );
    let (registry, problems) = Registry::new(wisp_plugins::bundled(), &PluginConfig::default());
    assert!(problems.is_empty());
    let host = Arc::new(KernelHost::new());
    host.set_mcp(mcp.clone());
    let started = registry.start(&registry.default_on(), host).await;
    let store = MemoryStore::default();
    let mut config = session(HAIKU);
    config.effort = wisp_protocol::Effort::Low;
    config.max_tokens = 256;
    let agent = Agent::new(Arc::new(provider()), config)
        .with_log(SessionLog::new(Box::new(store), "live"), log_context())
        .with_catalog(started.catalog)
        .with_hooks(started.hooks, started.host)
        .with_tools(
            started.toolbox,
            Workspace {
                root: scratch.root.clone(),
                home: None,
                temp: scratch.dir("tmp"),
            },
        )
        .with_start_layers(StartLayers {
            full_screen: true,
            ..StartLayers::default()
        });
    let (commands, events, task) = agent::spawn(agent);
    let mut live = Live {
        commands,
        events,
        spent: 0.0,
        steps: Vec::new(),
    };
    let events = tokio::time::timeout(std::time::Duration::from_secs(180), async {
        let mut events = live.turn("live-manage-add", &format!("Add the github MCP server from {}. Do not use the shell. Reply in one short sentence when done.", local.address())).await;
        events.extend(live.turn("live-manage-off", "Turn the github server's tools off in this chat. Reply in one short sentence when done.").await);
        events
    }).await.expect("two short manage turns timed out");
    let calls: Vec<_> = events
        .iter()
        .filter_map(|event| match event {
            AgentEvent::ToolCallRequested { tool, input, .. } => Some((tool.as_str(), input)),
            _ => None,
        })
        .collect();
    let searched = calls.iter().position(|(tool, _)| *tool == "search");
    let managed = calls.iter().position(|(tool, input)| {
        *tool == "manage" || (*tool == "use" && input["name"] == "manage")
    });
    let through_search =
        matches!((searched, managed), (Some(search), Some(manage)) if search < manage);
    eprintln!(
        "live manage: model {HAIKU}, reached manage through search: {through_search}, estimated spend ${:.6}",
        live.spent
    );
    task.handle.abort();
    let _ = task.handle.await;
    assert!(managed.is_some(), "manage was not called");
    assert!(mcp.rows().iter().any(|row| row.name == "github"));
    assert!(calls.iter().any(
        |(tool, input)| (*tool == "manage" && input["action"] == "off")
            || (*tool == "use" && input["name"] == "manage" && input["input"]["action"] == "off")
    ));
}
