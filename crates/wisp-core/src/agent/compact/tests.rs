use std::sync::{Arc, Mutex};
use std::time::Duration;

use futures::FutureExt;
use futures::future::BoxFuture;
use serde_json::{Value, json};
use tokio::sync::{Semaphore, mpsc};
use wisp_protocol::context::{Category, Fill, ReportDetail};
use wisp_protocol::log::{FeatureState, LogEvent, no_images, rebuild_history};
use wisp_protocol::settings::{FileState, SettingChange, SettingValue};
use wisp_protocol::{
    AgentEvent, Answer, CacheEffect, ClientCommand, CompactionEngine, CompactionFailure,
    CompactionTrigger, ContentBlock, DeliveryMode, ErrorKind, FailurePhase, InputId, ProcessEnd,
    RebuildReason, RejectKind, RequestId, RetryClass, StopReason, ToolAction, ToolDisplay,
    ToolOutcome, ToolPurpose, TurnOutcome, Usage,
};

use super::*;
use crate::agent::testing::{
    Caps, Fake, Knobs, Prefix, Script, Sent, Step, body_of, classified, ended, mid_stream, opaque,
    started, text, thinking, tool_use,
};
use crate::agent::tests::completed;
use crate::agent::tests::prepared;
use crate::agent::tests::{failing, overloaded, search_off, usage};
use crate::agent::tool_tests::send;
use crate::agent::{Connection, DEFAULT_MODEL, LogContext, default_session};
use crate::config_file::SiteEdit;
use crate::context::ModelSettings;
use crate::models::{ConfigModels, ModelTable};
use crate::provider::anthropic::COMPACTION_BETA;
use crate::provider::{Provider, ProviderError, Thread, reply_text};
use crate::session::{MemoryStore, Redactions, SessionLog, SessionStore};
use crate::settings::host::{HostError, SavedFile, SavedRead, SettingsHost};
use crate::test_guard::{
    WISP, continued, log_context, log_events, models, pick, server_error, stored, until,
    until_event, within,
};
use wisp_plugin_compaction::{ATTACHED_LEFT, CUT_OFF_INTRO, SUMMARY_INSTRUCTION};
use wisp_tool_text::STEER_NOTE;
use wisp_tool_text::compaction::INSTRUCTIONS_LINE;

const SUMMARY: &str = "So far: a greeting.";
const WRITTEN: &str = "## Goal and standing instructions\nSay hello.\n\n## Decisions\nNone yet.\n\n## State of the work\nDone: the greeting.\n\n## Files, branches and artefacts\nNone.\n\n## Commands, errors and fixes\nNone.\n\n## Open questions\nNone.";

fn temp_workspace() -> crate::workspace::Workspace {
    let dir = std::env::temp_dir();
    crate::workspace::Workspace {
        root: dir.clone(),
        home: None,
        temp: dir,
    }
}

mod attachments;
mod auto;
mod auto_rules;
mod background;
mod breaks;
mod established;
mod evict;
mod fallback_head;
mod gate;
mod gate_cost;
mod gate_held;
mod head_hooks;
mod held;
mod hooks;
mod image_lost;
mod kept_processes;
mod kept_tools;
mod lanes;
mod log;
mod marker;
#[cfg(any(target_os = "linux", target_os = "macos"))]
pub(in crate::agent) mod memory;
mod midturn;
mod overflow;
mod processes;
mod quote;
mod quote_retry;
mod quote_withdraw;
mod refused_thinking;
mod resume;
mod resume_bytes;
mod retry;
mod retry_rerun;
mod rewind;
mod sections;
mod smaller;
mod strategy;
mod switch;
mod threshold;
mod unreadable;
mod usage;
pub(super) mod views;
mod wisp;
mod withdrawals;

const NARROW_MODEL: &str = "claude-narrow-test";

const NARROW: crate::provider::ModelCapabilities = crate::provider::ModelCapabilities {
    slices: &[],
    efforts: &[
        Effort::Low,
        Effort::Medium,
        Effort::High,
        Effort::Xhigh,
        Effort::Max,
    ],
    default_effort: Some(Effort::Medium),
    per_message_effort: false,
    effort_in_id: None,
    adaptive_thinking: true,
    thinking_budgets: None,
    max_output: Some(64_000),
    images: None,
    inline_tools: false,
    tools_after_results: false,
    deferred_tools: false,
};

fn narrow_or_row(model: &str) -> Option<&'static crate::provider::ModelCapabilities> {
    if model == NARROW_MODEL {
        return Some(&NARROW);
    }
    crate::provider::anthropic::capabilities::row(model)
}

fn with_documented(documented: bool, steps: Vec<Step>) -> Arc<Fake> {
    Fake::with(
        Knobs {
            prefix: Prefix::Sent,
            caps: Some(Caps::Fn(narrow_or_row)),
            documented: Some(documented),
            keeps_warm: Some(true),
            media: true,
            ..Knobs::default()
        },
        steps,
    )
}

fn scripted(steps: Vec<Step>) -> Arc<Fake> {
    with_documented(false, steps)
}

fn documented(steps: Vec<Step>) -> Arc<Fake> {
    with_documented(true, steps)
}

fn done(native: bool) -> Step {
    if native {
        compaction(SUMMARY)
    } else {
        answer(WRITTEN, 60_000)
    }
}

fn start(input: u64, read: u64) -> Result<ProviderEvent, ProviderError> {
    Ok(crate::agent::tests::start(input, read))
}

fn finish(stop: StopReason, input: u64, read: u64, output: u64) -> Script {
    ended(stop, usage(input, read, output), vec![])
}

fn answer(words: &str, input: u64) -> Step {
    answer_read(words, input, 0)
}

fn answer_read(words: &str, input: u64, read: u64) -> Step {
    let mut script = vec![start(input, read)];
    script.extend(text(0, words));
    script.extend(finish(StopReason::EndTurn, input, read, 40));
    Step::from(script)
}

fn raw_block(summary: &str) -> Value {
    json!({"type": "compaction", "content": summary})
}

fn summarised(summary: &str, input: u64, read: u64, extra: Vec<ProviderEvent>) -> Step {
    let mut script = vec![start(input, read)];
    script.extend(extra.into_iter().map(Ok));
    script.extend(opaque(0, raw_block(summary)));
    script.extend(finish(StopReason::Compaction, input, read, 300));
    Step::from(script)
}

pub(super) fn paused_search(input: u64, output: u64) -> Step {
    let mut script = vec![start(input, 0)];
    script.extend(opaque(
        0,
        json!({"type": "server_tool_use", "id": "srvtoolu_1", "name": "web_search", "input": {"query": "rust"}}),
    ));
    script.extend(finish(StopReason::PauseTurn, input, 0, output));
    Step::from(script)
}

fn compaction(summary: &str) -> Step {
    summarised(summary, 1_200, 0, vec![])
}

fn server_failed() -> Step {
    failing(classified(
        ErrorKind::Server,
        "server error",
        FailurePhase::Response,
        RetryClass::Transient,
    ))
}

fn beta_refused() -> Step {
    let mut error = ProviderError::new(ErrorKind::InvalidRequest, "compaction: Extra inputs");
    error.rejected = vec![Feature::NativeCompaction];
    failing(error)
}

fn block_refused() -> Step {
    failing(
        ProviderError::new(ErrorKind::InvalidRequest, "compaction_signature_invalid")
            .refusing(Refused::CompactionBlock),
    )
}

fn logged(provider: Arc<Fake>, store: &MemoryStore) -> Agent {
    Agent::new(provider, default_session()).with_log(
        SessionLog::new(Box::new(store.clone()), "s1"),
        log_context(),
    )
}

fn two_endpoints(agent: Agent, provider: Arc<Fake>) -> Agent {
    two_endpoints_on(agent, provider, "env")
}

fn two_endpoints_on(agent: Agent, provider: Arc<Fake>, active: &str) -> Agent {
    let connection = |name: &str| Connection {
        name: name.into(),
        provider: provider.clone(),
        context: LogContext {
            endpoint_name: name.into(),
            ..log_context()
        },
        priced: true,
    };
    agent.with_connections(vec![connection("env"), connection("other")], vec![], active)
}

fn compact(request: u64, focus: Option<&str>) -> ClientCommand {
    ClientCommand::Compact {
        focus: focus.map(str::to_owned),
        request: RequestId(request),
    }
}

fn answers(event: &AgentEvent, request: u64) -> bool {
    let wanted = Some(RequestId(request));
    match event {
        AgentEvent::Compacted { answers, .. } | AgentEvent::CompactionFailed { answers, .. } => {
            *answers == wanted
        }
        AgentEvent::Rejected { request, .. } => *request == wanted,
        _ => false,
    }
}

struct Live {
    commands: Option<mpsc::Sender<ClientCommand>>,
    events: mpsc::Receiver<AgentEvent>,
    seen: Vec<AgentEvent>,
    task: tokio::task::JoinHandle<Agent>,
}

impl Live {
    fn start(mut agent: Agent) -> Self {
        let (commands, mut inbox) = mpsc::channel(16);
        let (sink, events) = mpsc::channel(4_096);
        let task = tokio::spawn(async move {
            agent.serve(&mut inbox, &sink).await;
            agent
        });
        Self {
            commands: Some(commands),
            events,
            seen: Vec::new(),
            task,
        }
    }

    async fn send(&self, command: ClientCommand) {
        if let Some(commands) = &self.commands {
            commands.send(command).await.unwrap();
        }
    }

    async fn until(&mut self, pick: impl Fn(&AgentEvent) -> bool) -> AgentEvent {
        until_event(&mut self.events, &mut self.seen, "the event", pick).await
    }

    async fn turn(&mut self, id: &str, text: &str) -> TurnOutcome {
        self.send(send(id, text)).await;
        match self
            .until(|e| matches!(e, AgentEvent::TurnEnded { .. }))
            .await
        {
            AgentEvent::TurnEnded { outcome, .. } => outcome,
            _ => unreachable!(),
        }
    }

    async fn compact(&mut self, request: u64, focus: Option<&str>) -> AgentEvent {
        self.send(compact(request, focus)).await;
        self.until(|e| answers(e, request)).await
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

async fn switch(live: &mut Live, request: u64, endpoint: Option<&str>, model: Option<&str>) {
    live.send(ClientCommand::shortcut(
        RequestId(request),
        endpoint.map(str::to_owned),
        model.map(str::to_owned),
        None,
    ))
    .await;
    live.until(|e| matches!(e, AgentEvent::SettingsChanged { .. }))
        .await;
}

fn head_text(agent: &Agent) -> String {
    reply_text(&agent.compaction.head.as_ref().unwrap().message.content).unwrap_or_default()
}

fn strip_cache_control(value: &Value) -> Value {
    match value {
        Value::Object(map) => Value::Object(
            map.iter()
                .filter(|(k, _)| k.as_str() != "cache_control")
                .map(|(k, v)| (k.clone(), strip_cache_control(v)))
                .collect(),
        ),
        Value::Array(items) => Value::Array(items.iter().map(strip_cache_control).collect()),
        other => other.clone(),
    }
}

#[tokio::test]
async fn a_manual_compact_sends_the_view_and_installs_a_native_head() {
    let store = MemoryStore::default();
    let provider = scripted(vec![
        answer("Hello.", 1_000),
        compaction(SUMMARY),
        answer("Again.", 300),
    ]);
    let mut live = Live::start(logged(provider.clone(), &store));
    assert!(completed(&live.turn("u1", "first").await));
    let done = live.compact(2, Some("the greeting")).await;
    assert!(
        matches!(
            &done,
            AgentEvent::Compacted {
                trigger: CompactionTrigger::Manual,
                engine: CompactionEngine::Native,
                partial: false,
                converted: false,
                cache_effect: CacheEffect::Rebuilt {
                    reason: RebuildReason::Compaction
                },
                answers: Some(RequestId(2)),
                turn: None,
                usage: Some(_),
                ..
            }
        ),
        "{done:?}"
    );
    let changed = live
        .until(|e| matches!(e, AgentEvent::ContextChanged { .. }))
        .await;
    assert!(matches!(
        changed,
        AgentEvent::ContextChanged { epoch: 1, .. }
    ));
    assert!(completed(&live.turn("u2", "second").await));
    let (agent, seen) = live.close().await;

    let started = seen
        .iter()
        .position(|e| {
            matches!(
                e,
                AgentEvent::CompactionStarted {
                    trigger: CompactionTrigger::Manual,
                    turn: None,
                    ..
                }
            )
        })
        .unwrap();
    assert!(started < seen.iter().position(|e| answers(e, 2)).unwrap());

    let first = provider.body(0);
    let asked = provider.body(1);
    assert!(!provider.beta(0).contains(COMPACTION_BETA));
    assert!(provider.beta(1).contains(COMPACTION_BETA));
    assert_eq!(asked["compaction"]["type"], "summarize");
    let instructions = asked["compaction"]["instructions"].as_str().unwrap();
    assert!(instructions.contains("Focus: the greeting\n"));
    assert!(instructions.ends_with(INSTRUCTIONS_LINE));
    assert!(asked.get("fallbacks").is_none());
    assert_eq!(asked["max_tokens"], 32_000);
    assert_eq!(asked["messages"].as_array().unwrap().len(), 2);
    assert_eq!(
        strip_cache_control(&asked["messages"][0]),
        strip_cache_control(&first["messages"][0])
    );

    let next = provider.body(2);
    assert!(provider.beta(2).contains(COMPACTION_BETA));
    let messages = next["messages"].as_array().unwrap();
    assert_eq!(messages.len(), 1);
    assert_eq!(messages[0]["role"], "user");
    assert_eq!(messages[0]["content"][0], raw_block(SUMMARY));
    let last = messages[0]["content"].as_array().unwrap().last().unwrap();
    assert_eq!(last["text"], "second");

    let head = agent.compaction.head.as_ref().unwrap();
    assert_eq!((head.epoch, head.from, head.native), (1, 2, true));
    assert_eq!(agent.history.len(), 4);
    let text = head_text(&agent);
    assert!(text.starts_with("<compacted_context epoch=\"1\" turns=\"1\u{2013}1\">\n"));
    assert!(text.contains("Turn 1: first\n"));
    assert!(!text.contains("<summary>"));

    let logged = log_events(&store);
    let compacted: Vec<&LogEvent> = logged
        .iter()
        .filter(|e| matches!(e, LogEvent::ContextCompacted { .. }))
        .collect();
    let [
        LogEvent::ContextCompacted {
            turn: None,
            epoch: 1,
            trigger: CompactionTrigger::Manual,
            engine: CompactionEngine::Native,
            focus: Some(focus),
            from: 2,
            summary: None,
            block: Some(block),
            covers: Some(covers),
            usage: Some(_),
            ..
        },
    ] = compacted.as_slice()
    else {
        panic!("{compacted:?}");
    };
    assert_eq!(focus, "the greeting");
    assert_eq!(block, &raw_block(SUMMARY));
    assert_eq!((covers.first_turn, covers.last_turn), (1, 1));
    assert_eq!(covers.first_seq, 0);

    let rebuilt = rebuild_history(logged.iter(), no_images).unwrap();
    assert_eq!(rebuilt.messages, *agent.history());
    assert_eq!(rebuilt.view(), agent.view_messages());
}

#[tokio::test]
async fn the_cached_share_is_unknown_after_a_compaction() {
    let store = MemoryStore::default();
    let provider = scripted(vec![answer("Hello.", 1_000), compaction(SUMMARY)]);
    let mut live = Live::start(logged(provider, &store));
    assert!(completed(&live.turn("u1", "first").await));
    live.compact(2, None).await;
    let (agent, _) = live.close().await;
    assert_eq!(agent.cached(), None);
}

#[tokio::test]
async fn the_system_and_tools_never_change_across_a_compaction() {
    for threads in [false, true] {
        let provider = scripted(vec![
            answer("Hello.", 1_000),
            answer("Fine.", 1_100),
            compaction(SUMMARY),
            answer("Again.", 300),
            answer("More.", 400),
        ])
        .view(|k| {
            k.threads = threads;
            k.wire_print = threads;
        });
        let mut live = Live::start(Agent::new(provider.clone(), default_session()));
        live.turn("u1", "first").await;
        live.turn("u2", "second").await;
        live.compact(3, None).await;
        live.turn("u3", "third").await;
        live.turn("u4", "fourth").await;
        live.close().await;
        let bodies: Vec<Value> = provider
            .sent()
            .iter()
            .map(|sent| {
                let mut request = sent.request();
                request.features.thread = None;
                strip_cache_control(&body_of(&request))
            })
            .collect();
        for body in &bodies[1..] {
            assert_eq!(body["system"], bodies[0]["system"]);
            assert_eq!(body.get("tools"), bodies[0].get("tools"));
        }
        let extends = |before: &Value, after: &Value| {
            let before = before["messages"].as_array().unwrap();
            let after = after["messages"].as_array().unwrap();
            after.len() > before.len() && after[..before.len()] == before[..]
        };
        assert!(extends(&bodies[0], &bodies[1]));
        assert!(extends(&bodies[1], &bodies[2]));
        assert!(extends(&bodies[3], &bodies[4]));
        assert_eq!(
            bodies[3]["messages"][0]["content"][0],
            bodies[4]["messages"][0]["content"][0]
        );
    }
}

#[tokio::test]
async fn nothing_to_compact_is_refused_before_a_turn_and_right_after_a_compaction() {
    let provider = scripted(vec![answer("Hello.", 1_000), compaction(SUMMARY)]);
    let mut live = Live::start(Agent::new(provider.clone(), default_session()));
    let refused = live.compact(1, None).await;
    assert!(
        matches!(
            &refused,
            AgentEvent::Rejected {
                kind: Some(RejectKind::NothingToCompact),
                ..
            }
        ),
        "{refused:?}"
    );
    live.turn("u1", "first").await;
    assert!(matches!(
        live.compact(2, None).await,
        AgentEvent::Compacted { .. }
    ));
    let again = live.compact(3, None).await;
    assert!(
        matches!(
            &again,
            AgentEvent::Rejected {
                kind: Some(RejectKind::NothingToCompact),
                ..
            }
        ),
        "{again:?}"
    );
    live.close().await;
    assert_eq!(provider.sent().len(), 2);
}

#[tokio::test]
async fn a_model_without_native_compaction_compacts_with_wisps_engine() {
    let provider = scripted(vec![answer("Hello.", 1_000), answer(WRITTEN, 60_000)]);
    let agent = Agent::new(provider.clone(), default_session()).with_model_settings(models(WISP));
    let mut live = Live::start(agent);
    live.turn("u1", "first").await;
    let done = live.compact(2, None).await;
    assert!(
        matches!(
            &done,
            AgentEvent::Compacted {
                engine: CompactionEngine::Wisp,
                ..
            }
        ),
        "{done:?}"
    );
    live.close().await;
    assert_eq!(provider.sent().len(), 2);
    assert!(provider.body(1).get("compaction").is_none());
}

#[tokio::test]
async fn while_compacting_a_message_waits_and_other_commands_are_refused() {
    let store = MemoryStore::default();
    let provider = scripted(vec![
        answer("Hello.", 1_000),
        Step::hang(),
        answer("Next.", 300),
    ]);
    let mut live = Live::start(logged(provider.clone(), &store));
    live.turn("u1", "first").await;
    live.send(compact(2, None)).await;
    live.until(|e| matches!(e, AgentEvent::CompactionStarted { .. }))
        .await;
    live.send(send("u2", "waiting")).await;
    let accepted = live
        .until(|e| matches!(e, AgentEvent::InputAccepted { .. }))
        .await;
    assert!(
        matches!(accepted, AgentEvent::InputAccepted { front: false, .. }),
        "{accepted:?}"
    );
    let second = live.compact(3, None).await;
    assert!(
        matches!(
            second,
            AgentEvent::Rejected {
                kind: Some(RejectKind::Compacting),
                ..
            }
        ),
        "{second:?}"
    );
    live.send(ClientCommand::Answer {
        call_id: "q1".into(),
        answers: vec![Answer::skipped()],
    })
    .await;
    let no_question = live
        .until(|e| matches!(e, AgentEvent::Rejected { .. }))
        .await;
    assert!(
        matches!(
            no_question,
            AgentEvent::Rejected {
                kind: Some(RejectKind::NoQuestion),
                ..
            }
        ),
        "{no_question:?}"
    );
    live.send(ClientCommand::Cancel).await;
    let cancelled = live.until(|e| answers(e, 2)).await;
    assert!(
        matches!(
            &cancelled,
            AgentEvent::CompactionFailed {
                kind: CompactionFailure::Cancelled,
                engine: Some(CompactionEngine::Native),
                turn: None,
                ..
            }
        ),
        "{cancelled:?}"
    );
    let outcome = live
        .until(|e| matches!(e, AgentEvent::TurnEnded { .. }))
        .await;
    assert!(matches!(
        outcome,
        AgentEvent::TurnEnded {
            outcome: TurnOutcome::Completed { .. },
            ..
        }
    ));
    let (agent, _) = live.close().await;
    assert!(agent.compaction.head.is_none());
    assert_eq!(agent.history.len(), 4);
    assert!(log_events(&store).iter().any(|e| matches!(
        e,
        LogEvent::CompactionFailed {
            turn: None,
            kind: CompactionFailure::Cancelled,
            ..
        }
    )));
}

#[tokio::test]
async fn a_compact_during_a_turn_is_refused() {
    let provider = scripted(vec![Step::hang()]);
    let mut live = Live::start(Agent::new(provider, default_session()));
    live.send(send("u1", "first")).await;
    live.until(|e| matches!(e, AgentEvent::StepStarted { .. }))
        .await;
    let refused = live.compact(2, None).await;
    assert!(
        matches!(
            refused,
            AgentEvent::Rejected {
                kind: Some(RejectKind::TurnRunning),
                ..
            }
        ),
        "{refused:?}"
    );
    live.send(ClientCommand::Cancel).await;
    live.until(|e| matches!(e, AgentEvent::TurnEnded { .. }))
        .await;
    live.close().await;
}

struct Held {
    gate: Semaphore,
}

impl SettingsHost for Held {
    fn save(&self, _edits: Vec<SiteEdit>) -> BoxFuture<'_, Result<SavedFile, HostError>> {
        async move {
            drop(self.gate.acquire().await);
            Err(HostError("held".into()))
        }
        .boxed()
    }

    fn read(&self) -> BoxFuture<'_, Result<SavedRead, HostError>> {
        async move {
            Ok(SavedRead {
                often: Vec::new(),
                saved: Vec::new(),
                file: FileState::Ok {
                    path_display: "config.toml".into(),
                },
            })
        }
        .boxed()
    }
}

#[tokio::test]
async fn a_queued_compact_is_cancelled_by_cancel_and_by_closing() {
    let host = Arc::new(Held {
        gate: Semaphore::new(0),
    });
    let provider = scripted(vec![answer("Hello.", 1_000)]);
    let agent = Agent::new(provider.clone(), default_session()).with_settings_host(host.clone());
    let mut live = Live::start(agent);
    live.turn("u1", "first").await;
    live.send(ClientCommand::ChangeSettings {
        request: RequestId(9),
        changes: vec![SettingChange::named("effort", SettingValue::Text("low".into())).unwrap()],
        save: true,
    })
    .await;
    live.send(compact(2, None)).await;
    live.send(ClientCommand::Cancel).await;
    let cancelled = live.until(|e| answers(e, 2)).await;
    assert!(
        matches!(
            &cancelled,
            AgentEvent::CompactionFailed {
                kind: CompactionFailure::Cancelled,
                engine: None,
                usage: None,
                cost: None,
                ..
            }
        ),
        "{cancelled:?}"
    );
    live.send(compact(3, None)).await;
    drop(live.commands.take());
    let closed = live.until(|e| answers(e, 3)).await;
    assert!(
        matches!(
            closed,
            AgentEvent::CompactionFailed {
                kind: CompactionFailure::Cancelled,
                ..
            }
        ),
        "{closed:?}"
    );
    host.gate.add_permits(4);
    live.close().await;
    assert_eq!(provider.sent().len(), 1);
}

#[tokio::test]
async fn a_rejected_beta_without_a_block_falls_back_to_wisps_engine() {
    let provider = scripted(vec![
        answer("Hello.", 1_000),
        beta_refused(),
        answer(WRITTEN, 60_000),
    ]);
    let mut live = Live::start(Agent::new(provider.clone(), default_session()));
    live.turn("u1", "first").await;
    let done = live.compact(2, None).await;
    assert!(
        matches!(
            &done,
            AgentEvent::Compacted {
                engine: CompactionEngine::Wisp,
                ..
            }
        ),
        "{done:?}"
    );
    let (agent, seen) = live.close().await;
    assert!(seen.iter().any(|e| matches!(
        e,
        AgentEvent::FeatureStateChanged {
            feature: wisp_protocol::Feature::NativeCompaction,
            state: FeatureState::FallenBack,
            ..
        }
    )));
    assert!(!agent.compaction.head.as_ref().unwrap().native);
    assert_eq!(provider.sent().len(), 3);
    assert!(!provider.beta(2).contains(COMPACTION_BETA));
    assert!(provider.body(2).get("compaction").is_none());
    assert!(
        !seen
            .iter()
            .any(|e| matches!(e, AgentEvent::CompactionFailed { .. }))
    );
}

#[tokio::test]
async fn a_refused_block_converts_a_manual_compact_with_no_further_request() {
    let store = MemoryStore::default();
    let provider = scripted(vec![
        answer("Hello.", 1_000),
        compaction(SUMMARY),
        answer("Again.", 300),
        block_refused(),
        answer("Third.", 200),
    ]);
    let mut live = Live::start(logged(provider.clone(), &store));
    live.turn("u1", "first").await;
    live.compact(2, None).await;
    live.turn("u2", "second").await;
    let converted = live.compact(4, Some("the greeting")).await;
    assert!(
        matches!(
            &converted,
            AgentEvent::Compacted {
                trigger: CompactionTrigger::Manual,
                engine: CompactionEngine::Wisp,
                partial: true,
                converted: true,
                answers: Some(RequestId(4)),
                summary: Some(summary),
                ..
            } if summary == SUMMARY
        ),
        "{converted:?}"
    );
    live.turn("u3", "third").await;
    let (agent, seen) = live.close().await;
    assert!(
        !seen
            .iter()
            .any(|e| matches!(e, AgentEvent::CompactionFailed { .. }))
    );
    assert_eq!(provider.sent().len(), 5);
    let next = provider.body(4);
    assert!(!provider.beta(4).contains(COMPACTION_BETA));
    let first = next["messages"][0]["content"][0]["text"].as_str().unwrap();
    assert!(first.starts_with("<compacted_context epoch=\"2\""));
    assert!(first.contains(&format!("<summary>\n{SUMMARY}\n</summary>")));
    let head = agent.compaction.head.as_ref().unwrap();
    assert_eq!((head.epoch, head.from, head.native), (2, 4, false));

    let logged = log_events(&store);
    let manual: Vec<&LogEvent> = logged
        .iter()
        .filter(|e| matches!(e, LogEvent::ContextCompacted { epoch: 2, .. }))
        .collect();
    let [
        LogEvent::ContextCompacted {
            trigger: CompactionTrigger::Manual,
            engine: CompactionEngine::Wisp,
            partial: true,
            summary: Some(summary),
            block: None,
            ..
        },
    ] = manual.as_slice()
    else {
        panic!("{manual:?}");
    };
    assert_eq!(summary, SUMMARY);
    let rebuilt = rebuild_history(logged.iter(), no_images).unwrap();
    assert_eq!(rebuilt.messages, *agent.history());
    assert_eq!(rebuilt.view(), agent.view_messages());
}

#[tokio::test]
async fn a_switch_whose_endpoint_cant_take_the_block_converts_before_the_turn() {
    let store = MemoryStore::default();
    let provider = scripted(vec![
        answer("Hello.", 1_000),
        compaction(SUMMARY),
        answer("Other.", 300),
    ]);
    let agent = two_endpoints(logged(provider.clone(), &store), provider.clone())
        .with_model_settings(models(
            "[endpoints.other.models.\"claude-opus-5-5\"]\nnative_compaction = false\n",
        ));
    let mut live = Live::start(agent);
    live.turn("u1", "first").await;
    live.compact(2, None).await;
    live.send(ClientCommand::shortcut(
        RequestId(3),
        Some("other".into()),
        None,
        None,
    ))
    .await;
    live.until(|e| {
        matches!(
            e,
            AgentEvent::SettingsChanged {
                answers: Some(RequestId(3)),
                ..
            }
        )
    })
    .await;
    assert!(completed(&live.turn("u2", "second").await));
    let (agent, seen) = live.close().await;
    let converted = seen
        .iter()
        .position(|e| {
            matches!(
                e,
                AgentEvent::Compacted {
                    trigger: CompactionTrigger::Convert,
                    engine: CompactionEngine::Wisp,
                    turn: Some(2),
                    partial: false,
                    converted: true,
                    summary: Some(summary),
                    ..
                } if summary == SUMMARY
            )
        })
        .unwrap();
    let started = seen
        .iter()
        .position(|e| matches!(e, AgentEvent::TurnStarted { turn: 2, .. }))
        .unwrap();
    assert!(converted < started);
    assert!(!seen.iter().any(|e| matches!(
        e,
        AgentEvent::CompactionStarted {
            trigger: CompactionTrigger::Switch,
            ..
        }
    )));
    assert_eq!(provider.sent().len(), 3);
    assert!(!provider.beta(2).contains(COMPACTION_BETA));
    let messages = provider.body(2)["messages"].clone();
    assert_eq!(messages.as_array().unwrap().len(), 1);
    assert!(
        messages[0]["content"][0]["text"]
            .as_str()
            .unwrap()
            .contains(SUMMARY)
    );
    let logged = log_events(&store);
    let convert = logged
        .iter()
        .position(|e| {
            matches!(
                e,
                LogEvent::ContextCompacted {
                    trigger: CompactionTrigger::Convert,
                    turn: Some(2),
                    ..
                }
            )
        })
        .unwrap();
    let turn = logged
        .iter()
        .position(|e| matches!(e, LogEvent::TurnStarted { turn: 2, .. }))
        .unwrap();
    assert!(convert < turn);
    let rebuilt = rebuild_history(logged.iter(), no_images).unwrap();
    assert_eq!(rebuilt.messages, *agent.history());
    assert_eq!(rebuilt.view(), agent.view_messages());
}

#[tokio::test]
async fn a_turn_refused_for_the_beta_converts_and_sends_again_once() {
    let store = MemoryStore::default();
    let provider = scripted(vec![
        answer("Hello.", 1_000),
        compaction(SUMMARY),
        beta_refused(),
        answer("Other.", 300),
    ]);
    let agent = two_endpoints(logged(provider.clone(), &store), provider.clone());
    let mut live = Live::start(agent);
    live.turn("u1", "first").await;
    live.compact(2, None).await;
    live.send(ClientCommand::shortcut(
        RequestId(3),
        Some("other".into()),
        None,
        None,
    ))
    .await;
    live.until(|e| {
        matches!(
            e,
            AgentEvent::SettingsChanged {
                answers: Some(RequestId(3)),
                ..
            }
        )
    })
    .await;
    assert!(completed(&live.turn("u2", "second").await));
    let (agent, seen) = live.close().await;
    assert!(seen.iter().any(|e| matches!(
        e,
        AgentEvent::FeatureStateChanged {
            feature: wisp_protocol::Feature::NativeCompaction,
            state: FeatureState::FallenBack,
            ..
        }
    )));
    assert!(seen.iter().any(|e| matches!(
        e,
        AgentEvent::Compacted {
            trigger: CompactionTrigger::Convert,
            turn: Some(2),
            converted: true,
            summary: Some(summary),
            ..
        } if summary == SUMMARY
    )));
    assert_eq!(provider.sent().len(), 4);
    assert!(provider.beta(2).contains(COMPACTION_BETA));
    assert_eq!(
        provider.body(2)["messages"][0]["content"][0],
        raw_block(SUMMARY)
    );
    assert!(!provider.beta(3).contains(COMPACTION_BETA));
    let resent = &provider.body(3)["messages"][0]["content"];
    assert!(resent[0]["text"].as_str().unwrap().contains(SUMMARY));
    assert_eq!(resent.as_array().unwrap().last().unwrap()["text"], "second");
    let rebuilt = rebuild_history(log_events(&store).iter(), no_images).unwrap();
    assert_eq!(rebuilt.messages, *agent.history());
    assert_eq!(rebuilt.view(), agent.view_messages());
}

#[tokio::test]
async fn records_between_turns_are_logged_without_a_turn() {
    let store = MemoryStore::default();
    let unknown = ProviderEvent::Unknown {
        provider: "anthropic".into(),
        raw: json!({"type": "brand_new"}),
    };
    let transformed = ProviderEvent::InputTransformations {
        provider: "anthropic".into(),
        transformations: serde_json::from_value(json!([{"type": "thinking_dropped"}])).unwrap(),
    };
    let provider = scripted(vec![
        answer("Hello.", 10_000),
        overloaded(),
        summarised(SUMMARY, 12_000, 0, vec![unknown, transformed]),
    ]);
    let agent = logged(provider.clone(), &store).with_retry_policy(crate::agent::retry::quick());
    let mut live = Live::start(agent);
    live.turn("u1", "first").await;
    assert!(matches!(
        live.compact(2, None).await,
        AgentEvent::Compacted { .. }
    ));
    live.close().await;
    let logged = log_events(&store);
    assert!(logged.iter().any(|e| matches!(
        e,
        LogEvent::Retrying {
            turn: None,
            step: None,
            ..
        }
    )));
    assert!(
        logged
            .iter()
            .any(|e| matches!(e, LogEvent::UnknownProviderEvent { turn: None, .. }))
    );
    assert!(
        logged
            .iter()
            .any(|e| matches!(e, LogEvent::InputTransformations { turn: None, .. }))
    );
    assert!(
        logged
            .iter()
            .any(|e| matches!(e, LogEvent::CacheMiss { turn: None, .. }))
    );
}

#[tokio::test]
async fn the_context_report_counts_compactions_after_one() {
    let provider = scripted(vec![answer("Hello.", 1_000), compaction(SUMMARY)]);
    let mut live = Live::start(Agent::new(provider, default_session()));
    live.turn("u1", "first").await;
    live.compact(2, None).await;
    let (agent, _) = live.close().await;
    let report = agent.report(ReportDetail::Summary, RequestId(5));
    assert_eq!(report.epoch, 1);
    assert_eq!(report.compactions, 1);
    assert!(report.compaction_cost_usd.is_some_and(|usd| usd > 0.0));
    assert!(matches!(
        report.fill,
        Fill::EstimatedAfterCompaction { epoch: 1, .. }
    ));
    assert!(
        report
            .categories
            .iter()
            .any(|c| c.category == Category::Summary && c.tokens > 0)
    );
    assert!(
        !report
            .categories
            .iter()
            .any(|c| c.category == Category::Replies && c.tokens > 0)
    );
}

pub(super) fn wisp_layout(
    agent: &Agent,
    (epoch, turns): (u64, (u64, u64)),
    summary: Option<&str>,
) -> String {
    let view = agent.history_view();
    let compact = agent.convert_compact_info(CompactionTrigger::Manual);
    let info = agent.strategy_info(compact, (epoch, turns), false);
    wisp_plugin_compaction::compacted_context(&wisp_plugin_compaction::Layout {
        info: &info,
        summary,
        said: &view.said,
        calls: &view.calls,
        running: None,
        pointers: wisp_plugin_compaction::pointers(view.log_pointer, view.output_pointer),
    })
}

#[test]
fn a_refused_calls_ledger_line_names_its_keys_and_never_a_value() {
    let provider = scripted(vec![]);
    let mut agent = Agent::new(provider, default_session());
    agent.set_toolbox(crate::tool::test_toolbox());
    let marker = "marker-4be1";
    let input = json!({"id": 2, "input": marker});
    assert_eq!(agent.target_of("c1", "nope", &input), "{id, input}");
    agent.note_event(
        &AgentEvent::ToolCallRequested {
            step: 1,
            block: 0,
            call_id: "c1".into(),
            tool: "nope".into(),
            input: input.clone(),
            display: futures::executor::block_on(agent.toolbox.rejected_display("nope", &input)),
            refused: true,
        },
        None,
    );
    agent.note_event(
        &AgentEvent::call_finished(
            "c1",
            ToolOutcome::InvalidInput {
                message: "there is no tool named `nope`".into(),
            },
        ),
        None,
    );
    let text = wisp_layout(&agent, (1, (1, 1)), Some("Short."));
    assert!(
        text.contains("- turn 0: nope {id, input} \u{b7} "),
        "{text}"
    );
    assert!(!text.contains(marker), "{text}");
    let span_key = format!("{}sk-ant-CANARY0123456789012345", "k.".repeat(33));
    let scrubbed = agent.target_of("c2", "nope", &json!({span_key: "val"}));
    assert_eq!(scrubbed, "{}");
    assert!(!scrubbed.contains("sk-ant-") && !scrubbed.contains("CANARY"));
    assert_eq!(
        agent.target_of("c3", "sk-ant-CANARY0123456789012345", &json!({"id": 1})),
        "{}"
    );
    crate::session::register_secrets(["CANARY-held-mcp-secret-token".to_owned()]);
    agent.hooks.host = Some(Arc::new(crate::plugin::KernelHost::new()));
    assert!(agent.hidden_mcp_name("mcp__local__CANARY-held-mcp-secret-token"));
    assert_eq!(
        agent.target_of(
            "c4",
            "mcp__local__CANARY-held-mcp-secret-token",
            &json!({"id": 1})
        ),
        "{}"
    );
}

#[test]
fn a_calls_target_is_the_one_its_request_showed() {
    let provider = scripted(vec![]);
    let mut agent = Agent::new(provider, default_session());
    agent.set_toolbox(crate::tool::test_toolbox());
    let input = json!({"text": "hello"});
    let mut display = crate::tool::fallback_display("echo", &input);
    display.target = "shown when requested".into();
    agent.note_event(
        &AgentEvent::ToolCallRequested {
            step: 1,
            block: 0,
            call_id: "c1".into(),
            tool: "echo".into(),
            input: input.clone(),
            display,
            refused: false,
        },
        None,
    );
    assert_eq!(
        agent.target_of("c1", "echo", &input),
        "shown when requested"
    );
    assert_eq!(
        agent.target_of("c2", "echo", &input),
        crate::tool::fallback_display("echo", &input).target
    );
}

#[test]
fn the_head_keeps_recent_words_and_the_ledger() {
    let provider = scripted(vec![]);
    let mut agent = Agent::new(provider, default_session()).with_model_settings(models(
        "[models.\"claude-opus-5-5\"]\nverbatim_user_tokens = 10\n",
    ));
    agent.note_opening(1, vec!["an old message".into()], Some(3));
    agent.note_inputs(1, vec![], Some(4));
    agent.note_opening(2, vec!["x".repeat(60)], Some(9));
    agent.note_inputs(2, vec!["   ".into()], Some(10));
    for n in 0..(LEDGER_CALLS + 2) {
        agent.note_event(
            &AgentEvent::ToolCallRequested {
                step: 1,
                block: 0,
                call_id: format!("c{n}"),
                tool: "bash".into(),
                input: json!({}),
                display: ToolDisplay {
                    action: ToolAction::Run,
                    verbs: None,
                    target: format!("step-{n:02}"),
                    output: wisp_protocol::OutputKind::Text,
                    view: None,
                    title: None,
                    purpose: ToolPurpose::Run,
                },
                refused: false,
            },
            None,
        );
    }
    let last = format!("c{}", LEDGER_CALLS + 1);
    agent.note_event(&AgentEvent::call_started(last.clone(), "~/p"), None);
    agent.note_event(
        &AgentEvent::call_finished(last, ToolOutcome::Cancelled),
        None,
    );
    let text = wisp_layout(&agent, (1, (1, 2)), Some("Short."));
    assert!(text.contains("<summary>\nShort.\n</summary>\n"));
    let cut = format!(
        "Turn 2: {} [cut here; the whole message is record 9 in the session log]",
        "x".repeat(30)
    );
    assert!(text.contains(&cut), "{text}");
    assert!(!text.contains("an old message"));
    assert_eq!(text.matches("bash step-").count(), LEDGER_CALLS);
    assert!(!text.contains("step-00") && !text.contains("step-01"));
    assert!(text.contains("- turn 0: bash step-02 \u{b7} no result\n"));
    assert!(text.contains(&format!(
        "- turn 0: bash step-{} \u{b7} cancelled \u{b7} in ~/p\n",
        LEDGER_CALLS + 1
    )));
}

#[tokio::test]
async fn manual_and_background_compaction_publish_passive_allowance_without_logging_it() {
    use crate::clock::{Clock, StillClock};
    use crate::plan_usage::{Display, PlanUsageCache};
    use wisp_protocol::plan_usage::{
        Percent, PlanUsage, UsageAction, UsageScope, UsageSource, UsageWindow,
    };
    let now = StillClock.now().as_second();
    for background_run in [false, true] {
        let store = MemoryStore::default();
        let observation = ProviderEvent::PlanUsage(Ok(PlanUsage::new(
            None,
            vec![UsageWindow {
                key: "codex.primary".into(),
                label: "5-hour".into(),
                duration_seconds: Some(18_000),
                scope: UsageScope::All,
                used_percent: Some(Percent::try_from(96.0).unwrap()),
                resets_at: Some(now + 3600),
                observed_at: now,
                source: UsageSource::ResponseHeader,
            }],
        )
        .unwrap()));
        let mut summary = summarised(SUMMARY, 1_200, 0, vec![]);
        summary.script.insert(0, Ok(observation));
        let provider = documented(vec![
            answer(
                "Hello.",
                if background_run {
                    background::NEAR
                } else {
                    10_000
                },
            ),
            summary,
        ]);
        let agent = if background_run {
            background::background(&provider, &store)
        } else {
            logged(provider, &store)
        }
        .with_clock(Arc::new(StillClock))
        .with_plan_usage(PlanUsageCache::default(), Display::Enabled);
        let mut live = Live::start(agent);
        live.send(ClientCommand::PlanUsage {
            request: RequestId(100),
            endpoint: "env".into(),
            action: UsageAction::Subscribe,
        })
        .await;
        live.until(
            |event| matches!(event, AgentEvent::PlanUsageChanged { state, .. } if !state.loading),
        )
        .await;
        live.turn("u1", "first").await;
        if background_run {
            live.until(prepared).await;
        } else {
            live.compact(2, None).await;
        }
        let has_observation = |event: &AgentEvent| matches!(event, AgentEvent::PlanUsageChanged { state, .. } if state.snapshot.is_some());
        while live.seen.iter().filter(|e| has_observation(e)).count() < 2 {
            live.until(has_observation).await;
        }
        live.close().await;
        assert!(
            !serde_json::to_string(&log_events(&store))
                .unwrap()
                .contains("codex.primary")
        );
    }
}

#[tokio::test]
async fn manual_and_background_compaction_hold_used_up_limits() {
    use crate::clock::{Clock, StillClock};
    use crate::plan_usage::{Display, PlanUsageCache, Room};
    let now = StillClock.now().as_second();
    for background_run in [false, true] {
        let store = MemoryStore::default();
        let cache = PlanUsageCache::default();
        let mut error = classified(
            ErrorKind::RateLimit,
            "quota used up",
            FailurePhase::Response,
            RetryClass::Never,
        );
        crate::provider::limit::reached(
            &mut error.detail,
            now,
            None,
            wisp_protocol::plan_usage::UsageSource::ErrorBody,
            None,
        );
        let provider = documented(vec![
            answer(
                "Hello.",
                if background_run {
                    background::NEAR
                } else {
                    10_000
                },
            ),
            failing(error),
        ]);
        let agent = if background_run {
            background::background(&provider, &store)
        } else {
            logged(provider, &store)
        }
        .with_clock(Arc::new(StillClock))
        .with_plan_usage(cache.clone(), Display::Disabled);
        let mut live = Live::start(agent);
        assert!(completed(&live.turn("u1", "first").await));
        if background_run {
            live.until(|event| {
                matches!(
                    event,
                    AgentEvent::CompactionFailed {
                        trigger: CompactionTrigger::Background,
                        ..
                    }
                )
            })
            .await;
        } else {
            assert!(matches!(
                live.compact(2, None).await,
                AgentEvent::CompactionFailed { .. }
            ));
        }
        assert_eq!(
            cache.room("env", DEFAULT_MODEL, now),
            Room::Used {
                model: None,
                cause: wisp_tool_text::agents::Cause::Limit,
                resets_at: None
            }
        );
        live.close().await;
    }
}

fn bad_request() -> Step {
    failing(classified(
        ErrorKind::InvalidRequest,
        "bad request",
        FailurePhase::Response,
        RetryClass::Never,
    ))
}

fn wisp_engine(provider: &Arc<Fake>, store: &MemoryStore) -> Agent {
    logged(provider.clone(), store).with_model_settings(models(WISP))
}

fn ends_of(store: &MemoryStore, id: u32) -> Vec<ProcessEnd> {
    log_events(store)
        .into_iter()
        .filter_map(|e| match e {
            LogEvent::ProcessEnded { id: i, end, .. } if i == id => Some(end),
            _ => None,
        })
        .collect()
}

fn ended_completed(event: &AgentEvent) -> bool {
    matches!(
        event,
        AgentEvent::TurnEnded {
            outcome: TurnOutcome::Completed { .. },
            ..
        }
    )
}

fn rebuilt(store: &MemoryStore) -> Vec<Message> {
    let records = store.read("s1").unwrap();
    crate::session::rebuild_checked(store, &records, None)
        .unwrap()
        .unwrap()
        .messages
}

fn small_other(store: &MemoryStore, provider: &Arc<Fake>) -> Agent {
    two_endpoints(logged(provider.clone(), store), provider.clone()).with_model_settings(models(
        "[endpoints.other.models.\"claude-opus-5-5\"]\nnative_compaction = false\ncontext_window = 104000\n",
    ))
}

fn retrying(seen: &[AgentEvent]) -> usize {
    seen.iter()
        .filter(|e| matches!(e, AgentEvent::Retrying { .. }))
        .count()
}

fn on(provider: &Arc<Fake>, store: &MemoryStore, config: &str) -> Agent {
    logged(provider.clone(), store).with_model_settings(models(config))
}

fn position(seen: &[AgentEvent], pick: impl Fn(&AgentEvent) -> bool) -> usize {
    seen.iter().position(pick).unwrap()
}

fn assert_rebuilds(store: &MemoryStore, agent: &Agent) {
    let rebuilt = rebuild_history(log_events(store).iter(), no_images).unwrap();
    assert_eq!(rebuilt.messages, *agent.history());
    assert_eq!(rebuilt.view(), agent.view_messages());
}

fn offered(outcome: &TurnOutcome) -> bool {
    matches!(outcome, TurnOutcome::Error { offer: Some(_), .. })
}

fn background_started(event: &AgentEvent) -> bool {
    matches!(
        event,
        AgentEvent::CompactionStarted {
            trigger: CompactionTrigger::Background,
            ..
        }
    )
}

async fn three_turns(live: &mut Live) {
    assert!(completed(&live.turn("u1", "first").await));
    assert!(completed(&live.turn("u2", "second").await));
    assert!(completed(&live.turn("u3", "third").await));
}

fn engine(native: bool, provider: &Arc<Fake>, store: &MemoryStore) -> Agent {
    if native {
        logged(provider.clone(), store)
    } else {
        wisp_engine(provider, store)
    }
}

#[tokio::test]
async fn a_server_card_and_its_answer_never_join_the_compaction_head() {
    for asked_by in [
        wisp_protocol::AskedBy::Server {
            name: "linear".into(),
        },
        wisp_protocol::AskedBy::Unknown,
    ] {
        let mut agent = Agent::new(scripted(vec![]), default_session());
        agent.turns = 1;
        let question = wisp_protocol::Question {
            header: "Needs you".into(),
            question: "Private server question".into(),
            options: Vec::new(),
            multi_select: false,
            source: Some(wisp_protocol::QuestionSource {
                asked_by,
                typed: true,
            }),
        };
        agent.note_event(
            &AgentEvent::QuestionAsked {
                call_id: "server".into(),
                questions: vec![question.clone()],
            },
            None,
        );
        agent.note_event(
            &AgentEvent::QuestionClosed {
                call_id: "server".into(),
                how: wisp_protocol::QuestionEnd::Answered {
                    answers: vec![Answer::own("Private typed words")],
                },
            },
            None,
        );
        assert!(agent.compaction.said.is_empty());
        let mut ordinary = question;
        ordinary.source = None;
        ordinary.question = "Ordinary question".into();
        agent.note_event(
            &AgentEvent::QuestionAsked {
                call_id: "ordinary".into(),
                questions: vec![ordinary],
            },
            None,
        );
        agent.note_event(
            &AgentEvent::QuestionClosed {
                call_id: "ordinary".into(),
                how: wisp_protocol::QuestionEnd::Answered {
                    answers: vec![Answer::own("Ordinary answer")],
                },
            },
            None,
        );
        let head = wisp_layout(&agent, (1, (1, 1)), Some("Summary"));
        assert!(head.contains("Ordinary answer"));
        assert!(!head.contains("Private server question") && !head.contains("Private typed words"));
    }
}

#[tokio::test]
async fn a_fresh_compaction_head_rechecks_manifest_lookup_targets_after_switch_off_and_replay() {
    use crate::plugin::{PluginConfig, PluginSwitches, Registry};
    use wisp_plugin_api::manifest::Origin;
    const HELD: &str = "CANARY-late-compaction-lookup-target";
    let scratch = crate::test_guard::ScratchDir::new("wisp-ledger-", "lookup");
    let workspace = scratch.workspace();
    let path = workspace
        .home
        .as_ref()
        .unwrap()
        .join("fixture.txt")
        .display()
        .to_string();
    let new_agent =
        || Agent::new(scripted(vec![]), default_session()).with_workspace(workspace.clone());
    let bundle = wisp_plugin_search::bundled();
    let (registry, problems) = Registry::new(
        vec![bundle, wisp_plugin_view_file::bundled()],
        &PluginConfig::default(),
    );
    assert!(problems.is_empty());
    let registry = Arc::new(registry);
    let mut agent = new_agent();
    agent.hooks.registry = Some(registry.clone());
    let kernel = Arc::new(crate::plugin::KernelHost::new());
    let host =
        crate::plugin::PluginHost::new("search", "0.4.0", &[], kernel).origin(Origin::Bundled);
    let search = (bundle.instantiate)(Arc::new(host));
    let query = json!({"query": HELD});
    let prepared = search.prepare("search", &query.to_string()).await.unwrap();
    let search_display = crate::plugin::convert::display(prepared.display()).unwrap();
    let mut open_display = crate::tool::fallback_display("use", &Value::Null);
    open_display.target = HELD.into();
    let mut file_display = crate::tool::fallback_display("view_file", &Value::Null);
    file_display.target = path.clone();
    let records = [
        ("fixture-search", "search", query.clone(), search_display),
        ("fixture-open", "use", json!({"name": HELD}), open_display),
        (
            "fixture-file",
            "view_file",
            json!({"path":path}),
            file_display,
        ),
    ]
    .map(
        |(call_id, tool, input, display)| LogEvent::ToolCallRequested {
            turn: 1,
            step: 1,
            block: 0,
            call_id: call_id.into(),
            tool: tool.into(),
            input,
            display,
            refused: false,
        },
    );
    let record = |agent: &mut Agent, records: &[LogEvent]| {
        for (seq, event) in records.iter().enumerate() {
            let (_, event) = crate::resume::replayed(event).unwrap();
            agent.note_event(&event, Some(seq as u64));
        }
    };
    record(&mut agent, &records);
    assert_eq!(
        wisp_layout(&agent, (1, (1, 1)), None).matches(HELD).count(),
        2
    );
    agent.hooks.registry = None;
    assert!(
        agent
            .target_of("fixture-search", "search", &query)
            .contains(HELD)
    );
    assert!(!agent.lookup_is_safe(&path));
    let check = |agent: &Agent| {
        let head = wisp_layout(agent, (2, (1, 1)), None);
        assert!(!head.contains(HELD));
        assert_eq!(
            head.matches(wisp_tool_text::index::INVALID_TARGET).count(),
            2
        );
        assert!(head.contains(&path));
    };
    let config = PluginConfig {
        switches: PluginSwitches {
            removed: ["search".to_owned()].into_iter().collect(),
            ..Default::default()
        },
        ..PluginConfig::default()
    };
    let (removed, problems) =
        Registry::new(vec![bundle, wisp_plugin_view_file::bundled()], &config);
    assert!(problems.is_empty());
    let mut resumed = new_agent();
    resumed.hooks.registry = Some(Arc::new(removed));
    record(&mut resumed, &records);
    let (without_lookup, problems) = Registry::new(
        vec![wisp_plugin_view_file::bundled()],
        &PluginConfig::default(),
    );
    assert!(problems.is_empty());
    assert_eq!(without_lookup.lookup_tool("search"), None);
    assert_eq!(without_lookup.lookup_tool("use"), None);
    let mut absent = new_agent();
    absent.hooks.registry = Some(Arc::new(without_lookup));
    record(&mut absent, &records);
    let tools = crate::tool::testkit::bundled_exact(&["view_file", "search", "use"]);
    assert_eq!(tools.lookup_tool("search"), Some(true));
    assert_eq!(tools.lookup_tool("use"), Some(true));
    assert_eq!(tools.lookup_tool("view_file"), Some(false));
    let mut toolbox_only =
        Agent::new(scripted(vec![]), default_session()).with_tools(tools, workspace.clone());
    record(&mut toolbox_only, &records);
    assert!(absent.lookup_is_safe(HELD));
    crate::session::register_secrets([HELD.to_owned()]);
    assert!(!absent.lookup_is_safe(HELD));
    for view in [&agent, &resumed, &absent, &toolbox_only] {
        check(view);
    }
}
