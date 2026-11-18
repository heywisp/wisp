use std::sync::{Arc, Mutex};
use std::time::Duration;

use futures::StreamExt;
use futures::stream::BoxStream;
use serde_json::Value;
use tokio::sync::mpsc;
use wisp_protocol::{
    AgentEvent, CacheEffect, ClientCommand, ContentBlock, Effort, MessageTime, TurnOutcome, Usage,
};

use wisp_protocol::RebuildReason;
use wisp_protocol::log::CredentialSource;

use super::PairState;
use crate::agent::{Agent, Connection, LogContext, default_session};
use crate::endpoints::redaction_items;
use crate::provider::anthropic::{AnthropicProvider, Credential, DEFAULT_BASE_URL, Settings};
use crate::provider::{
    Feature, ModelCapabilities, Provider, ProviderError, ProviderEvent, Request,
};
use crate::secrets::live_redactions;
use crate::session::Redactions;
use crate::test_guard::pick;

const SONNET: &str = "claude-sonnet-5-5";

fn padded_system() -> String {
    padded_with(60)
}

fn padded_with(notes: u32) -> String {
    let mut text = String::from(
        "You are a terse test assistant. Whatever you are asked, reply with the single word ok. \
The notes below are reference material that you never need to use.",
    );
    for n in 1..=notes {
        text.push_str(&format!(
            "\nNote {n}: the archive shelf {n} holds ledgers bound in grey cloth, catalogued by \
quarter and cross-referenced to the index cards kept in drawer {n}."
        ));
    }
    text
}

fn live_agent(model: &str, effort: Effort, binding: bool) -> Agent {
    agent_with(model, effort, binding, padded_system(), 2000)
}

fn agent_with(
    model: &str,
    effort: Effort,
    binding: bool,
    system: String,
    max_tokens: u32,
) -> Agent {
    session_agent(
        Arc::new(live_provider()),
        model,
        effort,
        binding,
        system,
        max_tokens,
    )
}

fn live_provider() -> AnthropicProvider {
    let settings = Settings::resolve(|name| std::env::var(name).ok(), None).unwrap();
    AnthropicProvider::from_settings(&settings).unwrap()
}

fn session_agent(
    provider: Arc<dyn Provider>,
    model: &str,
    effort: Effort,
    binding: bool,
    system: String,
    max_tokens: u32,
) -> Agent {
    let mut session = default_session();
    session.model = model.into();
    session.effort = effort;
    session.message_time = MessageTime::Off;
    session.system = Some(system);
    session.max_tokens = max_tokens;
    Agent::new(provider, session).with_binding_controls(binding)
}

struct Live {
    agent: Agent,
    spent: f64,
    budget: f64,
}

struct Turn {
    outcome: TurnOutcome,
    events: Vec<AgentEvent>,
}

impl Turn {
    fn usage(&self) -> Usage {
        match &self.outcome {
            TurnOutcome::Completed { usage, .. } => *usage,
            other => panic!(
                "the turn didn't complete: {}",
                secrets().scrub(&format!("{other:?}"))
            ),
        }
    }

    fn transformations(&self) -> Vec<Value> {
        self.events
            .iter()
            .filter_map(|e| match e {
                AgentEvent::InputTransformations {
                    transformations, ..
                } => serde_json::to_value(transformations).ok(),
                _ => None,
            })
            .collect()
    }

    fn settings(&self) -> Vec<CacheEffect> {
        pick!(&self.events, AgentEvent::SettingsChanged { cache, .. } => cache.clone())
    }
}

impl Live {
    fn new(agent: Agent, budget: f64) -> Self {
        Self {
            agent,
            spent: 0.0,
            budget,
        }
    }

    async fn turn(&mut self, text: &str) -> Turn {
        let (turn, stopped) = self.run(text).await;
        assert!(
            !stopped,
            "the cost guard stopped the turn at ${:.4}",
            self.spent
        );
        turn
    }

    async fn run(&mut self, text: &str) -> (Turn, bool) {
        let (cmd_tx, mut cmd_rx) = mpsc::channel(4);
        let (ev_tx, mut ev_rx) = mpsc::channel(1024);
        let budget = self.budget - self.spent;
        let collector = tokio::spawn(async move {
            let mut events = Vec::new();
            let mut spent = 0.0;
            let mut stopped = false;
            while let Some(e) = ev_rx.recv().await {
                if let AgentEvent::StepEnded { cost_usd, .. } = &e {
                    spent += cost_usd.unwrap_or(0.0);
                    if spent > budget && !stopped {
                        stopped = true;
                        let _ = cmd_tx.send(ClientCommand::Cancel).await;
                    }
                }
                events.push(e);
            }
            (events, spent, stopped)
        });
        let outcome = tokio::time::timeout(
            Duration::from_secs(180),
            self.agent.run_turn(text.into(), &mut cmd_rx, &ev_tx),
        )
        .await
        .unwrap()
        .unwrap();
        drop(ev_tx);
        let (events, spent, stopped) = collector.await.unwrap();
        self.spent += spent;
        eprintln!(
            "live: {text:?} on {} at {} -> {}; spent ${:.4} of ${:.2}",
            self.agent.session().model,
            self.agent.endpoint_name(),
            secrets().scrub(&format!("{outcome:?}")),
            self.spent,
            self.budget
        );
        (Turn { outcome, events }, stopped)
    }

    async fn change(&mut self, model: Option<&str>, effort: Option<Effort>) -> CacheEffect {
        self.change_on(None, model, effort).await
    }

    async fn change_on(
        &mut self,
        endpoint: Option<&str>,
        model: Option<&str>,
        effort: Option<Effort>,
    ) -> CacheEffect {
        let (ev_tx, mut ev_rx) = mpsc::channel(8);
        self.agent
            .change_settings(
                endpoint.map(str::to_owned),
                model.map(str::to_owned),
                effort,
                &ev_tx,
            )
            .await;
        drop(ev_tx);
        let mut caches = Vec::new();
        while let Some(e) = ev_rx.recv().await {
            match e {
                AgentEvent::SettingsChanged { cache, .. } => caches.push(cache),
                other => panic!(
                    "the change was refused: {}",
                    secrets().scrub(&format!("{other:?}"))
                ),
            }
        }
        caches.pop().unwrap()
    }
}

#[tokio::test]
#[ignore = "live: costs real money; run by hand with a direct API key in the environment"]
async fn live_settings_1_2_effort_keeps_the_cache_and_a_switch_rebuilds_it() {
    let mut live = Live::new(live_agent(SONNET, Effort::Low, false), 0.02);
    let first = live.turn("Say ok.").await;
    let before = first.usage();
    let top_before = live.agent.request_parts();
    assert_eq!(
        live.change(None, Some(Effort::Medium)).await,
        CacheEffect::Kept
    );
    let second = live.turn("Say ok again.").await;
    let after = second.usage();
    let top_after = live.agent.request_parts();
    eprintln!("live 1: turn 1 {before:?}; turn 2 {after:?}");
    assert_eq!(
        top_before, top_after,
        "the per-message change altered the top level of the request"
    );
    assert_eq!(top_after.0.effort, Effort::Low);
    assert_eq!(live.agent.session().effort, Effort::Medium);
    let cached = before.cache_read_input_tokens + before.cache_creation_input_tokens;
    assert!(
        after.cache_read_input_tokens >= cached,
        "the per-message change lost the cache: read {} of {cached}",
        after.cache_read_input_tokens
    );
    let switched = live.change(Some("claude-haiku-5-5"), None).await;
    assert!(matches!(switched, CacheEffect::Rebuilt { .. }));
    let third = live.turn("And once more.").await;
    let haiku = third.usage();
    eprintln!("live 2: haiku turn {haiku:?}");
    assert_eq!(haiku.cache_read_input_tokens, 0);
    assert!(haiku.cache_creation_input_tokens > 0);
}

#[tokio::test]
#[ignore = "live: costs real money; run by hand with a direct API key in the environment"]
async fn live_settings_3_switching_back_reports_input_transformations() {
    let mut live = Live::new(live_agent(SONNET, Effort::Low, true), 0.02);
    live.turn("Say ok.").await.usage();
    live.change(None, Some(Effort::Medium)).await;
    live.turn("Say ok again.").await.usage();
    live.change(Some("claude-sonnet-5"), None).await;
    let away = live.turn("Once more.").await;
    away.usage();
    live.change(Some(SONNET), None).await;
    let back = live.turn("And back again.").await;
    back.usage();
    let away_list = away.transformations();
    let list = back.transformations();
    eprintln!("live 3: on claude-sonnet-5 input_transformations = {away_list:?}");
    eprintln!("live 3: on the return input_transformations = {list:?}");
    assert!(
        !list.is_empty(),
        "the return carried no input_transformations"
    );
    let mismatch = list
        .iter()
        .flat_map(|l| l.as_array().cloned().unwrap_or_default())
        .any(|entry| entry["reason"] == "prefix_binding_mismatch");
    eprintln!(
        "live 3 verdict: {}",
        if mismatch {
            "prefix_binding_mismatch on the return: R24's drop_block fallback is needed"
        } else {
            "no prefix_binding_mismatch on the return: R24 is closed"
        }
    );
}

const TINY_PROMPT_TOKENS_MAX: f64 = 100.0;
const TINY_MAX_TOKENS: u32 = 128;
const SONNET_INPUT_PER_TOKEN: f64 = 2.0 / 1e6;
const SONNET_OUTPUT_PER_TOKEN: f64 = 10.0 / 1e6;
const ONE_HOUR_WRITE: f64 = 2.0;

type Errors = Arc<Mutex<Vec<String>>>;

struct Recording {
    inner: AnthropicProvider,
    errors: Errors,
}

impl Provider for Recording {
    fn stream(
        &self,
        request: &Request<'_>,
    ) -> BoxStream<'static, Result<ProviderEvent, ProviderError>> {
        let errors = self.errors.clone();
        self.inner
            .stream(request)
            .inspect(move |item| {
                if let Err(e) = item {
                    errors.lock().unwrap().push(e.message.clone());
                }
            })
            .boxed()
    }

    fn echoed<'a>(
        &self,
        content: &'a [ContentBlock],
        earlier: &crate::provider::ServerCalls,
    ) -> Vec<&'a ContentBlock> {
        self.inner.echoed(content, earlier)
    }

    fn search_signal(&self, block: &ContentBlock) -> Option<crate::provider::SearchSignal> {
        self.inner.search_signal(block)
    }

    fn pause_limit(&self) -> u32 {
        self.inner.pause_limit()
    }

    fn capabilities(&self, model: &str) -> Option<ModelCapabilities> {
        self.inner.capabilities(model)
    }

    fn features_documented(&self) -> bool {
        self.inner.features_documented()
    }
    fn account_bound_thinking(&self, model: &str) -> bool {
        self.inner.account_bound_thinking(model)
    }

    fn cache_prefix(&self, request: &Request<'_>) -> Option<String> {
        self.inner.cache_prefix(request)
    }
}

fn secrets() -> Redactions {
    live_redactions(&|name| std::env::var(name).ok())
}

fn tiny_agent(binding: bool) -> (Agent, Errors) {
    let errors = Errors::default();
    let provider = Recording {
        inner: live_provider(),
        errors: errors.clone(),
    };
    let agent = session_agent(
        Arc::new(provider),
        SONNET,
        Effort::Low,
        binding,
        "Reply with the single word ok.".to_owned(),
        TINY_MAX_TOKENS,
    );
    (agent, errors)
}

fn tiny_request_worst_case() -> f64 {
    TINY_PROMPT_TOKENS_MAX * SONNET_INPUT_PER_TOKEN * ONE_HOUR_WRITE
        + f64::from(TINY_MAX_TOKENS) * SONNET_OUTPUT_PER_TOKEN
}

fn requests_sent(turn: &Turn, state: PairState) -> usize {
    let retries = turn
        .events
        .iter()
        .filter(|e| matches!(e, AgentEvent::Retrying { .. }))
        .count();
    1 + retries + usize::from(state == PairState::FallenBack)
}

async fn gateway_probe(binding: bool, feature: Feature) -> (PairState, f64, bool) {
    let budget = 2.0 * tiny_request_worst_case();
    let (agent, errors) = tiny_agent(binding);
    let mut live = Live::new(agent, budget);
    if feature == Feature::PerMessageEffort {
        let pending = live.change(None, Some(Effort::Medium)).await;
        eprintln!("live 4: an effort change through the gateway reports {pending:?}");
    }
    let (turn, stopped) = live.run("Say ok.").await;
    let state = live
        .agent
        .pair_state(live.agent.endpoint_name(), SONNET, feature);
    let secrets = secrets();
    for text in errors.lock().unwrap().iter() {
        eprintln!("live 4: {feature:?} error text: {}", secrets.scrub(text));
    }
    eprintln!(
        "live 4: {feature:?} is {state:?}; requests sent {}; spent ${:.5} of a ${budget:.5} guard; \
corrections {:?}; input_transformations {}; outcome {}",
        requests_sent(&turn, state),
        live.spent,
        turn.settings(),
        secrets.scrub(&format!("{:?}", turn.transformations())),
        secrets.scrub(&format!("{:?}", turn.outcome))
    );
    (state, live.spent, stopped)
}

#[tokio::test]
#[ignore = "live: costs real money; run by hand with gateway credentials in the environment"]
async fn live_settings_4_the_gateway_passes_each_beta_on_or_falls_back() {
    let (effort, effort_spent, effort_stopped) =
        gateway_probe(false, Feature::PerMessageEffort).await;
    let (binding, binding_spent, binding_stopped) =
        gateway_probe(true, Feature::BindingControls).await;
    eprintln!("live 4 total: ${:.5}", effort_spent + binding_spent);
    assert!(!effort_stopped && !binding_stopped, "a cost guard tripped");
    assert!(matches!(effort, PairState::Proven | PairState::FallenBack));
    assert!(matches!(binding, PairState::Proven | PairState::FallenBack));
}

const DIRECT: &str = "direct";
const MANIFEST: &str = "manifest";

fn live_connection(name: &str, credential: Credential, base: &str) -> Connection {
    let provider = AnthropicProvider::new(&credential, base).unwrap();
    let origin = base.split('/').take(3).collect::<Vec<_>>().join("/");
    let context = LogContext {
        endpoint: origin,
        endpoint_name: name.to_owned(),
        credential_source: CredentialSource::Environment,
        redactions: redaction_items(&credential, base),
    };
    Connection {
        name: name.to_owned(),
        provider: Arc::new(provider),
        context,
        priced: true,
    }
}

fn two_endpoints() -> Agent {
    let var = |name: &str| {
        std::env::var(name)
            .ok()
            .filter(|v| !v.trim().is_empty())
            .unwrap_or_else(|| panic!("{name} is not set"))
    };
    let direct = live_connection(
        DIRECT,
        Credential::ApiKey(var("ANTHROPIC_API_KEY")),
        DEFAULT_BASE_URL,
    );
    let manifest = live_connection(
        MANIFEST,
        Credential::AuthToken(var("MANIFEST_API_KEY")),
        &var("MANIFEST_BASE_URL"),
    );
    let first = direct.provider.clone();
    session_agent(first, SONNET, Effort::Low, false, padded_with(10), 2000).with_connections(
        vec![direct, manifest],
        Vec::new(),
        DIRECT,
    )
}

fn states(agent: &Agent, endpoint: &str) -> String {
    format!(
        "per-message effort {:?}, binding controls {:?}",
        agent.pair_state(endpoint, SONNET, Feature::PerMessageEffort),
        agent.pair_state(endpoint, SONNET, Feature::BindingControls)
    )
}

fn report(n: u32, endpoint: &str, live: &Live, turn: &Turn) -> Usage {
    let usage = turn.usage();
    eprintln!(
        "live 5 turn {n} on {endpoint}: input {} cache read {} cache write {} (1h {:?}) output {}; \
{}; input_transformations {}",
        usage.input_tokens,
        usage.cache_read_input_tokens,
        usage.cache_creation_input_tokens,
        usage.cache_creation_1h_input_tokens,
        usage.output_tokens,
        states(&live.agent, endpoint),
        secrets().scrub(&format!("{:?}", turn.transformations()))
    );
    usage
}

#[tokio::test]
#[ignore = "live: costs real money; run by hand with ANTHROPIC_API_KEY, MANIFEST_API_KEY and MANIFEST_BASE_URL in the environment"]
async fn live_settings_5_a_session_bounces_between_two_endpoints() {
    let mut live = Live::new(two_endpoints(), 0.05);
    let prompt = "Think briefly, then say ok.";
    let first = live.turn(prompt).await;
    let one = report(1, DIRECT, &live, &first);
    for (n, endpoint) in [(2, MANIFEST), (3, DIRECT), (4, MANIFEST)] {
        let cache = live.change_on(Some(endpoint), None, None).await;
        assert_eq!(
            cache,
            CacheEffect::Rebuilt {
                reason: RebuildReason::EndpointSwitch
            }
        );
        let turn = live.turn(prompt).await;
        let usage = report(n, endpoint, &live, &turn);
        if n == 3 {
            assert!(
                usage.cache_read_input_tokens > 0,
                "back on {DIRECT}, turn 1's prefix wasn't read from cache ({} written then)",
                one.cache_creation_input_tokens
            );
        }
    }
    eprintln!("live 5 total: ${:.5}", live.spent);
}
