#![cfg(unix)]
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    reason = "a live check fails the test on any setup or network error"
)]

use crate::live;
use crate::pick;

use crate::scratch::ScratchDir;
use live::{agent_with, live_session, next_event};
use pick::pick;
use wisp_core::agent;
use wisp_core::models::ModelTable;
use wisp_protocol::{AgentEvent, ClientCommand, DeliveryMode, InputId, TurnOutcome, Usage};

const MODEL: &str = "claude-sonnet-5-5";
const CAP_USD: f64 = 0.10;
const MAX_TOKENS: u32 = 1024;
const ASK: &str = "Use bash to run `echo one`, then, as a separate call, `echo two`. \
Run one command per call. Then reply with the word done.";
const STEER: &str = "also print the date";

fn reply_text(events: &[AgentEvent]) -> String {
    events
        .iter()
        .filter_map(|e| match e {
            AgentEvent::TextDelta { text, .. } => Some(text.as_str()),
            _ => None,
        })
        .collect()
}

#[tokio::test(flavor = "multi_thread")]
#[ignore = "live: costs real money; run by hand with credentials in the environment"]
async fn live_a_steer_during_a_tool_loop_is_followed_in_the_same_turn() {
    let scratch = ScratchDir::new("wisp-live-", "steering");
    let agent = agent_with(
        live_session(MODEL, MAX_TOKENS),
        &["bash"],
        &scratch.dir("work"),
        &scratch.dir("tmp"),
    );
    let (commands, mut events, _task) = agent::spawn(agent);
    commands
        .send(ClientCommand::send("live-1", ASK))
        .await
        .unwrap();

    let mut seen = Vec::new();
    let mut steered = false;
    let mut spent = 0.0;
    let mut steps: Vec<Usage> = Vec::new();
    let mut turn = None;
    let outcome = loop {
        let event = next_event(&mut events).await;
        if let AgentEvent::TurnStarted { turn: n, .. } = &event {
            turn = Some(*n);
        }
        if matches!(event, AgentEvent::ToolCallStarted { .. }) && !steered {
            steered = true;
            commands
                .send(ClientCommand::Send {
                    id: InputId("live-2".into()),
                    text: STEER.into(),
                    images: Vec::new(),
                    mode: DeliveryMode::Steer,
                    turn,
                    replaces: None,
                })
                .await
                .unwrap();
        }
        if let AgentEvent::StepEnded {
            cost_usd, usage, ..
        } = &event
        {
            spent += cost_usd.unwrap_or(0.0);
            steps.extend(*usage);
            if spent > CAP_USD {
                commands.send(ClientCommand::Cancel).await.unwrap();
            }
        }
        let end = match &event {
            AgentEvent::TurnEnded { outcome, .. } => Some(outcome.clone()),
            _ => None,
        };
        seen.push(event);
        if let Some(outcome) = end {
            break outcome;
        }
    };
    drop(commands);
    while events.recv().await.is_some() {}

    let commands_run: Vec<String> = pick!(
        &seen,
        AgentEvent::ToolCallRequested { input, .. } =>
            input["command"].as_str().unwrap_or_default().to_owned()
    );
    let reply = reply_text(&seen);
    eprintln!("live: spent ${spent:.4}; commands {commands_run:?}\n{reply}");
    assert!(spent <= CAP_USD, "spent ${spent:.4}");
    assert!(
        matches!(outcome, TurnOutcome::Completed { .. }),
        "{outcome:?}"
    );
    assert!(steered, "no tool call started");
    let delivered_at = seen
        .iter()
        .find_map(|e| match e {
            AgentEvent::InputDelivered { ids, step } if ids == &[InputId("live-2".into())] => {
                Some(*step)
            }
            _ => None,
        })
        .expect("the steer was delivered");
    assert!(delivered_at >= 2, "{delivered_at}");
    let after_steer = seen
        .iter()
        .skip_while(|e| !matches!(e, AgentEvent::InputDelivered { ids, .. } if ids == &[InputId("live-2".into())]))
        .any(|e| matches!(e, AgentEvent::TextDelta { text, .. } if !text.trim().is_empty()));
    assert!(after_steer, "no text shown after the steer: {reply}");
    let started_turns = seen
        .iter()
        .filter(|e| matches!(e, AgentEvent::TurnStarted { .. }))
        .count();
    assert_eq!(started_turns, 1);
    assert!(
        commands_run.iter().any(|c| c.contains("date")),
        "the steer was not followed: {commands_run:?}"
    );
    let lowered = reply.to_lowercase();
    for word in ["inject", "suspicious"] {
        assert!(!lowered.contains(word), "{reply}");
    }
    assert!(
        !seen
            .iter()
            .any(|e| matches!(e, AgentEvent::CacheMiss { .. })),
        "a cache miss"
    );
    let index = usize::try_from(delivered_at).unwrap() - 1;
    let min = ModelTable::shared()
        .find(MODEL)
        .and_then(|row| row.min_cacheable)
        .unwrap_or(1024);
    if let (Some(before), Some(after)) = (steps.get(index - 1), steps.get(index)) {
        let previous = before.prompt_tokens();
        if previous >= min {
            assert!(
                after.cache_read_input_tokens * 10 >= previous * 9,
                "the step after the steer read {} of {previous} cached tokens",
                after.cache_read_input_tokens
            );
        }
    }
}
