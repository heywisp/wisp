#![cfg(unix)]
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    reason = "a live check fails the test on any setup or network error"
)]

use crate::live;

use crate::scratch::ScratchDir;
use live::{affords, agent_with, live_session, next_event, worst_cached_step_usd};
use wisp_core::agent;
use wisp_protocol::{AgentEvent, ClientCommand, TurnOutcome};

const MODEL: &str = "claude-sonnet-5-5";
const CAP_USD: f64 = 0.05;
const MAX_TOKENS: u32 = 1024;
const PREFIX_TOKENS: u64 = 4_000;
const STEP_TOKENS: u64 = 2_000;
const MAX_STEPS: u64 = 3;
const SOURCE: &str = "fn total(xs: &[u32]) -> u32 {\n    let mut acc = 0;\n    for x in xs {\n        acc += x;\n    }\n    acc\n}\n";

fn worst_step_usd(prompt_tokens: u64) -> f64 {
    worst_cached_step_usd(MODEL, prompt_tokens, PREFIX_TOKENS, STEP_TOKENS, MAX_TOKENS)
}

#[test]
fn the_worst_first_step_fits_under_the_cap() {
    let worst = worst_step_usd(0);
    assert!(worst <= CAP_USD, "${worst:.4}");
}

#[test]
fn the_second_step_still_fits_after_a_realistic_first() {
    let first = worst_step_usd(0);
    let prompt = PREFIX_TOKENS + STEP_TOKENS;
    let second = worst_step_usd(prompt);
    assert!(first + second <= CAP_USD, "${first:.4} + ${second:.4}");
}

async fn ask(scratch: &ScratchDir, tools: &[&str], id: &str, text: String) -> Vec<String> {
    let agent = agent_with(
        live_session(MODEL, MAX_TOKENS),
        tools,
        &scratch.root,
        &scratch.dir("tmp"),
    );
    let (commands, mut events, _task) = agent::spawn(agent);
    commands.send(ClientCommand::send(id, text)).await.unwrap();
    let mut spent = 0.0;
    let mut steps = 0u64;
    let mut prompt_tokens = 0u64;
    let mut tools = vec![];
    let outcome = loop {
        match next_event(&mut events).await {
            AgentEvent::StepEnded {
                cost_usd, usage, ..
            } => {
                spent += cost_usd.unwrap_or(0.0);
                steps += 1;
                if let Some(usage) = usage {
                    prompt_tokens = usage.prompt_tokens();
                }
                if steps >= MAX_STEPS || !affords(spent, worst_step_usd(prompt_tokens), CAP_USD) {
                    commands.send(ClientCommand::Cancel).await.unwrap();
                }
            }
            AgentEvent::ToolCallRequested { tool, .. } => tools.push(tool),
            AgentEvent::TurnEnded { outcome, .. } => break outcome,
            _ => {}
        }
    };
    eprintln!("live: spent ${spent:.4}; tools {tools:?}");
    assert!(
        matches!(outcome, TurnOutcome::Completed { .. }),
        "{outcome:?}"
    );
    assert!(spent <= CAP_USD, "spent ${spent:.4}");
    tools
}

#[tokio::test(flavor = "multi_thread")]
#[ignore = "live: costs real money; run by hand with credentials in the environment"]
async fn live_show_diff_shows_a_proposed_rename_and_changes_nothing() {
    let scratch = ScratchDir::new("wisp-live-", "show-diff");
    std::fs::write(scratch.root.join("sum.rs"), SOURCE).unwrap();
    let tools = ask(
        &scratch,
        &["bash", "show_diff", "view_file"],
        "live-show-diff-1",
        format!(
            "Here is sum.rs:\n\n```rust\n{SOURCE}```\n\nPropose renaming `acc` to `sum`. Show me the change as a diff, but don't change any file."
        ),
    )
    .await;
    assert!(tools.iter().any(|t| t == "show_diff"), "{tools:?}");
    assert_eq!(
        std::fs::read_to_string(scratch.root.join("sum.rs")).unwrap(),
        SOURCE
    );
}

#[tokio::test(flavor = "multi_thread")]
#[ignore = "live: costs real money; run by hand with credentials in the environment"]
async fn live_show_code_shows_a_command_to_paste() {
    let scratch = ScratchDir::new("wisp-live-", "show-code");
    let tools = ask(
        &scratch,
        &["bash", "show_code", "show_diff"],
        "live-show-code-1",
        "Give me a shell command that counts the lines of every .rs file under a directory, for me to paste into my terminal. Don't run it.".to_owned(),
    )
    .await;
    assert!(tools.iter().any(|t| t == "show_code"), "{tools:?}");
    assert!(!tools.iter().any(|t| t == "bash"), "{tools:?}");
}
