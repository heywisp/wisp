#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    reason = "a live check fails the test on any setup or network error"
)]

use crate::live;

use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use futures::StreamExt;
use futures::stream::BoxStream;
use live::turn_lines::with_turn_lines;
use live::{affords, live_session, next_event, next_id, provider, scrubbed, worst_usd};
use tokio::sync::mpsc;
use wisp_core::agent::{self, Agent};
use wisp_core::provider::anthropic::AnthropicProvider;
use wisp_core::provider::{ModelCapabilities, Provider, ProviderError, ProviderEvent, Request};
use wisp_protocol::{AgentEvent, ClientCommand, ContentBlock, Effort, StopReason, TurnOutcome};
use wisp_render::chrome::Status;
use wisp_render::motion::{MotionCtx, MotionPolicy};
use wisp_render::text::spans_text;
use wisp_render::theme::glyphs::UNICODE;
use wisp_render::transcript::copy;

const MODEL: &str = "claude-sonnet-5-5";
const CAP_USD: f64 = 0.04;
const SHORT_MAX_TOKENS: u32 = 128;
const LONG_MAX_TOKENS: u32 = 2_400;
const PROMPT_TOKENS_MAX: u64 = 500;
const STILL: MotionCtx = MotionCtx {
    policy: MotionPolicy::Reduced,
    ascii: false,
    blend: false,
};

#[derive(Clone, Copy, Debug)]
enum Seen {
    Delta { at: Duration, output_tokens: u64 },
    Stop { at: Duration },
}

type Log = Arc<Mutex<Vec<Seen>>>;

struct Counting {
    inner: AnthropicProvider,
    log: Log,
    started: Instant,
}

impl Provider for Counting {
    fn stream(
        &self,
        request: &Request<'_>,
    ) -> BoxStream<'static, Result<ProviderEvent, ProviderError>> {
        let log = self.log.clone();
        let started = self.started;
        self.inner
            .stream(request)
            .inspect(move |item| {
                let seen = match item {
                    Ok(ProviderEvent::MessageDelta { usage, .. }) => Some(Seen::Delta {
                        at: started.elapsed(),
                        output_tokens: usage.output_tokens,
                    }),
                    Ok(ProviderEvent::MessageStop) => Some(Seen::Stop {
                        at: started.elapsed(),
                    }),
                    _ => None,
                };
                if let Some(seen) = seen {
                    log.lock().unwrap().push(seen);
                }
            })
            .boxed()
    }

    fn echoed<'a>(
        &self,
        content: &'a [ContentBlock],
        earlier: &wisp_core::provider::ServerCalls,
    ) -> Vec<&'a ContentBlock> {
        self.inner.echoed(content, earlier)
    }

    fn search_signal(&self, block: &ContentBlock) -> Option<wisp_core::provider::SearchSignal> {
        self.inner.search_signal(block)
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

fn worst_next_usd(read_tokens: u64, max_tokens: u32) -> f64 {
    worst_usd(
        MODEL,
        read_tokens,
        PROMPT_TOKENS_MAX,
        u64::from(max_tokens),
        max_tokens,
    )
}

#[test]
fn both_live_turns_fit_under_the_cap_at_their_worst() {
    let first = worst_next_usd(0, SHORT_MAX_TOKENS);
    let second = worst_next_usd(0, LONG_MAX_TOKENS);
    assert!(first > 0.0 && first + second <= CAP_USD, "{first} {second}");
    assert!(first + second > 0.025, "{first} {second}");
}

struct Live {
    max_tokens: u32,
    commands: mpsc::Sender<ClientCommand>,
    events: mpsc::Receiver<AgentEvent>,
    log: Log,
    started: Instant,
    status: Status,
    spent: f64,
    prompt_tokens: u64,
}

struct Turn {
    outcome: TurnOutcome,
    lines: Vec<(Duration, String)>,
    done: String,
    estimate: u64,
    seen: Vec<Seen>,
}

impl Turn {
    fn stop_reason(&self) -> StopReason {
        match &self.outcome {
            TurnOutcome::Completed { stop_reason, .. } => stop_reason.clone(),
            other => panic!("the turn didn't complete: {}", scrubbed(other)),
        }
    }

    fn check_ending(&self, name: &str) {
        let exact = self.output_tokens();
        for (at, line) in &self.lines {
            assert!(
                !line.contains(copy::DONE),
                "{name} at {} ms: {line}",
                at.as_millis()
            );
        }
        match self.stop_reason() {
            StopReason::EndTurn => {
                assert!(
                    self.done.starts_with(&format!("{} \u{b7} ", copy::DONE)),
                    "{name}: {}",
                    self.done
                );
                assert!(
                    self.done
                        .contains(&copy::tokens_down("\u{2193}", "", exact)),
                    "{name}: {}",
                    self.done
                );
            }
            StopReason::MaxTokens => {
                eprintln!(
                    "live {name}: cut off at the output limit after {exact} tokens; measured anyway"
                );
                assert!(!self.done.contains(copy::DONE), "{name}: {}", self.done);
                assert!(self.done.contains("output limit"), "{name}: {}", self.done);
            }
            other => panic!("{name} stopped for {other:?}: {}", self.done),
        }
    }

    fn output_tokens(&self) -> u64 {
        match &self.outcome {
            TurnOutcome::Completed { usage, .. } => usage.output_tokens,
            other => panic!("the turn didn't complete: {}", scrubbed(other)),
        }
    }

    fn deltas(&self) -> usize {
        self.seen
            .iter()
            .filter(|s| matches!(s, Seen::Delta { .. }))
            .count()
    }

    fn deltas_before_the_last_stop(&self) -> usize {
        let last_stop = self
            .seen
            .iter()
            .rposition(|s| matches!(s, Seen::Stop { .. }))
            .unwrap_or(self.seen.len());
        self.seen[..last_stop]
            .iter()
            .filter(|s| matches!(s, Seen::Delta { .. }))
            .count()
    }
}

impl Live {
    fn new(max_tokens: u32, spent: f64) -> Live {
        let inner = provider();
        let started = Instant::now();
        let log = Log::default();
        let provider = Counting {
            inner,
            log: log.clone(),
            started,
        };
        let session = live_session(MODEL, max_tokens);
        let (commands, events, _task) =
            agent::spawn(with_turn_lines(Agent::new(Arc::new(provider), session)));
        let mut status = Status::new();
        status.turn_effort(Some(Effort::Low.as_str()));
        Live {
            max_tokens,
            commands,
            events,
            log,
            started,
            status,
            spent,
            prompt_tokens: 0,
        }
    }

    fn now(&self) -> Duration {
        self.started.elapsed()
    }

    fn line(&self, at: Duration) -> String {
        spans_text(&self.status.sentence(at, STILL, &UNICODE))
    }

    async fn turn(&mut self, text: &str) -> Turn {
        assert!(
            affords(
                self.spent,
                worst_next_usd(self.prompt_tokens, self.max_tokens),
                CAP_USD
            ),
            "the cost guard stopped the turn"
        );
        self.log.lock().unwrap().clear();
        let entered = self.now();
        self.status.submitted(entered, STILL);
        self.commands
            .send(ClientCommand::send(next_id(), text))
            .await
            .unwrap();
        let mut lines: Vec<(Duration, String)> = vec![];
        let mut next_sample = Duration::from_millis(300);
        loop {
            let event = next_event(&mut self.events).await;
            let at = self.now();
            let estimate = self.status.estimated_tokens();
            self.status.apply(&event, at, STILL);
            let since = at.saturating_sub(entered);
            if self.status.is_running() && since >= next_sample {
                lines.push((since, self.line(at)));
                next_sample = since + Duration::from_secs(1);
            }
            if let AgentEvent::StepEnded {
                cost_usd, usage, ..
            } = &event
            {
                self.spent += cost_usd.unwrap_or(0.0);
                if let Some(usage) = usage {
                    self.prompt_tokens = usage.prompt_tokens();
                }
                if self.spent > CAP_USD {
                    self.commands.send(ClientCommand::Cancel).await.unwrap();
                }
            }
            if let AgentEvent::TurnEnded { outcome, .. } = event {
                return Turn {
                    outcome,
                    lines,
                    done: self.line(at),
                    estimate,
                    seen: self.log.lock().unwrap().clone(),
                };
            }
        }
    }
}

fn report(name: &str, turn: &Turn) {
    for (at, line) in &turn.lines {
        eprintln!("live {name}: at {:>6} ms: {line}", at.as_millis());
    }
    eprintln!("live {name}: done line: {}", turn.done);
    eprintln!(
        "live {name}: message_delta events {} ({} before the last message_stop); exact output {} tokens",
        turn.deltas(),
        turn.deltas_before_the_last_stop(),
        turn.output_tokens()
    );
    for seen in &turn.seen {
        match seen {
            Seen::Delta { at, output_tokens } => eprintln!(
                "live {name}:   message_delta at {} ms, output_tokens {output_tokens}",
                at.as_millis()
            ),
            Seen::Stop { at } => eprintln!("live {name}:   message_stop at {} ms", at.as_millis()),
        }
    }
}

#[tokio::test(flavor = "multi_thread")]
#[ignore = "live: costs real money; run by hand with credentials in the environment"]
async fn live_working_line_shows_the_verb_then_done_and_counts_message_deltas() {
    let mut live = Live::new(SHORT_MAX_TOKENS, 0.0);

    let short = live.turn("Reply with the single word ok.").await;
    report("turn 1", &short);
    short.check_ending("turn 1");
    for (_, line) in &short.lines {
        assert!(
            line.is_empty() || line.starts_with(live.status.verb()) || line.contains("Retrying"),
            "{line}"
        );
    }

    let mut live = Live::new(LONG_MAX_TOKENS, live.spent);
    let long = live
        .turn(
            "Write a plain-text story of about 600 words about a lighthouse keeper. \
No headings, no lists, no Markdown.",
        )
        .await;
    report("turn 2", &long);
    let exact = long.output_tokens();
    eprintln!(
        "live turn 2: last estimate {} vs exact {exact}; total spent ${:.4} of ${CAP_USD}",
        long.estimate, live.spent
    );
    let verdict = if long.deltas_before_the_last_stop() > 1 {
        "more than one message_delta before message_stop: add AgentEvent::OutputTokens (plan section 6)"
    } else {
        "one message_delta before message_stop: keep the estimate"
    };
    eprintln!("live verdict: {verdict}");
    assert!(long.deltas() >= 1);
    assert!(exact >= 500, "{exact}");
    assert!(
        long.lines
            .iter()
            .any(|(at, line)| *at >= Duration::from_secs(1)
                && line.contains(&copy::esc_to_stop().text())),
        "{:#?}",
        long.lines
    );
    long.check_ending("turn 2");
    assert!(live.spent <= CAP_USD, "spent ${:.4}", live.spent);
}
