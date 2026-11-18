use std::sync::{Arc, Mutex};
use std::time::Duration;

use futures::stream::BoxStream;
use serde_json::{Value, json};
use tokio::sync::mpsc;
use wisp_protocol::log::{FeatureState, LogEvent, no_images, rebuild_history};
use wisp_protocol::settings::{Layer, SettingName, SettingTarget, SettingValue, When};
use wisp_protocol::{
    AgentEvent, CacheEffect, ClientCommand, ContentBlock, Effort, EffortMarker, Message,
    RebuildReason, Role, TurnOutcome,
};

use super::*;
use crate::agent::tests::completed;
use crate::agent::tool_tests::on_started;
use crate::agent::tool_tests::on_text;
use crate::agent::tool_tests::rebuilt;
use crate::agent::tool_tests::{costs, rejected};
use crate::agent::{Agent, IMPLICIT_ENDPOINT, LogContext, check_layout, default_session};
use crate::clock::TickingClock;
use crate::provider::anthropic::{
    AnthropicProvider, BINDING_CONTROLS_BETA, Credential, PER_MESSAGE_EFFORT_BETA,
};
use crate::provider::{Provider, ProviderError, ProviderEvent, Request};
use crate::session::{MemoryStore, Redactions, SessionLog};
use crate::test_guard::http::event;
use crate::test_guard::{ScratchDir, http, log_context, log_events, pick, until_turn_end};
use crate::test_guard::{error_json, http_400, http_529};
use crate::tool::test_toolbox;

const OPUS: &str = "claude-opus-5-5";
const SONNET: &str = "claude-sonnet-5-5";
const SONNET_5: &str = "claude-sonnet-5";

#[derive(Debug, Clone)]
struct Seen {
    head: String,
    body: String,
}

impl Seen {
    fn json(&self) -> Value {
        serde_json::from_str(&self.body).unwrap()
    }

    fn model(&self) -> String {
        self.json()["model"].as_str().unwrap().to_owned()
    }

    fn betas(&self) -> Vec<String> {
        self.head
            .lines()
            .find_map(|l| l.strip_prefix("anthropic-beta:"))
            .map(|v| v.trim().split(',').map(str::to_owned).collect())
            .unwrap_or_default()
    }

    fn has_beta(&self, beta: &str) -> bool {
        self.betas().iter().any(|b| b == beta)
    }

    fn markers(&self) -> Vec<String> {
        self.json()["messages"]
            .as_array()
            .unwrap()
            .iter()
            .filter(|m| m["role"] == "system")
            .map(|m| m["output_config"]["effort"].as_str().unwrap().to_owned())
            .collect()
    }

    fn effort(&self) -> String {
        self.json()["output_config"]["effort"]
            .as_str()
            .unwrap()
            .to_owned()
    }

    fn binding(&self) -> bool {
        self.json()["thinking"].get("block_binding").is_some()
    }
}

type Respond = dyn Fn(&Seen, usize) -> String + Send + Sync;

const HANG: &str = "\u{0}hang";
const DELAY: &str = "\u{0}delay";

async fn gateway(respond: Arc<Respond>) -> (String, Arc<Mutex<Vec<Seen>>>) {
    let seen = Arc::new(Mutex::new(Vec::new()));
    let log = seen.clone();
    let base = http::gateway(move |request| {
        let (head, body) = request.split_once("\r\n\r\n").unwrap();
        let request = Seen {
            head: head.to_ascii_lowercase(),
            body: body.to_owned(),
        };
        let n = {
            let mut log = log.lock().unwrap();
            log.push(request.clone());
            log.len() - 1
        };
        let response = respond(&request, n);
        async move {
            let response = match response.strip_prefix(DELAY) {
                Some(rest) => {
                    tokio::time::sleep(Duration::from_millis(150)).await;
                    rest.to_owned()
                }
                None => response,
            };
            match response.split_once(HANG) {
                Some((before, _)) => http::Reply::HeadThenHang(before.to_owned()),
                None => http::Reply::Send(response),
            }
        }
    })
    .await;
    (base, seen)
}

#[derive(Clone)]
enum Blk {
    Think,
    Redacted,
    Text(&'static str),
    Call(&'static str, Value),
}

fn sse_body(model: &str, n: usize, blocks: &[Blk], stop: &str, input: u64) -> String {
    let usage = json!({"input_tokens": input, "output_tokens": 1, "cache_read_input_tokens": 0, "cache_creation_input_tokens": 0});
    let mut out = event(
        "message_start",
        json!({"type": "message_start", "message": {"id": format!("msg_{n}"), "model": model, "usage": usage}}),
    );
    for (i, b) in blocks.iter().enumerate() {
        let (start, deltas): (Value, Vec<Value>) = match b {
            Blk::Think => (
                json!({"type": "thinking", "thinking": "", "signature": ""}),
                vec![
                    json!({"type": "thinking_delta", "thinking": format!("thought {n}.{i}")}),
                    json!({"type": "signature_delta", "signature": format!("sig{n}.{i}+/==")}),
                ],
            ),
            Blk::Redacted => (
                json!({"type": "redacted_thinking", "data": format!("Encrypted{n}.{i}==")}),
                Vec::new(),
            ),
            Blk::Text(t) => (
                json!({"type": "text", "text": ""}),
                vec![json!({"type": "text_delta", "text": format!("{t} {n}")})],
            ),
            Blk::Call(name, input) => (
                json!({"type": "tool_use", "id": format!("toolu_{n}_{i}"), "name": name, "input": {}}),
                vec![json!({"type": "input_json_delta", "partial_json": input.to_string()})],
            ),
        };
        out += &event(
            "content_block_start",
            json!({"type": "content_block_start", "index": i, "content_block": start}),
        );
        for d in deltas {
            out += &event(
                "content_block_delta",
                json!({"type": "content_block_delta", "index": i, "delta": d}),
            );
        }
        out += &event(
            "content_block_stop",
            json!({"type": "content_block_stop", "index": i}),
        );
    }
    out += &event(
        "message_delta",
        json!({"type": "message_delta", "delta": {"stop_reason": stop}, "usage": {"output_tokens": 7}}),
    );
    out += &event("message_stop", json!({"type": "message_stop"}));
    out
}

fn reply(seen: &Seen, n: usize, blocks: &[Blk], stop: &str) -> String {
    http::sse(&sse_body(&seen.model(), n, blocks, stop, 1000))
}

fn answer(seen: &Seen, n: usize) -> String {
    reply(seen, n, &[Blk::Think, Blk::Text("answer")], "end_turn")
}

fn chunk_error(kind: &str, message: &str) -> String {
    http::sse(&event(
        "error",
        serde_json::from_str::<Value>(&error_json(kind, message)).unwrap(),
    ))
}

fn gateway_chunk(message: &str) -> String {
    http::sse(&format!(
        "data: {}\n\ndata: [DONE]\n\n",
        json!({"error": {"message": message}})
    ))
}

fn json_200(message: &str) -> String {
    http::json(
        "200 OK",
        &json!({"error": {"message": message}}).to_string(),
    )
}

const EXTRA_OUTPUT_CONFIG: &str = "messages.3.output_config: Extra inputs are not permitted";
const EXTRA_BINDING: &str = "thinking.block_binding: Extra inputs are not permitted";
const DOC_EFFORT_BETA: &str = "Unexpected value(s) `mid-conversation-output-config-2026-07-01` for the `anthropic-beta` header.";
const DOC_BINDING_BETA: &str =
    "Unexpected value(s) `thinking-binding-controls-2026-08-01` for the `anthropic-beta` header.";
const DOC_FALLBACK_BETA: &str =
    "Unexpected value(s) `server-side-fallback-2026-07-01` for the `anthropic-beta` header.";

struct Official(AnthropicProvider);

impl Provider for Official {
    fn stream(
        &self,
        request: &Request<'_>,
    ) -> BoxStream<'static, Result<ProviderEvent, ProviderError>> {
        self.0.stream(request)
    }

    fn echoed<'a>(
        &self,
        content: &'a [ContentBlock],
        earlier: &crate::provider::ServerCalls,
    ) -> Vec<&'a ContentBlock> {
        self.0.echoed(content, earlier)
    }

    fn search_signal(&self, block: &ContentBlock) -> Option<crate::provider::SearchSignal> {
        self.0.search_signal(block)
    }

    fn capabilities(&self, model: &str) -> Option<crate::provider::ModelCapabilities> {
        self.0.capabilities(model)
    }

    fn features_documented(&self) -> bool {
        true
    }

    fn account_bound_thinking(&self, model: &str) -> bool {
        self.0.account_bound_thinking(model)
    }

    fn cache_prefix(&self, request: &Request<'_>) -> Option<String> {
        self.0.cache_prefix(request)
    }
}

fn env(name: &str) -> ScratchDir {
    ScratchDir::new("wisp-settings-test-", name)
}

type OnEvent = (fn(&AgentEvent) -> bool, Vec<ClientCommand>);

struct Rig {
    agent: Agent,
    seen: Arc<Mutex<Vec<Seen>>>,
    store: MemoryStore,
    _env: ScratchDir,
}

impl Rig {
    async fn new(
        official: bool,
        model: &str,
        effort: Effort,
        respond: impl Fn(&Seen, usize) -> String + Send + Sync + 'static,
    ) -> Self {
        let (base, seen) = gateway(Arc::new(respond)).await;
        let inner = AnthropicProvider::new(&Credential::ApiKey("sk-test".into()), &base).unwrap();
        let provider: Arc<dyn Provider> = if official {
            Arc::new(Official(inner))
        } else {
            Arc::new(inner)
        };
        let mut session = default_session();
        session.model = model.into();
        session.effort = effort;
        let env = env("rig");
        let store = MemoryStore::default();
        let context = LogContext {
            endpoint: base.clone(),
            endpoint_name: IMPLICIT_ENDPOINT.into(),
            ..log_context()
        };
        let agent = Agent::new(provider, session)
            .with_clock(TickingClock::new())
            .with_tools(test_toolbox(), env.workspace())
            .with_platform("Linux")
            .with_resources(None)
            .with_cancel_grace(Duration::from_millis(300))
            .with_retry_policy(crate::agent::retry::RetryPolicy {
                step_retries: Some(0),
                ..crate::agent::retry::quick()
            })
            .with_log(SessionLog::new(Box::new(store.clone()), "s1"), context);
        Self {
            agent,
            seen,
            store,
            _env: env,
        }
    }

    fn seen(&self) -> Vec<Seen> {
        self.seen.lock().unwrap().clone()
    }

    async fn turn(&mut self, text: &str) -> (TurnOutcome, Vec<AgentEvent>) {
        self.turn_with(text, None).await
    }

    async fn turn_with(
        &mut self,
        text: &str,
        on: Option<OnEvent>,
    ) -> (TurnOutcome, Vec<AgentEvent>) {
        let (cmd_tx, mut cmd_rx) = mpsc::channel(8);
        let (ev_tx, mut ev_rx) = mpsc::channel(1024);
        let collector = tokio::spawn(async move {
            let mut seen = Vec::new();
            let mut on = on;
            while let Some(e) = ev_rx.recv().await {
                if on.as_ref().is_some_and(|(f, _)| f(&e))
                    && let Some((_, commands)) = on.take()
                {
                    for c in commands {
                        cmd_tx.send(c).await.unwrap();
                    }
                }
                seen.push(e);
            }
            seen
        });
        let outcome = self
            .agent
            .run_turn(text.into(), &mut cmd_rx, &ev_tx)
            .await
            .unwrap();
        drop(ev_tx);
        (outcome, collector.await.unwrap())
    }

    async fn change(&mut self, model: Option<&str>, effort: Option<Effort>) -> Vec<AgentEvent> {
        let (ev_tx, mut ev_rx) = mpsc::channel(64);
        self.agent
            .change_settings(None, model.map(str::to_owned), effort, &ev_tx)
            .await;
        drop(ev_tx);
        let mut events = Vec::new();
        while let Some(e) = ev_rx.recv().await {
            events.push(e);
        }
        events
    }

    async fn cache_of(&mut self, model: Option<&str>, effort: Option<Effort>) -> CacheEffect {
        let events = self.change(model, effort).await;
        match events.as_slice() {
            [AgentEvent::SettingsChanged { cache, .. }] => cache.clone(),
            other => panic!("{other:?}"),
        }
    }

    fn rebuilt_matches_history(&self) {
        let rebuilt = rebuild_history(log_events(&self.store).iter(), no_images)
            .unwrap()
            .messages;
        assert_eq!(
            serde_json::to_string(&rebuilt).unwrap(),
            serde_json::to_string(&*self.agent.history()).unwrap()
        );
    }
}

fn split(body: &str) -> (&str, &str) {
    let m = body.find(r#","messages":"#).unwrap();
    (&body[..m], &body[m..body.len() - 1])
}

fn extends(earlier: &Seen, later: &Seen) -> bool {
    let (_, a) = split(&earlier.body);
    let (_, b) = split(&later.body);
    let closed = a.strip_suffix(']').unwrap();
    let open_tail = a.strip_suffix("]}]");
    b.starts_with(closed) || open_tail.is_some_and(|t| b.starts_with(t))
}

fn assert_chain(seen: &[Seen]) {
    for (k, pair) in seen.windows(2).enumerate() {
        assert!(
            extends(&pair[0], &pair[1]),
            "request {} doesn't extend request {k}:\n{}\n{}",
            k + 1,
            pair[0].body,
            pair[1].body
        );
    }
}

fn marker_json(effort: &str) -> String {
    format!(r#"{{"role":"system","content":[],"output_config":{{"effort":"{effort}"}}}}"#)
}

fn settings_events(events: &[AgentEvent]) -> Vec<CacheEffect> {
    pick!(events, AgentEvent::SettingsChanged { cache, .. } => cache.clone())
}

fn states(records: &[LogEvent]) -> Vec<(String, String, FeatureState)> {
    records
        .iter()
        .filter_map(|r| match r {
            LogEvent::PerMessageEffort { model, state, .. } => {
                Some(("effort".to_owned(), model.clone(), *state))
            }
            LogEvent::BindingControls { model, state, .. } => {
                Some(("binding".to_owned(), model.clone(), *state))
            }
            _ => None,
        })
        .collect()
}

fn announced(events: &[AgentEvent]) -> Vec<(wisp_protocol::Feature, String, FeatureState)> {
    pick!(
        events,
        AgentEvent::FeatureStateChanged {
            model,
            feature,
            state,
            ..
        } => (*feature, model.clone(), *state)
    )
}

fn kept_if_passed_on() -> CacheEffect {
    CacheEffect::KeptIfPassedOn
}

#[tokio::test]
async fn an_effort_change_on_a_per_message_model_only_appends() {
    let mut rig = Rig::new(true, OPUS, Effort::Medium, answer).await;
    assert!(completed(&rig.turn("first").await.0));
    assert_eq!(
        rig.cache_of(None, Some(Effort::Low)).await,
        CacheEffect::Kept
    );
    assert!(completed(&rig.turn("second").await.0));
    let seen = rig.seen();
    assert_eq!(seen.len(), 2);
    let (head_a, msgs_a) = split(&seen[0].body);
    let (head_b, msgs_b) = split(&seen[1].body);
    assert_eq!(head_a, head_b);
    assert_eq!(seen[0].betas(), seen[1].betas());
    assert!(seen[1].has_beta(PER_MESSAGE_EFFORT_BETA));
    assert!(seen[0].has_beta(PER_MESSAGE_EFFORT_BETA));
    assert_eq!(seen[1].effort(), "medium");
    let rest = &msgs_b[msgs_a.len() - 1..];
    assert!(msgs_b.starts_with(msgs_a.strip_suffix(']').unwrap()));
    assert!(rest.starts_with(r#",{"role":"assistant","#), "{rest}");
    assert!(
        rest.contains(r#"{"type":"text","text":"second"}]},{"role":"system""#),
        "{rest}"
    );
    assert!(
        rest.ends_with(&format!("{}]", marker_json("low"))),
        "{rest}"
    );
    assert_eq!(seen[1].markers(), ["low"]);
    rig.rebuilt_matches_history();
}

#[tokio::test]
async fn an_effort_change_without_per_message_effort_changes_only_the_top_level() {
    let mut rig = Rig::new(true, SONNET_5, Effort::High, answer).await;
    rig.turn("first").await;
    assert_eq!(
        rig.cache_of(None, Some(Effort::Low)).await,
        rebuilt(RebuildReason::EffortChange)
    );
    rig.turn("second").await;
    let seen = rig.seen();
    assert!(!seen[0].has_beta(PER_MESSAGE_EFFORT_BETA));
    let (head_a, _) = split(&seen[0].body);
    let (head_b, _) = split(&seen[1].body);
    assert_eq!(
        head_a.replace(r#""effort":"high""#, r#""effort":"low""#),
        head_b
    );
    assert_ne!(head_a, head_b);
    assert!(extends(&seen[0], &seen[1]));
    assert!(seen[1].markers().is_empty());
    let markers: Vec<EffortMarker> = rig
        .agent
        .history()
        .iter()
        .filter_map(Message::effort_marker)
        .collect();
    assert_eq!(
        markers,
        [EffortMarker {
            level: Effort::Low,
            rendered: false
        }]
    );
    rig.rebuilt_matches_history();
}

#[tokio::test]
async fn an_effort_change_then_a_cancel_mid_tool_keeps_the_marker_and_the_open_tail() {
    let mut rig = Rig::new(true, OPUS, Effort::Medium, |seen, n| match n {
        0 => reply(
            seen,
            n,
            &[Blk::Think, Blk::Call("sleep", json!({"ms": 60_000}))],
            "tool_use",
        ),
        _ => answer(seen, n),
    })
    .await;
    rig.change(None, Some(Effort::Low)).await;
    let cancel = (
        on_started as fn(&AgentEvent) -> bool,
        vec![ClientCommand::Cancel],
    );
    let (outcome, _) = rig.turn_with("go", Some(cancel)).await;
    assert!(matches!(
        outcome,
        TurnOutcome::Cancelled {
            committed_steps: 1,
            ..
        }
    ));
    let history = rig.agent.history();
    assert_eq!(
        history.iter().map(|m| m.role).collect::<Vec<_>>(),
        [Role::User, Role::System, Role::Assistant, Role::User]
    );
    assert!(matches!(
        history[3].content.as_slice(),
        [ContentBlock::ToolResult { .. }]
    ));
    rig.change(None, Some(Effort::High)).await;
    rig.turn("again").await;
    let seen = rig.seen();
    assert_eq!(seen.len(), 2);
    assert_chain(&seen);
    let messages = seen[1].json()["messages"].clone();
    let roles: Vec<&str> = messages
        .as_array()
        .unwrap()
        .iter()
        .map(|m| m["role"].as_str().unwrap())
        .collect();
    assert_eq!(roles, ["user", "system", "assistant", "user", "system"]);
    let merged = messages[3]["content"].as_array().unwrap();
    assert_eq!(merged[0]["type"], "tool_result");
    assert_eq!(merged.last().unwrap()["text"], "again");
    assert_eq!(seen[1].markers(), ["low", "high"]);
    rig.rebuilt_matches_history();
}

#[tokio::test]
async fn a_cancel_during_the_first_reply_drops_the_marker_with_its_message() {
    let mut rig = Rig::new(true, OPUS, Effort::Medium, |seen, n| match n {
        0 => {
            let body = sse_body(&seen.model(), n, &[Blk::Text("partial")], "end_turn", 1000);
            let cut = body.find("event: content_block_stop").unwrap();
            http::sse(&format!("{}{HANG}", &body[..cut]))
        }
        _ => answer(seen, n),
    })
    .await;
    rig.change(None, Some(Effort::Low)).await;
    let cancel = (
        on_text as fn(&AgentEvent) -> bool,
        vec![ClientCommand::Cancel],
    );
    let (outcome, _) = rig.turn_with("lost", Some(cancel)).await;
    assert!(
        matches!(
            outcome,
            TurnOutcome::Cancelled {
                committed_steps: 0,
                ..
            }
        ),
        "{outcome:?}"
    );
    assert!(rig.agent.history().is_empty());
    rig.turn("kept").await;
    let seen = rig.seen();
    assert_eq!(seen.len(), 2);
    assert_eq!(seen[0].markers(), ["low"]);
    assert_eq!(seen[1].markers(), ["low"]);
    let roles: Vec<Role> = rig.agent.history().iter().map(|m| m.role).collect();
    assert_eq!(roles, [Role::User, Role::System, Role::Assistant]);
    let text = serde_json::to_string(&rig.agent.history()[0]).unwrap();
    assert!(text.contains("kept") && !text.contains("lost"));
    rig.rebuilt_matches_history();
}

#[tokio::test]
async fn a_model_switch_keeps_every_block_and_switching_back_renders_the_same_bytes() {
    let mut rig = Rig::new(true, OPUS, Effort::Medium, |seen, n| {
        reply(
            seen,
            n,
            &[Blk::Think, Blk::Redacted, Blk::Text("answer")],
            "end_turn",
        )
    })
    .await;
    rig.change(None, Some(Effort::Low)).await;
    rig.turn("one").await;
    assert_eq!(
        rig.cache_of(Some(SONNET), Some(Effort::High)).await,
        rebuilt(RebuildReason::ModelSwitch)
    );
    rig.turn("two").await;
    rig.turn("three").await;
    rig.change(Some(OPUS), Some(Effort::Low)).await;
    rig.turn("four").await;
    let seen = rig.seen();
    assert_eq!(
        seen.iter().map(Seen::model).collect::<Vec<_>>(),
        [OPUS, SONNET, SONNET, OPUS]
    );
    assert_chain(&seen);
    for (k, request) in seen.iter().enumerate().skip(1) {
        let messages = split(&request.body).1;
        for n in 0..k {
            for needle in [
                format!(
                    r#"{{"type":"thinking","thinking":"thought {n}.0","signature":"sig{n}.0+/=="}}"#
                ),
                format!(r#"{{"type":"redacted_thinking","data":"Encrypted{n}.1=="}}"#),
            ] {
                assert!(messages.contains(&needle), "{k}: {needle}");
            }
        }
    }
    assert_eq!(seen[1].markers(), ["low", "high"]);
    assert_eq!(seen[3].markers(), ["low", "high", "low"]);
    let (_, opus_first) = split(&seen[0].body);
    let (_, opus_back) = split(&seen[3].body);
    assert!(opus_back.starts_with(opus_first.strip_suffix(']').unwrap()));
    rig.rebuilt_matches_history();
}

fn opus_accepts_sonnet_rejects(seen: &Seen, n: usize) -> String {
    if seen.model() == SONNET && !seen.markers().is_empty() {
        return http_400(EXTRA_OUTPUT_CONFIG);
    }
    answer(seen, n)
}

#[tokio::test]
async fn gateway_states_are_kept_per_model() {
    let mut rig = Rig::new(false, OPUS, Effort::Medium, opus_accepts_sonnet_rejects).await;
    rig.turn("one").await;
    assert_eq!(
        rig.cache_of(None, Some(Effort::Low)).await,
        kept_if_passed_on()
    );
    let (_, two) = rig.turn("two").await;
    assert_eq!(
        announced(&two),
        [(
            wisp_protocol::Feature::PerMessageEffort,
            OPUS.to_owned(),
            FeatureState::Proven
        )]
    );
    assert_eq!(
        rig.agent
            .pair_state(IMPLICIT_ENDPOINT, OPUS, Feature::PerMessageEffort),
        PairState::Proven
    );
    assert_eq!(
        rig.cache_of(None, Some(Effort::High)).await,
        CacheEffect::Kept
    );
    rig.change(Some(SONNET), Some(Effort::Medium)).await;
    let (outcome, events) = rig.turn("three").await;
    assert!(completed(&outcome), "{outcome:?}");
    assert_eq!(
        settings_events(&events),
        [rebuilt(RebuildReason::PerMessageEffortRejected)]
    );
    assert_eq!(costs(&events).len(), 1);
    assert_eq!(
        announced(&events),
        [(
            wisp_protocol::Feature::PerMessageEffort,
            SONNET.to_owned(),
            FeatureState::FallenBack
        )]
    );
    rig.turn("four").await;
    rig.change(Some(OPUS), Some(Effort::Low)).await;
    rig.turn("five").await;
    let seen = rig.seen();
    assert_eq!(seen.len(), 6);
    assert_eq!(seen[2].model(), SONNET);
    assert_eq!(seen[2].markers(), ["low", "medium"]);
    assert!(seen[2].has_beta(PER_MESSAGE_EFFORT_BETA));
    assert!(seen[3].markers().is_empty());
    assert!(!seen[3].has_beta(PER_MESSAGE_EFFORT_BETA));
    assert_eq!(seen[3].effort(), "medium");
    assert_eq!(seen[4].effort(), "medium");
    assert!(seen[4].markers().is_empty());
    assert!(extends(&seen[3], &seen[4]));
    assert_eq!(seen[5].model(), OPUS);
    assert_eq!(seen[5].markers(), ["low"]);
    assert_eq!(seen[5].effort(), "medium");
    assert!(seen[5].has_beta(PER_MESSAGE_EFFORT_BETA));
    let (_, last_opus) = split(&seen[1].body);
    let (_, back) = split(&seen[5].body);
    assert!(back.starts_with(last_opus.strip_suffix(']').unwrap()));
    assert_eq!(
        states(&log_events(&rig.store)),
        [
            ("effort".to_owned(), OPUS.to_owned(), FeatureState::Proven),
            (
                "effort".to_owned(),
                SONNET.to_owned(),
                FeatureState::FallenBack
            ),
        ]
    );
    assert_eq!(
        rig.agent
            .pair_state(IMPLICIT_ENDPOINT, SONNET, Feature::PerMessageEffort),
        PairState::FallenBack
    );
    rig.rebuilt_matches_history();
}

async fn first_request_rejection(message: &'static str, form: fn(&str) -> String) {
    let mut rig = Rig::new(false, SONNET, Effort::Medium, move |seen, n| {
        if seen.has_beta(PER_MESSAGE_EFFORT_BETA) {
            return form(message);
        }
        answer(seen, n)
    })
    .await;
    let (outcome, events) = rig.turn("one").await;
    assert!(completed(&outcome), "{message}: {outcome:?}");
    let steps = costs(&events);
    assert_eq!(steps.len(), 1);
    assert!(!steps[0].1);
    assert!(settings_events(&events).is_empty());
    assert_eq!(
        announced(&events),
        [(
            wisp_protocol::Feature::PerMessageEffort,
            SONNET.to_owned(),
            FeatureState::FallenBack
        )],
        "{message}"
    );
    assert_eq!(
        rig.cache_of(None, Some(Effort::Low)).await,
        rebuilt(RebuildReason::EffortChange)
    );
    rig.turn("two").await;
    let seen = rig.seen();
    assert_eq!(seen.len(), 3, "{message}");
    assert!(seen[0].has_beta(PER_MESSAGE_EFFORT_BETA));
    for s in &seen[1..] {
        assert!(!s.has_beta(PER_MESSAGE_EFFORT_BETA));
        assert!(s.markers().is_empty());
    }
    assert_eq!(seen[2].effort(), "low");
    assert!(extends(&seen[1], &seen[2]));
}

#[tokio::test]
async fn a_rejected_beta_name_or_header_falls_back_before_any_marker() {
    first_request_rejection(
        "Unsupported beta header value: mid-conversation-output-config-2026-07-01",
        http_400,
    )
    .await;
    first_request_rejection(
        "unknown beta mid-conversation-output-config-2026-07-01",
        http_400,
    )
    .await;
    first_request_rejection("invalid anthropic-beta header", http_400).await;
    for form in [http_400, json_200, chunk_form, gateway_chunk] {
        first_request_rejection(DOC_EFFORT_BETA, form).await;
    }
}

#[test]
fn each_pattern_matches_its_fixture_and_nothing_else() {
    use crate::provider::anthropic::{ErrorForm, rejected_features};
    let both = [Feature::PerMessageEffort, Feature::BindingControls];
    let effort = [Feature::PerMessageEffort];
    let binding = [Feature::BindingControls];
    let http = ErrorForm::Http400;
    let cases: &[(&str, ErrorForm, &[Feature])] = &[
        (EXTRA_OUTPUT_CONFIG, http, &effort),
        (
            "messages.12.output_config: Extra inputs are not permitted",
            ErrorForm::Chunk,
            &effort,
        ),
        (
            "output_config.effort requires a model that supports per-turn effort; this model does not",
            http,
            &effort,
        ),
        (EXTRA_BINDING, http, &binding),
        (
            "block_binding: Extra inputs are not permitted",
            ErrorForm::Json,
            &binding,
        ),
        (
            "Unsupported beta: mid-conversation-output-config-2026-07-01",
            http,
            &effort,
        ),
        (
            "unknown beta thinking-binding-controls-2026-08-01",
            ErrorForm::Chunk,
            &binding,
        ),
        ("invalid anthropic-beta header", http, &both),
        ("invalid anthropic-beta header", ErrorForm::Json, &both),
        ("invalid anthropic-beta header", ErrorForm::Chunk, &[]),
        (
            "messages.output_config: Extra inputs are not permitted",
            http,
            &[],
        ),
        (
            "xmessages.3.output_config: Extra inputs are not permitted",
            http,
            &[],
        ),
        (
            "messages.3.output_config: extra inputs are not permitted",
            http,
            &[],
        ),
        (
            "Unsupported beta: server-side-fallback-2026-07-01",
            http,
            &[],
        ),
        (
            "Unsupported beta: mid-conversation-output-config-2026-07-01x",
            http,
            &[],
        ),
        (
            "mid-conversation-output-config-2026-07-01 is great",
            http,
            &[],
        ),
        (
            "Overloaded while reading output_config",
            ErrorForm::Chunk,
            &[],
        ),
        ("x-anthropic-beta-ish header", http, &[]),
        (DOC_EFFORT_BETA, http, &effort),
        (DOC_EFFORT_BETA, ErrorForm::Json, &effort),
        (DOC_EFFORT_BETA, ErrorForm::Chunk, &effort),
        (DOC_BINDING_BETA, ErrorForm::Chunk, &binding),
        (
            "Unexpected value(s) `mid-conversation-output-config-2026-07-01`, `thinking-binding-controls-2026-08-01` for the `anthropic-beta` header.",
            ErrorForm::Chunk,
            &both,
        ),
        (
            "Unexpected value(s) `server-side-fallback-2026-07-01`, `mid-conversation-output-config-2026-07-01` for the `anthropic-beta` header.",
            http,
            &effort,
        ),
        (DOC_FALLBACK_BETA, http, &[]),
        (DOC_FALLBACK_BETA, ErrorForm::Json, &[]),
        (DOC_FALLBACK_BETA, ErrorForm::Chunk, &[]),
        (
            "invalid anthropic-beta header: server-side-fallback-2026-07-01",
            http,
            &[],
        ),
        (
            "invalid anthropic-beta header value mid-conversation-output-config-2026-07-01",
            http,
            &effort,
        ),
        (
            "invalid anthropic-beta header: server-side-fallback-2026-07-01, mid-conversation-output-config-2026-07-01",
            http,
            &[],
        ),
        (
            "unknown beta server-side-fallback-2026-07-01 next to mid-conversation-output-config-2026-07-01",
            http,
            &[],
        ),
        (
            "thinking.adaptive.block_binding: Extra inputs are not permitted",
            http,
            &binding,
        ),
        (
            "body.thinking.block_binding: Extra inputs are not permitted",
            ErrorForm::Chunk,
            &binding,
        ),
        (
            "body.messages.3.output_config: Extra inputs are not permitted",
            ErrorForm::Json,
            &effort,
        ),
        (
            "request.body.messages.0.output_config: Extra inputs are not permitted",
            ErrorForm::Chunk,
            &effort,
        ),
        (
            "thinking.xblock_binding: Extra inputs are not permitted",
            http,
            &[],
        ),
        (
            "thinking.block_bindings: Extra inputs are not permitted",
            http,
            &[],
        ),
        (
            "body.messages.3.output_configs: Extra inputs are not permitted",
            http,
            &[],
        ),
    ];
    for (message, form, expected) in cases {
        assert_eq!(
            rejected_features(message, *form, Features::trying(&both)),
            expected.to_vec(),
            "{message} as {form:?}"
        );
    }
    assert!(rejected_features(EXTRA_OUTPUT_CONFIG, http, Features::trying(&binding)).is_empty());
    assert!(rejected_features(EXTRA_BINDING, http, Features::trying(&effort)).is_empty());
    assert!(rejected_features(EXTRA_OUTPUT_CONFIG, http, Features::trying(&[])).is_empty());
    assert!(
        rejected_features(
            DOC_BINDING_BETA,
            ErrorForm::Chunk,
            Features::trying(&effort)
        )
        .is_empty()
    );
    assert!(rejected_features(DOC_EFFORT_BETA, http, Features::trying(&binding)).is_empty());
}

async fn a_failing_turn(
    official: bool,
    respond: impl Fn(&Seen, usize) -> String + Send + Sync + 'static,
) -> (Rig, TurnOutcome, Vec<AgentEvent>) {
    let mut rig = Rig::new(official, SONNET, Effort::Medium, respond).await;
    rig.change(None, Some(Effort::Low)).await;
    let (outcome, events) = rig.turn("one").await;
    (rig, outcome, events)
}

#[tokio::test]
async fn pre_event_errors_that_do_not_match_keep_todays_classification() {
    let (rig, outcome, events) = a_failing_turn(false, |_, _| {
        chunk_error("overloaded_error", "Overloaded while reading output_config")
    })
    .await;
    assert!(matches!(outcome, TurnOutcome::Error { .. }), "{outcome:?}");
    assert_eq!(costs(&events), [(None, true)]);
    assert_eq!(rig.seen().len(), 1);

    let (rig, outcome, events) = a_failing_turn(false, |_, _| {
        http_400("Unsupported beta: server-side-fallback-2026-07-01")
    })
    .await;
    assert!(matches!(outcome, TurnOutcome::Error { .. }));
    assert_eq!(costs(&events), [(None, false)]);
    assert_eq!(rig.seen().len(), 1);

    let (rig, outcome, events) = a_failing_turn(false, |_, _| http_400(DOC_FALLBACK_BETA)).await;
    assert!(matches!(outcome, TurnOutcome::Error { .. }), "{outcome:?}");
    assert_eq!(costs(&events), [(None, false)]);
    assert_eq!(rig.seen().len(), 1);
    assert!(settings_events(&events).is_empty());
    assert!(states(&log_events(&rig.store)).is_empty());
    assert_eq!(
        rig.agent
            .pair_state(IMPLICIT_ENDPOINT, SONNET, Feature::PerMessageEffort),
        PairState::Unknown
    );

    let (rig, outcome, events) = a_failing_turn(false, |seen, n| {
        let mut body = sse_body(&seen.model(), n, &[], "end_turn", 10);
        let cut = body.find("event: message_delta").unwrap();
        body.truncate(cut);
        body += &format!(
            "data: {}\n\n",
            json!({"error": {"message": EXTRA_OUTPUT_CONFIG}})
        );
        http::sse(&body)
    })
    .await;
    assert!(matches!(outcome, TurnOutcome::Error { .. }), "{outcome:?}");
    assert_eq!(costs(&events).len(), 1);
    assert!(costs(&events)[0].1);
    assert_eq!(rig.seen().len(), 1);

    let (rig, outcome, events) = a_failing_turn(false, |seen, n| {
        let mut body = sse_body(&seen.model(), n, &[], "end_turn", 10);
        let cut = body.find("event: message_delta").unwrap();
        body.truncate(cut);
        body += &event(
            "error",
            serde_json::from_str::<Value>(&error_json(
                "invalid_request_error",
                EXTRA_OUTPUT_CONFIG,
            ))
            .unwrap(),
        );
        http::sse(&body)
    })
    .await;
    assert!(matches!(outcome, TurnOutcome::Error { .. }));
    assert_eq!(costs(&events).len(), 1);
    assert!(costs(&events)[0].1);
    assert_eq!(rig.seen().len(), 1);
}

fn the_error(outcome: &TurnOutcome) -> (wisp_protocol::ErrorKind, String) {
    let TurnOutcome::Error { kind, message, .. } = outcome else {
        panic!("{outcome:?}")
    };
    (*kind, message.clone())
}

fn effort_states(rig: &Rig) -> Vec<FeatureState> {
    states(&log_events(&rig.store))
        .into_iter()
        .map(|(_, _, s)| s)
        .collect()
}

#[tokio::test]
async fn a_proven_effort_rejected_by_shape_falls_back_once_and_completes() {
    let mut rig = Rig::new(false, SONNET, Effort::Medium, |seen, n| match n {
        2 => http_400(EXTRA_OUTPUT_CONFIG),
        _ => answer(seen, n),
    })
    .await;
    rig.change(None, Some(Effort::Low)).await;
    rig.turn("one").await;
    rig.turn("two").await;
    rig.change(None, Some(Effort::High)).await;
    let (outcome, events) = rig.turn("three").await;
    assert!(completed(&outcome), "{outcome:?}");
    assert_eq!(costs(&events).len(), 1);
    let seen = rig.seen();
    assert_eq!(seen.len(), 4);
    assert!(!seen[2].markers().is_empty() && seen[2].has_beta(PER_MESSAGE_EFFORT_BETA));
    assert!(seen[3].markers().is_empty() && !seen[3].has_beta(PER_MESSAGE_EFFORT_BETA));
    assert_eq!(seen[3].effort(), "high");
    assert_eq!(
        effort_states(&rig),
        [FeatureState::Proven, FeatureState::FallenBack]
    );
    assert_eq!(
        rig.agent
            .pair_state(IMPLICIT_ENDPOINT, SONNET, Feature::PerMessageEffort),
        PairState::FallenBack
    );
}

#[tokio::test]
async fn proven_binding_controls_rejected_by_shape_fall_back_once_and_complete() {
    let mut rig = Rig::new(false, SONNET, Effort::Medium, |seen, n| match n {
        1 => http_400(EXTRA_BINDING),
        _ => answer(seen, n),
    })
    .await;
    rig.agent.settings.binding_controls = true;
    rig.turn("one").await;
    let (outcome, _) = rig.turn("two").await;
    assert!(completed(&outcome), "{outcome:?}");
    let seen = rig.seen();
    assert_eq!(seen.len(), 3);
    assert!(seen[1].binding() && seen[1].has_beta(BINDING_CONTROLS_BETA));
    assert!(!seen[2].binding() && !seen[2].has_beta(BINDING_CONTROLS_BETA));
    assert_eq!(
        effort_states(&rig),
        [FeatureState::Proven, FeatureState::FallenBack]
    );
}

#[tokio::test]
async fn a_documented_effort_rejected_by_shape_falls_back_and_stops_sending_the_field_and_beta() {
    let (mut rig, outcome, _) = a_failing_turn(true, |seen, n| match n {
        0 => http_400(EXTRA_OUTPUT_CONFIG),
        _ => answer(seen, n),
    })
    .await;
    assert!(completed(&outcome), "{outcome:?}");
    rig.turn("two").await;
    let seen = rig.seen();
    assert_eq!(seen.len(), 3);
    assert!(!seen[0].markers().is_empty() && seen[0].has_beta(PER_MESSAGE_EFFORT_BETA));
    for later in &seen[1..] {
        assert!(later.markers().is_empty() && !later.has_beta(PER_MESSAGE_EFFORT_BETA));
        assert_eq!(later.effort(), "low");
    }
    assert_eq!(effort_states(&rig), [FeatureState::FallenBack]);
    assert_eq!(
        rig.agent
            .pair_state(IMPLICIT_ENDPOINT, SONNET, Feature::PerMessageEffort),
        PairState::FallenBack
    );
}

#[tokio::test]
async fn documented_binding_controls_rejected_by_shape_fall_back_and_stop_sending_the_field_and_beta()
 {
    let mut rig = Rig::new(true, SONNET, Effort::Medium, |seen, n| match n {
        0 => http_400(EXTRA_BINDING),
        _ => answer(seen, n),
    })
    .await;
    rig.agent.settings.binding_controls = true;
    let (outcome, _) = rig.turn("one").await;
    assert!(completed(&outcome), "{outcome:?}");
    rig.turn("two").await;
    let seen = rig.seen();
    assert_eq!(seen.len(), 3);
    assert!(seen[0].binding() && seen[0].has_beta(BINDING_CONTROLS_BETA));
    for later in &seen[1..] {
        assert!(!later.binding() && !later.has_beta(BINDING_CONTROLS_BETA));
    }
    assert_eq!(effort_states(&rig), [FeatureState::FallenBack]);
}

#[tokio::test]
async fn a_rejection_repeated_after_the_fall_back_ends_the_turn_with_its_400() {
    let (rig, outcome, events) = a_failing_turn(true, |_, _| http_400(EXTRA_OUTPUT_CONFIG)).await;
    let (kind, message) = the_error(&outcome);
    assert_eq!(kind, wisp_protocol::ErrorKind::InvalidRequest);
    assert!(message.contains(EXTRA_OUTPUT_CONFIG), "{message}");
    assert_eq!(rig.seen().len(), 2);
    assert_eq!(effort_states(&rig), [FeatureState::FallenBack]);
    assert_eq!(costs(&events).len(), 1);
}

#[tokio::test]
async fn an_established_feature_rejected_only_in_weak_wording_ends_the_turn() {
    let weak = "Unsupported beta: mid-conversation-output-config-2026-07-01";
    let (rig, outcome, events) = a_failing_turn(true, move |_, _| http_400(weak)).await;
    assert_eq!(
        the_error(&outcome).0,
        wisp_protocol::ErrorKind::InvalidRequest
    );
    assert_eq!(rig.seen().len(), 1);
    assert!(settings_events(&events).is_empty());
    assert!(effort_states(&rig).is_empty());

    let mut rig = Rig::new(false, SONNET, Effort::Medium, move |seen, n| match n {
        2 => http_400(weak),
        _ => answer(seen, n),
    })
    .await;
    rig.change(None, Some(Effort::Low)).await;
    rig.turn("one").await;
    rig.turn("two").await;
    rig.change(None, Some(Effort::High)).await;
    let (outcome, _) = rig.turn("three").await;
    assert_eq!(
        the_error(&outcome).0,
        wisp_protocol::ErrorKind::InvalidRequest
    );
    assert_eq!(rig.seen().len(), 3);
    assert_eq!(effort_states(&rig), [FeatureState::Proven]);
    assert_eq!(
        rig.agent
            .pair_state(IMPLICIT_ENDPOINT, SONNET, Feature::PerMessageEffort),
        PairState::Proven
    );
}

#[test]
fn an_established_feature_is_rejected_only_by_shape_or_an_unexpected_beta() {
    use crate::provider::anthropic::{ErrorForm, rejected_features};
    let established = Features {
        documented: true,
        ..Features::using(&Feature::ALL, FeatureUse::Established)
    };
    let http = ErrorForm::Http400;
    let cases: &[(&str, &[Feature])] = &[
        (EXTRA_OUTPUT_CONFIG, &[Feature::PerMessageEffort]),
        (EXTRA_BINDING, &[Feature::BindingControls]),
        (
            "compaction: Extra inputs are not permitted",
            &[Feature::NativeCompaction],
        ),
        (
            "messages.0.content.0: unexpected content block type 'compaction'; expected one of text, image",
            &[Feature::NativeCompaction],
        ),
        (DOC_EFFORT_BETA, &[Feature::PerMessageEffort]),
        (DOC_BINDING_BETA, &[Feature::BindingControls]),
        (
            "Unsupported beta: mid-conversation-output-config-2026-07-01",
            &[],
        ),
        ("invalid anthropic-beta header", &[]),
        (
            "invalid anthropic-beta header value mid-conversation-output-config-2026-07-01",
            &[],
        ),
        (DOC_FALLBACK_BETA, &[]),
    ];
    for (message, expected) in cases {
        for form in [http, ErrorForm::Json, ErrorForm::Chunk] {
            assert_eq!(
                rejected_features(message, form, established),
                expected.to_vec(),
                "{message} as {form:?}"
            );
        }
    }
    assert!(rejected_features(EXTRA_OUTPUT_CONFIG, http, Features::default()).is_empty());
    let proven = Features::using(&[Feature::InlineTools], FeatureUse::Established);
    assert_eq!(
        rejected_features("invalid anthropic-beta header", http, proven),
        [Feature::InlineTools]
    );
}

async fn one_step_resend(form: fn(&str) -> String) {
    let (rig, outcome, events) = a_failing_turn(false, move |seen, n| {
        if seen.markers().is_empty() {
            http::sse(&sse_body(
                &seen.model(),
                n,
                &[Blk::Text("fine")],
                "end_turn",
                2000,
            ))
        } else {
            form(EXTRA_OUTPUT_CONFIG)
        }
    })
    .await;
    assert!(completed(&outcome), "{outcome:?}");
    let steps = costs(&events);
    let price = 2000.0 * 2.0 / 1e6 + 7.0 * 10.0 / 1e6;
    assert_eq!(steps.len(), 1);
    assert!((steps[0].0.unwrap() - price).abs() < 1e-12, "{steps:?}");
    assert!(!steps[0].1);
    let TurnOutcome::Completed {
        cost: Some(cost),
        steps: 1,
        ..
    } = outcome
    else {
        panic!("{outcome:?}")
    };
    assert!(!cost.turn_lower_bound && !cost.session_lower_bound);
    assert_eq!(rig.seen().len(), 2);
    assert_eq!(
        settings_events(&events),
        [rebuilt(RebuildReason::PerMessageEffortRejected)]
    );
    let records = log_events(&rig.store);
    assert_eq!(
        states(&records),
        [(
            "effort".to_owned(),
            SONNET.to_owned(),
            FeatureState::FallenBack
        )]
    );
    let logged_marker = records.iter().find_map(|r| match r {
        LogEvent::TurnStarted { effort_marker, .. } => *effort_marker,
        _ => None,
    });
    assert_eq!(
        logged_marker,
        Some(EffortMarker {
            level: Effort::Low,
            rendered: true
        })
    );
    assert_eq!(
        rig.agent
            .history()
            .iter()
            .filter_map(Message::effort_marker)
            .collect::<Vec<_>>(),
        [EffortMarker {
            level: Effort::Low,
            rendered: false
        }]
    );
    assert!(records.iter().any(|r| matches!(
        r,
        LogEvent::SettingsChanged {
            cache: CacheEffect::Rebuilt {
                reason: RebuildReason::PerMessageEffortRejected
            },
            ..
        }
    )));
    rig.rebuilt_matches_history();
}

fn chunk_form(message: &str) -> String {
    chunk_error("invalid_request_error", message)
}

#[tokio::test]
async fn every_resend_is_one_step_in_each_error_form() {
    one_step_resend(http_400).await;
    one_step_resend(chunk_form).await;
    one_step_resend(gateway_chunk).await;
    one_step_resend(json_200).await;
}

#[tokio::test]
async fn an_error_naming_both_features_falls_back_for_both_at_once() {
    let mut rig = Rig::new(false, SONNET, Effort::Medium, |seen, n| {
        if seen.has_beta(PER_MESSAGE_EFFORT_BETA) || seen.has_beta(BINDING_CONTROLS_BETA) {
            return http_400(&format!("{EXTRA_OUTPUT_CONFIG}; {EXTRA_BINDING}"));
        }
        answer(seen, n)
    })
    .await;
    rig.agent.settings.binding_controls = true;
    rig.change(None, Some(Effort::Low)).await;
    let (outcome, events) = rig.turn("one").await;
    assert!(completed(&outcome), "{outcome:?}");
    assert_eq!(costs(&events).len(), 1);
    let seen = rig.seen();
    assert_eq!(seen.len(), 2);
    assert!(seen[0].binding() && seen[0].has_beta(BINDING_CONTROLS_BETA));
    assert!(!seen[1].binding() && seen[1].betas() == ["server-side-fallback-2026-07-01"]);
    assert_eq!(states(&log_events(&rig.store)).len(), 2);
}

fn dummy() -> AnthropicProvider {
    AnthropicProvider::new(&Credential::ApiKey("x".into()), "http://127.0.0.1:9").unwrap()
}

#[tokio::test]
async fn native_tool_changes_prove_only_the_provider_that_recognizes_them() {
    let change = ContentBlock::ToolRemoval {
        tool: wisp_protocol::ToolRef::ToolReference {
            name: "tool".into(),
        },
    };
    let messages = [Message {
        role: Role::User,
        content: vec![ContentBlock::opaque(
            "anthropic",
            json!({"type": "compaction", "tool_changes": [change]}),
        )],
    }];
    let providers: [(Arc<dyn Provider>, PairState); 2] = [
        (Arc::new(dummy()), PairState::Proven),
        (crate::agent::testing::Fake::new([]), PairState::Unknown),
    ];
    for (provider, expected) in providers {
        let mut agent = Agent::new(provider, default_session());
        let (events, _receiver) = mpsc::channel(8);
        agent
            .prove(
                Features::trying(&[Feature::InlineTools]),
                &messages,
                &events,
            )
            .await;
        assert_eq!(
            agent.pair_state(
                IMPLICIT_ENDPOINT,
                &agent.session.model,
                Feature::InlineTools
            ),
            expected
        );
    }
}

#[tokio::test]
async fn features_named_one_after_the_other_take_two_resends_in_one_step() {
    let mut rig = Rig::new(false, SONNET, Effort::Medium, |seen, n| {
        if seen.binding() {
            return http_400(EXTRA_BINDING);
        }
        if !seen.markers().is_empty() {
            return chunk_form(EXTRA_OUTPUT_CONFIG);
        }
        answer(seen, n)
    })
    .await;
    rig.agent.settings.binding_controls = true;
    rig.change(None, Some(Effort::Low)).await;
    let (outcome, events) = rig.turn("one").await;
    assert!(completed(&outcome), "{outcome:?}");
    assert_eq!(costs(&events).len(), 1);
    assert!(!costs(&events)[0].1);
    let seen = rig.seen();
    assert_eq!(seen.len(), 3);
    assert!(seen[1].markers() == ["low"] && !seen[1].binding());
    assert!(seen[2].markers().is_empty() && !seen[2].binding());
    rig.turn("two").await;
    let seen = rig.seen();
    assert_eq!(seen.len(), 4);
    assert!(!seen[3].binding() && !seen[3].has_beta(BINDING_CONTROLS_BETA));
}

#[tokio::test]
async fn the_controls_beta_is_tried_once_per_model_through_a_gateway() {
    let mut rig = Rig::new(false, SONNET, Effort::Medium, |seen, n| {
        if seen.model() == SONNET && seen.binding() {
            return http_400("Unsupported beta: thinking-binding-controls-2026-08-01");
        }
        answer(seen, n)
    })
    .await;
    rig.agent.settings.binding_controls = true;
    rig.turn("one").await;
    rig.turn("two").await;
    rig.change(Some(OPUS), None).await;
    rig.turn("three").await;
    rig.turn("four").await;
    let seen = rig.seen();
    assert_eq!(seen.len(), 5);
    assert!(seen[0].binding() && seen[0].has_beta(BINDING_CONTROLS_BETA));
    for s in &seen[1..3] {
        assert!(!s.binding() && !s.has_beta(BINDING_CONTROLS_BETA));
    }
    for s in &seen[3..] {
        assert!(s.binding() && s.has_beta(BINDING_CONTROLS_BETA));
        assert_eq!(
            s.json()["thinking"]["block_binding"],
            json!({"prefix_mismatch_behavior": "drop_block"})
        );
    }
    assert_eq!(
        states(&log_events(&rig.store)),
        [
            (
                "binding".to_owned(),
                SONNET.to_owned(),
                FeatureState::FallenBack
            ),
            ("binding".to_owned(), OPUS.to_owned(), FeatureState::Proven),
        ]
    );
}

#[tokio::test]
async fn the_official_endpoint_always_sends_the_controls_beta() {
    let mut rig = Rig::new(true, SONNET, Effort::Medium, answer).await;
    rig.agent.settings.binding_controls = true;
    rig.turn("one").await;
    rig.change(Some(SONNET_5), None).await;
    rig.turn("two").await;
    let seen = rig.seen();
    assert!(
        seen.iter()
            .all(|s| s.binding() && s.has_beta(BINDING_CONTROLS_BETA))
    );
    assert!(states(&log_events(&rig.store)).is_empty());
}

#[tokio::test]
async fn a_to_b_to_a_never_renders_the_b_era_marker() {
    let mut rig = Rig::new(true, OPUS, Effort::Medium, answer).await;
    rig.change(None, Some(Effort::Low)).await;
    rig.turn("a1").await;
    rig.turn("a2").await;
    rig.change(Some(SONNET_5), None).await;
    rig.turn("b1").await;
    rig.change(None, Some(Effort::High)).await;
    rig.turn("b2").await;
    rig.change(Some(OPUS), None).await;
    rig.turn("a3").await;
    let seen = rig.seen();
    assert_eq!(seen.len(), 5);
    assert!(seen[2].markers().is_empty() && seen[3].markers().is_empty());
    assert_eq!(seen[2].effort(), "low");
    assert_eq!(seen[3].effort(), "high");
    let history_markers: Vec<EffortMarker> = rig
        .agent
        .history()
        .iter()
        .filter_map(Message::effort_marker)
        .collect();
    assert_eq!(
        history_markers,
        [
            EffortMarker {
                level: Effort::Low,
                rendered: true
            },
            EffortMarker {
                level: Effort::High,
                rendered: false
            },
            EffortMarker {
                level: Effort::High,
                rendered: true
            },
        ]
    );
    assert_eq!(seen[4].markers(), ["low", "high"]);
    assert_eq!(seen[4].effort(), "medium");
    let (_, a2) = split(&seen[1].body);
    let (_, a3) = split(&seen[4].body);
    assert!(a3.starts_with(a2.strip_suffix(']').unwrap()));
    assert!(a3.ends_with(&format!("{}]", marker_json("high"))), "{a3}");
    rig.rebuilt_matches_history();
}

fn on_tool_requested(e: &AgentEvent) -> bool {
    matches!(e, AgentEvent::ToolCallRequested { .. })
}

#[tokio::test]
async fn a_change_during_a_tool_loop_waits_for_the_next_turn() {
    let mut rig = Rig::new(true, OPUS, Effort::Medium, |seen, n| match n {
        0 | 1 => reply(
            seen,
            n,
            &[
                Blk::Think,
                Blk::Call("sleep", json!({"ms": 0, "text": "x"})),
            ],
            "tool_use",
        ),
        _ => answer(seen, n),
    })
    .await;
    let changes = vec![
        ClientCommand::shortcut(RequestId(1), None, Some(SONNET.into()), Some(Effort::Low)),
        ClientCommand::shortcut(RequestId(2), None, None, Some(Effort::Xhigh)),
    ];
    let (outcome, events) = rig
        .turn_with("loop", Some((on_tool_requested, changes)))
        .await;
    let TurnOutcome::Completed { model, steps, .. } = &outcome else {
        panic!("{outcome:?}")
    };
    assert_eq!((model.as_str(), *steps), (OPUS, 3));
    assert_eq!(settings_events(&events).len(), 2);
    assert_eq!(
        rig.agent.pending_settings(),
        Some((IMPLICIT_ENDPOINT, SONNET, Effort::Xhigh))
    );
    rig.turn("next").await;
    let seen = rig.seen();
    assert_eq!(seen.len(), 4);
    for s in &seen[..3] {
        assert_eq!(s.model(), OPUS);
        assert!(s.markers().is_empty());
        assert_eq!(s.effort(), "medium");
    }
    assert_chain(&seen[..3]);
    assert_eq!(seen[3].model(), SONNET);
    assert_eq!(seen[3].markers(), ["xhigh"]);
    assert_eq!(rig.agent.pending_settings(), None);
    rig.rebuilt_matches_history();
}

#[tokio::test]
async fn an_unknown_model_gets_todays_request_shape() {
    let mut rig = Rig::new(true, "claude-future-9", Effort::Medium, answer).await;
    rig.turn("one").await;
    assert_eq!(
        rig.cache_of(None, Some(Effort::Max)).await,
        rebuilt(RebuildReason::EffortChange)
    );
    rig.turn("two").await;
    let seen = rig.seen();
    for s in &seen {
        assert!(!s.has_beta(PER_MESSAGE_EFFORT_BETA));
        assert!(s.markers().is_empty());
        assert_eq!(s.json()["max_tokens"], crate::agent::DEFAULT_MAX_TOKENS);
    }
    assert_eq!(seen[1].effort(), "max");
    assert!(
        rig.change(Some("claude-future-10"), Some(Effort::Xhigh))
            .await
            .len()
            == 1
    );
}

#[tokio::test]
async fn max_tokens_is_capped_at_the_models_maximum_output() {
    let mut rig = Rig::new(true, OPUS, Effort::Medium, answer).await;
    rig.agent.settings.max_tokens = 300_000;
    rig.turn("one").await;
    rig.change(Some("claude-future-9"), None).await;
    rig.turn("two").await;
    let account = rig.agent.endpoint_name().to_owned();
    let mut text = "[models.\"claude-future-9\"]\nmax_output = 20000\n".to_owned();
    let models = |text: &str| {
        let config = crate::models::ConfigModels::parse(text).unwrap();
        crate::context::ModelSettings::new(crate::models::ModelTable::shipped().unwrap(), config)
    };
    rig.agent.models = models(&text);
    rig.turn("three").await;
    text.push_str(&format!(
        "\n[endpoints.{account}.models.\"claude-future-9\"]\nmax_output = \"10k\"\n"
    ));
    rig.agent.models = models(&text);
    rig.turn("four").await;
    let seen = rig.seen();
    assert_eq!(seen[0].json()["max_tokens"], 128_000);
    assert_eq!(seen[1].json()["max_tokens"], 300_000);
    assert_eq!(seen[2].json()["max_tokens"], 20_000);
    assert_eq!(seen[3].json()["max_tokens"], 10_000);
}

#[tokio::test]
async fn every_bad_change_is_rejected_and_changes_nothing() {
    let mut rig = Rig::new(true, OPUS, Effort::Xhigh, answer).await;
    let long = "a".repeat(MAX_MODEL_ID + 1);
    let cases: Vec<(Option<&str>, Option<Effort>, &str)> = vec![
        (Some(""), None, "empty"),
        (Some(&long), None, "longer than 128"),
        (Some("claude opus"), None, "may only hold"),
        (Some("claude-\u{1b}[31m"), None, "may only hold"),
        (
            Some("claude-opus-4-6"),
            Some(Effort::Xhigh),
            "doesn't take effort xhigh",
        ),
        (
            Some("claude-opus-4-6"),
            None,
            "doesn't take the current effort xhigh",
        ),
        (
            Some("claude-opus-4-5"),
            Some(Effort::High),
            "adaptive thinking",
        ),
        (
            Some("claude-sonnet-4-5"),
            Some(Effort::High),
            "adaptive thinking",
        ),
        (Some("claude-haiku-4-5"), None, "adaptive thinking"),
    ];
    for (model, effort, reason) in cases {
        let events = rig.change(model, effort).await;
        let reasons = rejected(&events);
        assert_eq!(reasons.len(), 1, "{model:?}: {events:?}");
        assert!(reasons[0].contains(reason), "{reasons:?} vs {reason}");
        assert!(settings_events(&events).is_empty());
        assert_eq!(rig.agent.pending_settings(), None);
    }
    assert!(check_model_id(&"a".repeat(MAX_MODEL_ID)).is_ok());
    assert!(check_model_id("us.anthropic/claude:opus@5-5_x").is_ok());
    assert_eq!(rig.cache_of(None, None).await, CacheEffect::Kept);
    assert_eq!(
        rig.cache_of(Some(OPUS), Some(Effort::Xhigh)).await,
        CacheEffect::Kept
    );
    assert_eq!(rig.agent.pending_settings(), None);
    let records = log_events(&rig.store);
    assert_eq!(
        records
            .iter()
            .filter(|r| matches!(r, LogEvent::Rejected { .. }))
            .count(),
        9
    );
}

#[tokio::test]
async fn a_later_change_replaces_the_pending_one_and_a_return_clears_it() {
    let mut rig = Rig::new(true, OPUS, Effort::Medium, answer).await;
    rig.change(Some(SONNET), None).await;
    assert_eq!(
        rig.agent.pending_settings(),
        Some((IMPLICIT_ENDPOINT, SONNET, Effort::Medium))
    );
    rig.change(None, Some(Effort::Max)).await;
    assert_eq!(
        rig.agent.pending_settings(),
        Some((IMPLICIT_ENDPOINT, SONNET, Effort::Max))
    );
    let events = rig.change(Some(OPUS), Some(Effort::Medium)).await;
    assert_eq!(settings_events(&events), [CacheEffect::Kept]);
    assert_eq!(rig.agent.pending_settings(), None);
}

#[tokio::test]
async fn spawned_sessions_take_settings_while_idle() {
    let rig = Rig::new(false, OPUS, Effort::Medium, answer).await;
    let store = rig.store.clone();
    let (commands, mut events, _task) = crate::agent::spawn(rig.agent);
    commands
        .send(ClientCommand::shortcut(
            RequestId(3),
            None,
            None,
            Some(Effort::Low),
        ))
        .await
        .unwrap();
    let event = events.recv().await.unwrap();
    assert_eq!(
        event,
        AgentEvent::SettingsChanged {
            withdrawn: None,
            restored: None,
            changes: vec![AppliedChange {
                target: SettingTarget::Name(SettingName::parse("effort").unwrap()),
                value: Some(SettingValue::Text("low".into())),
                layer: Layer::Session,
                when: When::NextTurn,
                cache: Some(CacheEffect::KeptIfPassedOn),
                consequence: None,
                saved: None,
                overridden: None,
                warnings: Vec::new(),
            }],
            answers: Some(RequestId(3)),
            endpoint: IMPLICIT_ENDPOINT.into(),
            model: OPUS.into(),
            effort: Effort::Low,
            cache: CacheEffect::KeptIfPassedOn,
            account_bound: Vec::new(),
        }
    );
    drop(commands);
    while events.recv().await.is_some() {}
    let kinds: Vec<bool> = log_events(&store)
        .iter()
        .map(|e| matches!(*e, LogEvent::SettingsChanged { .. }))
        .collect();
    assert_eq!(kinds.iter().filter(|k| **k).count(), 1);
}

#[test]
fn the_layout_check_takes_markers_and_tool_parts_after_a_user_message_and_refuses_other_system_messages()
 {
    let provider = dummy();
    let marker = Message::effort(EffortMarker {
        level: Effort::Low,
        rendered: true,
    });
    let assistant = Message {
        role: Role::Assistant,
        content: vec![ContentBlock::Text { text: "a".into() }],
    };
    let good = [
        Message::user_text("q"),
        marker.clone(),
        assistant.clone(),
        Message::user_text("r"),
        marker.clone(),
    ];
    assert!(check_layout(&good, &[], &Default::default(), &provider).is_ok());
    let first = [marker.clone(), Message::user_text("q")];
    assert!(check_layout(&first, &[], &Default::default(), &provider).is_err());
    let text = Message {
        role: Role::System,
        content: vec![ContentBlock::Text { text: "x".into() }],
    };
    let sentence = [Message::user_text("q"), text.clone()];
    assert!(check_layout(&sentence, &[], &Default::default(), &provider).is_ok());
    let stray = [Message::user_text("q"), assistant.clone(), text.clone()];
    assert!(check_layout(&stray, &[], &Default::default(), &provider).is_err());
    let between = [
        Message::user_text("q"),
        assistant.clone(),
        text,
        Message::user_text("r"),
    ];
    let refused = check_layout(&between, &[], &Default::default(), &provider).unwrap_err();
    assert_eq!(refused.message, Some(2));
    assert!(
        refused.problem.contains("doesn't follow a user message"),
        "{}",
        refused.problem
    );
    let other = [
        Message::user_text("q"),
        Message {
            role: Role::System,
            content: vec![ContentBlock::ToolResult {
                tool_use_id: "t".into(),
                content: Vec::new(),
                is_error: false,
            }],
        },
    ];
    assert!(check_layout(&other, &[], &Default::default(), &provider).is_err());
    let empty = [
        Message::user_text("q"),
        Message {
            role: Role::System,
            content: Vec::new(),
        },
    ];
    assert!(check_layout(&empty, &[], &Default::default(), &provider).is_err());
}

#[tokio::test]
async fn a_pending_change_kept_if_passed_on_is_corrected_when_its_pair_falls_back() {
    let mut rig = Rig::new(false, SONNET, Effort::Medium, |seen, n| match n {
        0 => reply(
            seen,
            n,
            &[
                Blk::Think,
                Blk::Call("sleep", json!({"ms": 50, "text": "x"})),
            ],
            "tool_use",
        ),
        1 => chunk_form(DOC_EFFORT_BETA),
        _ => answer(seen, n),
    })
    .await;
    let change = vec![ClientCommand::shortcut(
        RequestId(4),
        None,
        None,
        Some(Effort::Low),
    )];
    let (outcome, events) = rig.turn_with("loop", Some((on_started, change))).await;
    assert!(completed(&outcome), "{outcome:?}");
    let changes: Vec<(Effort, CacheEffect)> = pick!(
        &events,
        AgentEvent::SettingsChanged { effort, cache, .. } => (*effort, cache.clone())
    );
    assert_eq!(
        changes,
        [
            (Effort::Low, kept_if_passed_on()),
            (
                Effort::Low,
                rebuilt(RebuildReason::PerMessageEffortRejected)
            ),
        ]
    );
    let seen = rig.seen();
    assert_eq!(seen.len(), 3);
    assert!(seen[1].has_beta(PER_MESSAGE_EFFORT_BETA));
    assert!(!seen[2].has_beta(PER_MESSAGE_EFFORT_BETA));
    assert_eq!(
        rig.agent.pending_settings(),
        Some((IMPLICIT_ENDPOINT, SONNET, Effort::Low))
    );
}

#[test]
fn an_unknown_beta_error_naming_a_non_trial_beta_never_matches() {
    use crate::provider::anthropic::{ErrorForm, rejected_features};
    let both = [Feature::PerMessageEffort, Feature::BindingControls];
    let message =
        "unknown beta: server-side-fallback-2026-07-01, mid-conversation-output-config-2026-07-01";
    for form in [ErrorForm::Http400, ErrorForm::Json, ErrorForm::Chunk] {
        assert!(
            rejected_features(message, form, Features::trying(&both)).is_empty(),
            "{form:?}"
        );
    }
}

fn on_step(e: &AgentEvent) -> bool {
    matches!(e, AgentEvent::StepStarted { .. })
}

fn delayed_rejection(seen: &Seen, n: usize) -> String {
    match n {
        0 => format!("{DELAY}{}", http_400(EXTRA_OUTPUT_CONFIG)),
        _ => answer(seen, n),
    }
}

fn corrections(events: &[AgentEvent]) -> Vec<(String, Effort, CacheEffect)> {
    pick!(
        events,
        AgentEvent::SettingsChanged {
            model,
            effort,
            cache,
            ..
        } => (model.clone(), *effort, cache.clone())
    )
}

#[tokio::test]
async fn a_pending_switch_to_another_model_needs_no_correction() {
    let mut rig = Rig::new(false, SONNET, Effort::Medium, delayed_rejection).await;
    rig.change(None, Some(Effort::Low)).await;
    let switch = vec![ClientCommand::shortcut(
        RequestId(5),
        None,
        Some(OPUS.into()),
        None,
    )];
    let (outcome, events) = rig.turn_with("one", Some((on_step, switch))).await;
    assert!(completed(&outcome), "{outcome:?}");
    assert_eq!(rig.seen().len(), 2);
    assert_eq!(
        announced(&events),
        [(
            wisp_protocol::Feature::PerMessageEffort,
            SONNET.to_owned(),
            FeatureState::FallenBack
        )]
    );
    assert_eq!(
        corrections(&events),
        [(
            OPUS.to_owned(),
            Effort::Low,
            rebuilt(RebuildReason::ModelSwitch)
        )]
    );
    assert_eq!(
        rig.agent.pending_settings(),
        Some((IMPLICIT_ENDPOINT, OPUS, Effort::Low))
    );
    assert_eq!(
        rig.agent
            .history()
            .iter()
            .filter_map(Message::effort_marker)
            .collect::<Vec<_>>(),
        [EffortMarker {
            level: Effort::Low,
            rendered: false
        }]
    );
    rig.rebuilt_matches_history();
}

#[tokio::test]
async fn a_correction_carries_the_pending_effort() {
    let mut rig = Rig::new(false, SONNET, Effort::Medium, delayed_rejection).await;
    rig.change(None, Some(Effort::Low)).await;
    let change = vec![ClientCommand::shortcut(
        RequestId(6),
        None,
        None,
        Some(Effort::High),
    )];
    let (outcome, events) = rig.turn_with("one", Some((on_step, change))).await;
    assert!(completed(&outcome), "{outcome:?}");
    assert_eq!(rig.seen().len(), 2);
    assert_eq!(
        corrections(&events),
        [
            (SONNET.to_owned(), Effort::High, kept_if_passed_on()),
            (
                SONNET.to_owned(),
                Effort::High,
                rebuilt(RebuildReason::PerMessageEffortRejected)
            ),
        ]
    );
    assert_eq!(
        rig.agent.pending_settings(),
        Some((IMPLICIT_ENDPOINT, SONNET, Effort::High))
    );
    rig.rebuilt_matches_history();
}

mod switch;

#[tokio::test]
async fn a_feature_trial_fallback_is_not_counted_as_a_retry() {
    let mut rig = Rig::new(false, SONNET, Effort::Medium, |seen, n| {
        if seen.binding() {
            return http_400(EXTRA_BINDING);
        }
        if !seen.markers().is_empty() {
            return chunk_form(EXTRA_OUTPUT_CONFIG);
        }
        if n == 2 {
            return http_529();
        }
        answer(seen, n)
    })
    .await;
    rig.agent.retry = crate::agent::retry::RetryPolicy {
        step_retries: Some(1),
        turn_retries: Some(1),
        ..crate::agent::retry::quick()
    };
    rig.agent.settings.binding_controls = true;
    rig.change(None, Some(Effort::Low)).await;
    let (outcome, events) = rig.turn("one").await;
    assert!(completed(&outcome), "{outcome:?}");
    let seen = rig.seen();
    assert_eq!(seen.len(), 4);
    assert_eq!(
        seen[2].body, seen[3].body,
        "the retry re-sends the fallen-back request"
    );
    let retries: Vec<u32> = pick!(
        &events,
        AgentEvent::Retrying { attempt, step, .. } => *attempt * 100 + *step
    );
    assert_eq!(
        retries,
        [101],
        "two fallbacks then one retry, all in step 1"
    );
    assert_eq!(costs(&events).len(), 1);
    assert_eq!(rig.agent.last_turn_stats().unwrap().retries, 1);
}

#[test]
fn model_validation_substitutes_the_default_for_each_provider_and_keeps_gateway_ids() {
    use crate::endpoints::{EndpointName, PricingMode, ProviderKind, ResolvedEndpoint, UrlSource};
    use crate::provider::credential::Source;
    let endpoint = ResolvedEndpoint {
        name: EndpointName::parse("openai").unwrap(),
        provider: ProviderKind::OpenAi,
        credential: Credential::ApiKey("synthetic".into()),
        base_url: "http://127.0.0.1:1".into(),
        source: Source::Environment,
        key_env: None,
        url_source: UrlSource::Official,
        pricing: PricingMode::List,
    };
    let openai = crate::provider::openai::OpenAiProvider::from_endpoint(&endpoint).unwrap();
    let agent = Agent::new(Arc::new(openai), default_session());
    assert_eq!(
        agent
            .validate_settings(None, None, Some(Effort::Medium))
            .unwrap()
            .model,
        wisp_protocol::settings::DEFAULT_MODEL
    );
    let anthropic = AnthropicProvider::new(
        &Credential::ApiKey("synthetic".into()),
        crate::provider::anthropic::DEFAULT_BASE_URL,
    )
    .unwrap();
    let mut config = default_session();
    config.model = "gpt-oss-gateway".into();
    let mut agent = Agent::new(Arc::new(anthropic), config);
    assert_eq!(
        agent.validate_settings(None, None, None).unwrap().model,
        "gpt-oss-gateway"
    );
    agent.session.model = "gpt-6.1-sol".into();
    assert_eq!(
        agent.validate_settings(None, None, None).unwrap().model,
        "gpt-6.1-sol"
    );
    assert!(
        agent
            .validate_settings(None, Some("gpt-6.1-sol".into()), None)
            .is_err()
    );
    assert_eq!(
        agent
            .validate_settings(None, Some("gateway-model".into()), None)
            .unwrap()
            .model,
        "gateway-model"
    );
    let gateway = AnthropicProvider::new(
        &Credential::ApiKey("synthetic".into()),
        "http://127.0.0.1:1",
    )
    .unwrap();
    let gw_agent = Agent::new(Arc::new(gateway), default_session());
    assert_eq!(
        gw_agent
            .validate_settings(None, Some("gpt-6.1-sol".into()), None)
            .unwrap()
            .model,
        "gpt-6.1-sol"
    );
    assert_eq!(
        gw_agent
            .validate_settings(None, Some("gemini-4-carbon".into()), None)
            .unwrap()
            .model,
        "gemini-4-carbon"
    );
    let endpoint = ResolvedEndpoint {
        name: EndpointName::parse("gemini").unwrap(),
        provider: ProviderKind::Gemini,
        base_url: ProviderKind::Gemini.default_url().to_owned(),
        ..endpoint
    };
    let provider = endpoint.provider.connect(&endpoint, None).unwrap();
    let config = SessionConfig {
        model: "gemini-4-carbon-high".into(),
        ..default_session()
    };
    let mut agent = Agent::new(provider.clone(), config.clone());
    assert!(
        agent
            .validate_settings(None, Some("gemini-4-carbon".into()), None)
            .is_ok()
    );
    agent.session.model = "gemini-99-missing".into();
    assert_eq!(
        agent.validate_settings(None, None, None).unwrap().model,
        "gemini-99-missing"
    );
    let sibling = agent
        .validate_settings(None, None, Some(Effort::Low))
        .unwrap();
    assert_eq!(
        (sibling.model.as_str(), sibling.effort),
        ("gemini-99-missing", Effort::Low)
    );
    agent.session.model = "gemini-4-carbon-high".into();
    let sibling = agent
        .validate_settings(None, None, Some(Effort::Low))
        .unwrap();
    assert_eq!(
        (sibling.model.as_str(), sibling.effort),
        ("gemini-4-carbon-low", Effort::Low)
    );
    agent.session.model = "gemini-4-hydrogen-high".into();
    for effort in [Effort::Low, Effort::Medium, Effort::High, Effort::Xhigh] {
        let sibling = agent.validate_settings(None, None, Some(effort)).unwrap();
        assert_eq!(
            (sibling.model, sibling.effort),
            (format!("gemini-4-hydrogen-{}", effort.as_str()), effort)
        );
    }
    assert_eq!(
        agent
            .validate_settings(None, None, Some(Effort::Max))
            .unwrap_err(),
        "gemini-4-hydrogen-high doesn't take effort max; it takes low, medium, high, xhigh"
    );
    let dir = tempfile::tempdir().unwrap();
    let saved = crate::models::list::SavedList {
        attempted: 0,
        failed: false,
        list: crate::models::list::ModelList::from_gemini_ids(
            [
                (config.model.clone(), None, None),
                ("gemini-4-carbon".into(), None, None),
            ],
            None,
        )
        .unwrap(),
        default_model: None,
    };
    saved
        .save(dir.path(), provider.list_key().unwrap())
        .unwrap();
    let provider = endpoint
        .provider
        .connect(&endpoint, Some(dir.path()))
        .unwrap();
    let mut agent = Agent::new(provider.clone(), config).with_connections(
        vec![
            crate::agent::Connection {
                name: "gemini".into(),
                provider: provider.clone(),
                context: endpoint.log_context(),
                priced: true,
            },
            crate::agent::Connection {
                name: "gemini-other".into(),
                provider,
                context: endpoint.log_context(),
                priced: true,
            },
        ],
        Vec::new(),
        "gemini",
    );
    assert!(
        agent
            .validate_settings(None, Some("gemini-4-carbon-high".into()), None)
            .is_ok()
    );
    assert_eq!(
        agent.validate_settings(None, None, None).unwrap().model,
        "gemini-4-carbon-high"
    );
    assert_eq!(
        agent
            .validate_settings(None, None, Some(Effort::High))
            .unwrap()
            .effort,
        Effort::High
    );
    assert_eq!(
        agent
            .validate_settings(Some("gemini-other".into()), None, None)
            .unwrap()
            .model,
        "gemini-4-carbon-high"
    );
    assert_eq!(
        agent
            .validate_settings(None, Some("gemini-4-carbon-high".into()), None)
            .unwrap()
            .effort,
        Effort::High
    );
    assert_eq!(
        agent
            .validate_settings(None, None, Some(Effort::Low))
            .unwrap_err(),
        "model gemini-4-carbon-low isn't one account gemini serves"
    );
    for effort in [Effort::Xhigh, Effort::Max] {
        assert_eq!(
            agent
                .validate_settings(None, None, Some(effort))
                .unwrap_err(),
            format!(
                "gemini-4-carbon-high doesn't take effort {}; it takes low, medium, high",
                effort.as_str()
            )
        );
    }
    agent.session.model = "gemini-99-missing".into();
    assert_eq!(
        agent
            .validate_settings(None, None, Some(Effort::Low))
            .unwrap()
            .model,
        "gemini-99-missing"
    );
    let moved = agent
        .validate_settings(Some("gemini-other".into()), None, None)
        .unwrap();
    assert_eq!(moved.model, "gemini-4-carbon");
    assert!(agent.switch_targets(&agent.next_pair()).iter().any(|(target, _, model, _)| matches!(target, wisp_protocol::settings::SwitchTarget::Endpoint(name) if name == "gemini-other") && *model == moved.model));
    for model in ["gemini-4-carbon-medium", "gemini-99-missing"] {
        let reason = agent
            .validate_settings(None, Some(model.into()), None)
            .unwrap_err();
        assert_eq!(
            reason,
            format!("model {model} isn't one account gemini serves")
        );
        assert!(agent.pending_settings().is_none());
    }
}
