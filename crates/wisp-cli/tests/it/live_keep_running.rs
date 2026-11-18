#![cfg(unix)]
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    reason = "a live check fails the test on any setup or network error"
)]

use crate::live;

use std::net::{Ipv4Addr, TcpStream};
use std::path::PathBuf;
use std::time::{Duration, Instant};

use crate::scratch::ScratchDir;
use crate::support::server::free_port;
use live::{affords, agent_with, live_session, next_event, worst_cached_step_usd};
use rustix::process::{Pid, Signal, kill_process_group};
use wisp_core::agent;
use wisp_protocol::{AgentEvent, ClientCommand, TurnOutcome, Usage};

const MODEL: &str = "claude-sonnet-5-5";
const CAP_USD: f64 = 0.10;
const MAX_TOKENS: u32 = 1024;
const PREFIX_TOKENS: u64 = 4_000;
const STEP_TOKENS: u64 = 1_500;

fn worst_next_usd(prompt_tokens: u64) -> f64 {
    worst_cached_step_usd(MODEL, prompt_tokens, PREFIX_TOKENS, STEP_TOKENS, MAX_TOKENS)
}

#[test]
fn the_worst_first_request_fits_under_the_keep_running_cap() {
    let first = worst_next_usd(0);
    assert!(first > 0.0 && first < CAP_USD / 2.0, "{first}");
}

#[derive(Default)]
struct KillGroup(Vec<u32>);

impl Drop for KillGroup {
    fn drop(&mut self) {
        for pid in &self.0 {
            if let Some(pid) = i32::try_from(*pid).ok().and_then(Pid::from_raw) {
                let _ = kill_process_group(pid, Signal::KILL);
            }
        }
    }
}

fn answers(port: u16, within: Duration) -> bool {
    let until = Instant::now() + within;
    while Instant::now() < until {
        if TcpStream::connect((Ipv4Addr::LOCALHOST, port)).is_ok() {
            return true;
        }
        std::thread::sleep(Duration::from_millis(100));
    }
    false
}

#[tokio::test(flavor = "multi_thread")]
#[ignore = "live: costs real money; run by hand with credentials in the environment"]
async fn live_a_kept_server_still_answers_after_the_agent_closes() {
    let scratch = ScratchDir::new("wisp-live-", "keep-running");
    let mut guard = KillGroup::default();
    let port = free_port();

    let agent = agent_with(
        live_session(MODEL, MAX_TOKENS),
        &["bash"],
        &scratch.dir("work"),
        &scratch.dir("tmp"),
    );
    let (commands, mut events, task) = agent::spawn(agent);

    let prompt = format!(
        "Start `python3 -m http.server {port} --bind 127.0.0.1` with one bash call in the \
         background, set so it keeps running after this session ends. Then reply only: started."
    );
    commands
        .send(ClientCommand::send("live-1", prompt))
        .await
        .unwrap();
    let mut spent = 0.0;
    let mut guarded = false;
    let mut inputs = vec![];
    let mut log = None;
    let outcome = loop {
        let event = next_event(&mut events).await;
        match &event {
            AgentEvent::ToolCallRequested { input, .. } => inputs.push(input.clone()),
            AgentEvent::ProcessStarted {
                keeps_running: true,
                pid,
                log: kept_log,
                ..
            } => {
                guard.0.extend(*pid);
                log.clone_from(kept_log);
            }
            AgentEvent::StepEnded {
                cost_usd, usage, ..
            } => {
                spent += cost_usd.unwrap_or(0.0);
                let prompt_tokens = usage.as_ref().map_or(0, Usage::prompt_tokens);
                if !guarded && !affords(spent, worst_next_usd(prompt_tokens), CAP_USD) {
                    guarded = true;
                    commands.send(ClientCommand::Cancel).await.unwrap();
                }
            }
            _ => {}
        }
        if let AgentEvent::TurnEnded { outcome, .. } = event {
            break outcome;
        }
    };
    eprintln!("live: spent ${spent:.4}; inputs {inputs:?}");
    drop(commands);
    while events.recv().await.is_some() {}
    assert!(task.join().await, "the agent did not shut down cleanly");

    assert!(spent <= CAP_USD, "spent ${spent:.4}");
    assert!(
        matches!(outcome, TurnOutcome::Completed { .. }),
        "{outcome:?}"
    );
    assert!(
        inputs
            .iter()
            .any(|i| i["keep_running"] == true && i["run_in_background"] == true),
        "{inputs:?}"
    );
    assert_eq!(guard.0.len(), 1, "{:?}", guard.0);
    let log = PathBuf::from(log.expect("a kept process names its log"));
    assert!(
        log.starts_with(scratch.root.join("tmp")),
        "{}",
        log.display()
    );
    assert!(
        answers(port, Duration::from_secs(10)),
        "the kept server on port {port} does not answer after the agent closed"
    );
}
