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
use wisp_protocol::{AgentEvent, Answer, ClientCommand, Question, TurnOutcome};

const MODEL: &str = "claude-sonnet-5-5";
const CAP_USD: f64 = 0.05;
const MAX_TOKENS: u32 = 1024;
const PREFIX_TOKENS: u64 = 5_000;
const STEP_TOKENS: u64 = 1_500;
const MAX_STEPS: u64 = 4;
const PROMPT: &str =
    "Ask me which of two colours, red or blue, to use, then reply with just that colour.";

fn worst_step_usd(prompt_tokens: u64) -> f64 {
    worst_cached_step_usd(MODEL, prompt_tokens, PREFIX_TOKENS, STEP_TOKENS, MAX_TOKENS)
}

#[test]
fn the_worst_first_ask_step_fits_under_the_cap() {
    let worst = worst_step_usd(0);
    assert!(worst <= CAP_USD, "${worst:.4}");
}

#[test]
fn the_second_ask_step_still_fits_after_a_realistic_first() {
    let first = worst_step_usd(0);
    let prompt = PREFIX_TOKENS + STEP_TOKENS;
    let second = worst_step_usd(prompt);
    assert!(first + second <= CAP_USD, "${first:.4} + ${second:.4}");
}

fn blue(questions: &[Question]) -> Option<Vec<Answer>> {
    let question = questions.first()?;
    let at = question
        .options
        .iter()
        .position(|o| o.label.to_lowercase().contains("blue"))?;
    let mut answers = vec![Answer::choose(u32::try_from(at).ok()?)];
    answers.extend(questions.iter().skip(1).map(|_| Answer::skipped()));
    Some(answers)
}

#[tokio::test(flavor = "multi_thread")]
#[ignore = "live: costs real money; run by hand with credentials in the environment"]
async fn live_ask_user_asks_red_or_blue_and_follows_the_answer() {
    let scratch = ScratchDir::new("wisp-live-", "ask");
    let agent = agent_with(
        live_session(MODEL, MAX_TOKENS),
        &["ask_user", "bash", "show_diff", "view_file"],
        &scratch.root,
        &scratch.dir("tmp"),
    )
    .with_user_input(true);
    let (commands, mut events, _task) = agent::spawn(agent);
    commands
        .send(ClientCommand::send("live-ask-1", PROMPT))
        .await
        .unwrap();
    let mut spent = 0.0;
    let mut steps = 0u64;
    let mut prompt_tokens = 0u64;
    let mut tools = vec![];
    let mut asked = 0u32;
    let mut reply = String::new();
    let mut answered_step = None;
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
            AgentEvent::QuestionAsked { call_id, questions } => {
                asked += 1;
                let Some(answers) = blue(&questions) else {
                    commands.send(ClientCommand::Cancel).await.unwrap();
                    panic!("no option names blue after ${spent:.4}: {questions:?}");
                };
                commands
                    .send(ClientCommand::Answer { call_id, answers })
                    .await
                    .unwrap();
            }
            AgentEvent::QuestionClosed {
                how: wisp_protocol::QuestionEnd::Answered { .. },
                ..
            } => answered_step = Some(steps),
            AgentEvent::TextDelta { text, .. } => reply.push_str(&text),
            AgentEvent::TurnEnded { outcome, .. }
                if !matches!(outcome, TurnOutcome::Completed { .. })
                    || answered_step.is_some_and(|at| steps > at) =>
            {
                break outcome;
            }
            _ => {}
        }
    };
    eprintln!("live: spent ${spent:.4} over {steps} steps; tools {tools:?}; reply {reply:?}");
    assert!(
        matches!(outcome, TurnOutcome::Completed { .. }),
        "{outcome:?}"
    );
    assert_eq!(asked, 1, "the model asked without ask_user: {tools:?}");
    assert_eq!(tools, ["ask_user"], "{tools:?}");
    assert!((2..=3).contains(&steps), "{steps}");
    assert!(reply.to_lowercase().contains("blue"), "{reply:?}");
    assert!(spent <= CAP_USD, "spent ${spent:.4}");
}
