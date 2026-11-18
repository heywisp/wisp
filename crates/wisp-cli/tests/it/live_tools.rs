#![cfg(unix)]
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    reason = "a live check fails the test on any setup or network error"
)]

use crate::live;

use std::path::Path;
use std::time::Instant;

use crate::scratch::ScratchDir;
use live::{TTL, affords, agent_with, live_session, next_event, next_id, worst_cached_step_usd};
use tokio::sync::mpsc;
use wisp_core::agent;
use wisp_protocol::{AgentEvent, ClientCommand, TurnOutcome};
use wisp_render::markdown::{Layout, Renderer};
use wisp_render::text::spans_text;
use wisp_render::theme::glyphs::GlyphSet;
use wisp_render::transcript::{Config, Detail, Transcript, UserPart, copy};
use wisp_tool_text::output::INLINE_LIMIT;

const MODEL: &str = "claude-sonnet-5-5";
const CAP_USD: f64 = 0.05;
const MAX_TOKENS: u32 = 1024;
const PREFIX_TOKENS: u64 = 1_500;
const BYTES_PER_TOKEN: u64 = 3;
const WRAPPER_TOKENS: u64 = 200;

fn step_tokens(calls: u64) -> u64 {
    calls * (INLINE_LIMIT as u64 / BYTES_PER_TOKEN) + u64::from(MAX_TOKENS) + WRAPPER_TOKENS
}

fn worst_next_usd(prompt_tokens: u64, calls: u64) -> f64 {
    worst_cached_step_usd(
        MODEL,
        prompt_tokens,
        PREFIX_TOKENS,
        step_tokens(calls),
        MAX_TOKENS,
    )
}

#[test]
fn the_worst_first_request_fits_under_the_cap() {
    let first = worst_next_usd(0, 0);
    assert!(first > 0.0 && first < CAP_USD, "{first}");
    assert!(worst_next_usd(30_000, 1) > worst_next_usd(1, 1));
    let threshold = CAP_USD - worst_next_usd(8_000, 1);
    assert!(threshold > 0.02, "{threshold}");
}

#[test]
fn each_parallel_call_adds_a_whole_inline_result_to_the_worst_case() {
    let one = worst_next_usd(8_000, 1);
    let three = worst_next_usd(8_000, 3);
    assert!(three > CAP_USD, "{three}");
    assert!(three - one > 0.025, "{one} {three}");
    assert!(affords(0.01, worst_next_usd(8_000, 1), CAP_USD));
    assert!(!affords(0.01, worst_next_usd(8_000, 3), CAP_USD));
}

fn workspace() -> ScratchDir {
    let scratch = ScratchDir::new("wisp-live-", "tools");
    let work = scratch.dir("work");
    for name in ["alpha", "beta"] {
        let src = work.join(name).join("src");
        std::fs::create_dir_all(&src).unwrap();
        std::fs::write(
            work.join(name).join("Cargo.toml"),
            format!("[package]\nname = \"{name}\"\nversion = \"0.1.0\"\nedition = \"2024\"\n"),
        )
        .unwrap();
        std::fs::write(src.join("lib.rs"), "pub fn one() -> u32 {\n    1\n}\n").unwrap();
    }
    std::fs::write(
        work.join("Cargo.toml"),
        "[workspace]\nmembers = [\"alpha\", \"beta\"]\nresolver = \"3\"\n",
    )
    .unwrap();
    scratch
}

struct Live {
    commands: mpsc::Sender<ClientCommand>,
    events: mpsc::Receiver<AgentEvent>,
    transcript: Transcript,
    started: Instant,
    spent: f64,
    prompt_tokens: u64,
    calls: u64,
    guarded: bool,
}

impl Live {
    fn new(root: &Path, temp: &Path) -> Live {
        let agent = agent_with(live_session(MODEL, MAX_TOKENS), &["bash"], root, temp);
        let (commands, events, _task) = agent::spawn(agent);
        Live {
            commands,
            events,
            transcript: Transcript::new(Config {
                provider: "Anthropic".to_owned(),
                cache_ttl: TTL,
                logging: false,
            }),
            started: Instant::now(),
            spent: 0.0,
            prompt_tokens: 0,
            calls: 0,
            guarded: false,
        }
    }

    async fn turn(
        &mut self,
        text: &str,
        mut on: impl FnMut(&AgentEvent) -> bool,
    ) -> (TurnOutcome, Vec<String>) {
        if !self.affords_next() {
            return (
                TurnOutcome::Cancelled {
                    usage: None,
                    cost: None,
                    committed_steps: 0,
                },
                vec![],
            );
        }
        self.commands
            .send(ClientCommand::send(next_id(), text))
            .await
            .unwrap();
        let mut run = vec![];
        loop {
            let event = next_event(&mut self.events).await;
            self.transcript.apply(&event, self.started.elapsed());
            if matches!(event, AgentEvent::InputDelivered { .. }) {
                self.transcript
                    .deliver(vec![UserPart::Text(text.to_owned())], None);
            }
            if let AgentEvent::StepEnded {
                cost_usd, usage, ..
            } = &event
            {
                self.spent += cost_usd.unwrap_or(0.0);
                if let Some(usage) = usage {
                    self.prompt_tokens = usage.prompt_tokens();
                }
                if !self.guarded && !self.affords_next() {
                    self.commands.send(ClientCommand::Cancel).await.unwrap();
                }
                self.calls = 0;
            }
            if let AgentEvent::ToolCallRequested { input, .. } = &event {
                self.calls += 1;
                run.push(input["command"].as_str().unwrap_or_default().to_owned());
            }
            if on(&event) {
                self.commands.send(ClientCommand::Cancel).await.unwrap();
            }
            if let AgentEvent::TurnEnded { outcome, .. } = event {
                return (outcome, run);
            }
        }
    }

    fn affords_next(&mut self) -> bool {
        let fits = affords(
            self.spent,
            worst_next_usd(self.prompt_tokens, self.calls),
            CAP_USD,
        );
        self.guarded |= !fits;
        fits
    }

    fn rows(&mut self) -> String {
        let r = Renderer::new(Layout::new(100, GlyphSet::Unicode));
        self.transcript
            .rows(&r, Detail::Scrollback)
            .iter()
            .map(|l| spans_text(&l.spans))
            .collect::<Vec<_>>()
            .join("\n")
    }
}

#[tokio::test(flavor = "multi_thread")]
#[ignore = "live: costs real money; run by hand with credentials in the environment"]
async fn live_tui_lists_the_crates_and_keeps_steps_when_a_sleep_is_stopped() {
    let scratch = workspace();
    let mut live = Live::new(&scratch.dir("work"), &scratch.dir("tmp"));

    let (outcome, run) = live
        .turn(
            "List the Rust crates in this directory. Reply with only their names.",
            |_| false,
        )
        .await;
    let rows = live.rows();
    eprintln!(
        "live: turn 1 spent ${:.4}; commands {run:?}\n{rows}",
        live.spent
    );
    assert!(
        !live.guarded,
        "the cost guard stopped turn 1 at ${:.4}",
        live.spent
    );
    assert!(
        matches!(outcome, TurnOutcome::Completed { .. }),
        "{outcome:?}"
    );
    assert!(!run.is_empty(), "no command ran\n{rows}");
    assert!(rows.contains("Ran"), "{rows}");
    assert!(rows.contains("alpha") && rows.contains("beta"), "{rows}");
    assert!(!rows.contains("Preparing"), "{rows}");

    let mut stopped = false;
    let (outcome, run) = live
        .turn(
            "Run exactly `sleep 30` with bash, then tell me when it is done.",
            |event| {
                let fire = matches!(event, AgentEvent::ToolCallStarted { .. }) && !stopped;
                stopped |= fire;
                fire
            },
        )
        .await;
    let rows = live.rows();
    eprintln!(
        "live: total spent ${:.4}; commands {run:?}\n{rows}",
        live.spent
    );
    assert!(
        !live.guarded,
        "the cost guard stopped turn 2 at ${:.4}",
        live.spent
    );
    assert!(live.spent <= CAP_USD, "spent ${:.4}", live.spent);
    let TurnOutcome::Cancelled {
        committed_steps, ..
    } = outcome
    else {
        panic!("turn 2 was not cancelled: {outcome:?}");
    };
    assert!(committed_steps >= 1, "{committed_steps}");
    assert!(rows.contains(&copy::stopped(committed_steps)), "{rows}");
    let row = rows
        .lines()
        .find(|l| l.contains("Ran       sleep 30"))
        .unwrap_or_else(|| panic!("{rows}"));
    assert!(row.contains("stopped"), "{rows}");
    assert!(!rows.contains("Preparing"), "{rows}");
}
