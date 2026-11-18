#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    reason = "a live check fails the test on any setup or network error"
)]

use crate::live;
use crate::pick;

use std::sync::Arc;

use futures::StreamExt;
use live::turn_lines::with_turn_lines;
use live::{live_session, provider};
use pick::pick;
use tokio::sync::mpsc;
use wisp_core::agent::Agent;
use wisp_core::agent::retry::RetryPolicy;
use wisp_core::provider::{FeatureUse, Features, Provider, Request};
use wisp_core::search::SearchForm;
use wisp_protocol::{AgentEvent, ContentBlock, Effort, Message, SearchEnd, TurnOutcome};

const MODEL: &str = "claude-haiku-5-5";
const MAX_TOKENS: u32 = 1024;
const CHECK_TOKENS: u32 = 16;
const FIRST: &str = "Use web search exactly once to find the latest stable Rust release, then answer in one short sentence.";
const SECOND: &str = "In three words, what did you search for? Don't search again.";
const BREAKPOINT_TOKENS: u32 = 256;
const BREAKPOINTS: &str = "Make exactly two web searches, one for \"tokio latest release\" and one for \"serde latest release\", then answer in one line.";
const PROJECT_BLOCK: &str = "Project instructions: keep answers to one line.";

async fn turn(agent: &mut Agent, text: &str) -> (TurnOutcome, Vec<AgentEvent>) {
    let (_commands, mut rx) = mpsc::channel(8);
    let (tx, mut events) = mpsc::channel(8192);
    let outcome = agent.run_turn(text.into(), &mut rx, &tx).await.unwrap();
    drop(tx);
    let mut seen = Vec::new();
    while let Some(event) = events.recv().await {
        seen.push(event);
    }
    (outcome, seen)
}

fn finished_searches(events: &[AgentEvent]) -> usize {
    events
        .iter()
        .filter(|e| {
            matches!(
                e,
                AgentEvent::SearchEnded {
                    outcome: SearchEnd::Done(_),
                    ..
                }
            )
        })
        .count()
}

fn cost_of(outcome: &TurnOutcome) -> String {
    match outcome {
        TurnOutcome::Completed {
            cost: Some(cost), ..
        } => format!(
            "turn ${:.4}, session ${:.4}",
            cost.turn_usd, cost.session_usd
        ),
        other => format!("no cost: {other:?}"),
    }
}

fn step_usage(events: &[AgentEvent]) -> Vec<wisp_protocol::Usage> {
    events
        .iter()
        .filter_map(|e| match e {
            AgentEvent::StepEnded { usage, .. } => *usage,
            _ => None,
        })
        .collect()
}

fn retried(events: &[AgentEvent]) -> Vec<String> {
    pick!(events, AgentEvent::Retrying { kind, reason, .. } => format!("{kind:?}: {reason}"))
}

fn without_thinking(history: &[Message]) -> Vec<Message> {
    history
        .iter()
        .map(|m| Message {
            role: m.role,
            content: m
                .content
                .iter()
                .filter(|b| {
                    !matches!(
                        b,
                        ContentBlock::Thinking { .. } | ContentBlock::RedactedThinking { .. }
                    )
                })
                .cloned()
                .collect(),
        })
        .filter(|m| !m.content.is_empty())
        .collect()
}

#[tokio::test]
#[ignore = "live: costs real money; run by hand with credentials in the environment"]
async fn live_native_web_search_on_the_providers_own_origin() {
    let provider = Arc::new(provider());
    if !provider.features_documented() {
        eprintln!(
            "live: not run: ANTHROPIC_BASE_URL is not the provider's own origin; live_native_web_search_through_a_gateway covers gateways"
        );
        return;
    }
    let mut agent = with_turn_lines(Agent::new(
        provider.clone(),
        live_session(MODEL, MAX_TOKENS),
    ))
    .with_web_search(true);
    let form = agent.search_form(agent.endpoint_name(), MODEL);
    if form != SearchForm::Native {
        eprintln!(
            "live: not run: {MODEL} has no native web search here ({form:?}); use the provider's own origin, with no gateway in ANTHROPIC_BASE_URL"
        );
        return;
    }
    let mut summary: Vec<String> = Vec::new();

    let (outcome, first) = turn(&mut agent, FIRST).await;
    let searches = finished_searches(&first);
    let refused = first.iter().any(|e| {
        matches!(
            e,
            AgentEvent::FeatureStateChanged {
                feature: wisp_protocol::Feature::NativeWebSearch,
                ..
            }
        )
    });
    let completed = matches!(outcome, TurnOutcome::Completed { .. });
    summary.push(format!(
        "search completed: {} ({searches} finished; {}; usage {:?})",
        if completed && searches >= 1 && !refused {
            "yes"
        } else {
            "NO"
        },
        cost_of(&outcome),
        step_usage(&first)
    ));
    if refused {
        summary.push("search completed: web search is switched off for this account".into());
    }

    let (outcome, second) = turn(&mut agent, SECOND).await;
    summary.push(format!(
        "follow-up accepted: {} ({}; searches {})",
        if matches!(outcome, TurnOutcome::Completed { .. }) {
            "yes"
        } else {
            "NO"
        },
        cost_of(&outcome),
        finished_searches(&second)
    ));

    let mut check = live_session(MODEL, MAX_TOKENS);
    check.max_tokens = CHECK_TOKENS;
    let mut messages = without_thinking(&agent.history());
    messages.push(Message::user_text("Reply with: ok"));
    let request = Request {
        session: &check,
        tools: &[],
        deferred: &[],
        by_value: &[],
        server_tools: &[],
        messages: &messages,
        previous_response_id: None,
        features: Features::default(),
        options: Default::default(),
        rendering: &wisp_core::provider::UNRENDERED,
        compaction: None,
    };
    let items: Vec<_> = provider.stream(&request).collect().await;
    summary.push(match items.iter().find_map(|i| i.as_ref().err()) {
        None => "plan A outcome: accepted: completed native blocks sent with no server tool".into(),
        Some(error) => format!(
            "plan A outcome: REFUSED, rule R is needed: {:?}: {}",
            error.kind, error.message
        ),
    });

    let mut session = live_session(MODEL, MAX_TOKENS);
    session.max_tokens = BREAKPOINT_TOKENS;
    session.instructions = Some(PROJECT_BLOCK.to_owned());
    let no_retries = RetryPolicy {
        step_retries: Some(0),
        turn_retries: Some(0),
        ..RetryPolicy::default()
    };
    let mut marked = with_turn_lines(Agent::new(provider.clone(), session))
        .with_web_search(true)
        .with_retry_policy(no_retries);
    let (outcome, events) = turn(&mut marked, BREAKPOINTS).await;
    let usage = step_usage(&events);
    let searched = finished_searches(&events);
    summary.push(match &outcome {
        TurnOutcome::Completed { .. } => format!(
            "breakpoint outcome: answered with 3 markers and {searched} searches; {}; usage {usage:?}",
            cost_of(&outcome)
        ),
        TurnOutcome::Error {
            kind,
            message,
            detail,
            ..
        } => format!(
            "breakpoint outcome: REFUSED after {searched} searches: {kind:?}: {message:?} (phase {:?}, status {:?}); retries {:?}; usage {usage:?}",
            detail.as_ref().map(|d| d.phase),
            detail.as_ref().and_then(|d| d.status),
            retried(&events)
        ),
        other => format!("breakpoint outcome: {other:?}"),
    });

    for line in &summary {
        eprintln!("live: {line}");
    }
}

fn holds_thinking(history: &[Message]) -> bool {
    history.iter().any(|m| {
        m.content.iter().any(|b| {
            matches!(
                b,
                ContentBlock::Thinking { .. } | ContentBlock::RedactedThinking { .. }
            )
        })
    })
}

#[tokio::test]
#[ignore = "live: costs real money; run by hand with credentials in the environment"]
async fn live_web_search_turned_off_after_a_search_keeps_the_thinking_bound() {
    let provider = Arc::new(provider());
    if !provider.features_documented() {
        eprintln!("live: not run: ANTHROPIC_BASE_URL is not the provider's own origin");
        return;
    }
    let mut searching = live_session(MODEL, MAX_TOKENS);
    searching.effort = Effort::Medium;
    let mut agent =
        with_turn_lines(Agent::new(provider.clone(), searching.clone())).with_web_search(true);
    let form = agent.search_form(agent.endpoint_name(), MODEL);
    if form != SearchForm::Native {
        eprintln!("live: not run: {MODEL} has no native web search here ({form:?})");
        return;
    }
    let (outcome, first) = turn(&mut agent, FIRST).await;
    let searches = finished_searches(&first);
    let thinking = holds_thinking(&agent.history());
    eprintln!(
        "live: search turn: {searches} finished; thinking in the history: {}; {}",
        if thinking { "yes" } else { "NO" },
        cost_of(&outcome)
    );
    let mut check = searching;
    check.max_tokens = CHECK_TOKENS;
    let mut messages = agent.history().to_vec();
    messages.push(Message::user_text("Reply with: ok"));
    let request = Request {
        session: &check,
        tools: &[],
        deferred: &[],
        by_value: &[],
        server_tools: &[],
        messages: &messages,
        previous_response_id: None,
        features: Features {
            binding_controls: Some(FeatureUse::Established),
            documented: true,
            ..Features::default()
        },
        options: Default::default(),
        rendering: &wisp_core::provider::UNRENDERED,
        compaction: None,
    };
    let items: Vec<_> = provider.stream(&request).collect().await;
    let verdict = match (
        items.iter().find_map(|i| i.as_ref().err()),
        thinking,
        searches,
    ) {
        (Some(error), _, _) => format!(
            "REFUSED: {:?}: {}; the flip must take the whole thinking cut",
            error.kind, error.message
        ),
        (None, true, n) if n >= 1 => {
            "accepted with thinking and native blocks in view: 13d stands".to_owned()
        }
        (None, _, _) => "UNPROVEN: no thinking or no finished search in the history".to_owned(),
    };
    eprintln!("live: web search off: {verdict}");
}

#[tokio::test]
#[ignore = "live: costs real money; run by hand with credentials in the environment"]
async fn live_a_client_call_named_like_the_search_tool_is_accepted_beside_it() {
    let provider = provider();
    let mut session = live_session(MODEL, MAX_TOKENS);
    session.max_tokens = CHECK_TOKENS * 4;
    let messages = vec![
        Message::user_text("What is the latest stable Rust release?"),
        Message {
            role: wisp_protocol::Role::Assistant,
            content: vec![ContentBlock::ToolUse {
                id: "toolu_01NotRun".into(),
                name: "web_search".into(),
                input: serde_json::json!({"query": "latest stable Rust release"}),
            }],
        },
        Message {
            role: wisp_protocol::Role::User,
            content: vec![ContentBlock::ToolResult {
                tool_use_id: "toolu_01NotRun".into(),
                content: vec![ContentBlock::Text {
                    text: format!("Invalid input: {}", wisp_core::tool::SEARCH_NOT_RUN),
                }],
                is_error: true,
            }],
        },
    ];
    let request = Request {
        session: &session,
        tools: &[],
        deferred: &[],
        by_value: &[],
        server_tools: std::slice::from_ref(wisp_core::search::tool("web_search_20250305").unwrap()),
        messages: &messages,
        previous_response_id: None,
        features: Features::default(),
        options: Default::default(),
        rendering: &wisp_core::provider::UNRENDERED,
        compaction: None,
    };
    let items: Vec<_> = provider.stream(&request).collect().await;
    if let Some(error) = items.iter().find_map(|i| i.as_ref().err()) {
        panic!(
            "live: a client web_search call beside the server tool was refused: {:?}: {}",
            error.kind, error.message
        );
    }
    eprintln!("live: a client web_search call beside the server tool was accepted");
}

#[tokio::test]
#[ignore = "live: costs real money; run by hand with a gateway's credentials in the environment"]
async fn live_native_web_search_through_a_gateway() {
    let provider = Arc::new(provider());
    if provider.features_documented() {
        eprintln!(
            "live: not run: ANTHROPIC_BASE_URL is the provider's own origin; export a gateway's ANTHROPIC_AUTH_TOKEN and ANTHROPIC_BASE_URL"
        );
        return;
    }
    let mut agent = with_turn_lines(Agent::new(
        provider.clone(),
        live_session(MODEL, MAX_TOKENS),
    ))
    .with_web_search(true);
    let form = agent.search_form(agent.endpoint_name(), MODEL);
    assert_eq!(
        form,
        SearchForm::Native,
        "a gateway sends search by default: {form:?}"
    );
    let (outcome, events) = turn(&mut agent, FIRST).await;
    let searches = finished_searches(&events);
    let refused = events.iter().any(|e| {
        matches!(
            e,
            AgentEvent::FeatureStateChanged {
                feature: wisp_protocol::Feature::NativeWebSearch,
                ..
            }
        )
    });
    let completed = matches!(outcome, TurnOutcome::Completed { .. });
    eprintln!(
        "live: gateway search: {} ({searches} finished; {}; usage {:?})",
        match (completed, refused, searches) {
            (true, false, n) if n >= 1 => "ran",
            (true, true, _) => "refused by the gateway, turn completed without search",
            (true, false, _) => "NOT RUN: the turn completed with no search",
            _ => "NO",
        },
        cost_of(&outcome),
        step_usage(&events)
    );
    assert!(completed, "{outcome:?}");
}

#[tokio::test]
#[ignore = "live: costs real money; run by hand with OPENAI_API_KEY in the environment"]
async fn live_native_web_search_on_openai() {
    use wisp_core::endpoints::{
        EndpointName, PricingMode, ProviderKind, ResolvedEndpoint, UrlSource,
    };
    use wisp_core::provider::credential::{Credential, Source};
    use wisp_core::provider::openai::{DEFAULT_BASE_URL, DEFAULT_MODEL, OpenAiProvider};
    let Ok(key) = std::env::var("OPENAI_API_KEY") else {
        eprintln!("live: not run: OPENAI_API_KEY is not set");
        return;
    };
    let endpoint = ResolvedEndpoint {
        name: EndpointName::parse("openai").unwrap(),
        provider: ProviderKind::OpenAi,
        credential: Credential::ApiKey(key),
        base_url: DEFAULT_BASE_URL.into(),
        source: Source::Environment,
        key_env: None,
        url_source: UrlSource::Official,
        pricing: PricingMode::List,
    };
    let provider = Arc::new(OpenAiProvider::from_endpoint(&endpoint).unwrap());
    let session = live_session(DEFAULT_MODEL, MAX_TOKENS);
    let mut agent = with_turn_lines(Agent::new(provider, session)).with_web_search(true);
    let form = agent.search_form(agent.endpoint_name(), DEFAULT_MODEL);
    assert_eq!(form, SearchForm::Native, "{form:?}");
    let (outcome, first) = turn(&mut agent, FIRST).await;
    let searches = finished_searches(&first);
    eprintln!(
        "live: openai search: {searches} finished; {}",
        cost_of(&outcome)
    );
    assert!(
        matches!(outcome, TurnOutcome::Completed { .. }),
        "{outcome:?}"
    );
    assert!(searches >= 1);
    let (outcome, _) = turn(&mut agent, SECOND).await;
    eprintln!("live: openai follow-up: {}", cost_of(&outcome));
    assert!(
        matches!(outcome, TurnOutcome::Completed { .. }),
        "{outcome:?}"
    );
}
