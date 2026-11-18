#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    reason = "a live check fails the test on any setup or network error"
)]

use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

use tokio::sync::mpsc;
use wisp_core::agent::{self, Agent};
use wisp_core::pricing::{estimate_usd, model_info};
use wisp_core::provider::anthropic::{AnthropicProvider, Settings};
use wisp_core::secrets::live_redactions;
use wisp_core::session::Redactions;
use wisp_core::workspace::Workspace;
use wisp_protocol::{AgentEvent, CacheTtl, Effort, SessionConfig, Usage};

#[path = "bundled.rs"]
pub(crate) mod bundled;
#[path = "turn_lines.rs"]
pub(crate) mod turn_lines;

pub(crate) const TTL: CacheTtl = CacheTtl::FiveMinutes;

pub(crate) fn provider() -> AnthropicProvider {
    let settings = Settings::resolve(|name| std::env::var(name).ok(), None)
        .unwrap_or_else(|e| panic!("{}", scrubbed(&e)));
    AnthropicProvider::from_settings(&settings).unwrap_or_else(|e| panic!("{}", scrubbed(&e)))
}

pub(crate) fn live_session(model: &str, max_tokens: u32) -> SessionConfig {
    let mut session = agent::default_session();
    session.model = model.to_owned();
    session.effort = Effort::Low;
    session.max_tokens = max_tokens;
    session.cache_ttl = TTL;
    session
}

#[allow(dead_code, reason = "only the Unix-only live tests use it")]
pub(crate) fn agent_with(
    session: SessionConfig,
    flags: &[&str],
    root: &Path,
    temp: &Path,
) -> Agent {
    turn_lines::with_turn_lines(Agent::new(Arc::new(provider()), session)).with_tools(
        bundled::toolbox(flags),
        Workspace {
            root: root.to_owned(),
            home: None,
            temp: temp.to_owned(),
        },
    )
}

pub(crate) async fn next_event(events: &mut mpsc::Receiver<AgentEvent>) -> AgentEvent {
    tokio::time::timeout(Duration::from_secs(180), events.recv())
        .await
        .expect("no event for three minutes")
        .expect("the agent stopped")
}

pub(crate) fn worst_usd(model: &str, read: u64, hour: u64, five: u64, output: u32) -> f64 {
    let info = model_info(model).expect("the live model is priced");
    let usage = Usage {
        output_tokens: u64::from(output),
        cache_read_input_tokens: read,
        cache_creation_input_tokens: hour + five,
        cache_creation_5m_input_tokens: Some(five),
        cache_creation_1h_input_tokens: Some(hour),
        ..Usage::default()
    };
    estimate_usd(info, &usage, TTL)
}

#[allow(dead_code, reason = "only the Unix-only live tests use it")]
pub(crate) fn worst_cached_step_usd(
    model: &str,
    prompt_tokens: u64,
    prefix: u64,
    step: u64,
    output: u32,
) -> f64 {
    let cold_prefix = if prompt_tokens == 0 { prefix } else { 0 };
    worst_usd(model, prompt_tokens, cold_prefix, step, output)
}

pub(crate) fn affords(spent: f64, worst: f64, cap: f64) -> bool {
    let fits = spent + worst <= cap;
    if !fits {
        eprintln!(
            "live: cost guard: spent ${spent:.4} + worst next request ${worst:.4} would pass the ${cap} cap"
        );
    }
    fits
}

pub(crate) fn secrets() -> Redactions {
    live_redactions(&|name| std::env::var(name).ok())
}

pub(crate) fn scrubbed(value: &impl std::fmt::Debug) -> String {
    secrets().scrub(&format!("{value:?}"))
}

pub(crate) fn next_id() -> String {
    static NEXT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(1);
    let n = NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    format!("live-{n}")
}
