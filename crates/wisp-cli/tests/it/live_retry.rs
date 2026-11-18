#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    reason = "a live check fails the test on any setup or network error"
)]

use crate::live;

use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use futures::StreamExt;
use live::turn_lines::{self, with_turn_lines};
use live::{live_session, next_event, scrubbed, worst_usd};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::mpsc;
use wisp_core::agent::{self, Agent};
use wisp_core::provider::anthropic::{AnthropicProvider, DEFAULT_BASE_URL, Settings};
use wisp_protocol::{AgentEvent, ClientCommand, TurnOutcome, Usage};

const MODEL: &str = "claude-sonnet-5-5";
const CAP_USD: f64 = 0.10;
const MAX_TOKENS: u32 = 256;
const FILLER_WORDS: usize = 1_000;
const CUT_REQUEST: usize = 1;
const SECOND: &str = "Count from one to twenty in words.";
const FRAMING_TOKENS: u64 = 1_000;

fn first_prompt() -> String {
    let filler = (0..FILLER_WORDS)
        .map(|i| format!("w{i}"))
        .collect::<Vec<_>>()
        .join(" ");
    format!("Here is a list of words to ignore: {filler}\nReply with just: ok")
}

fn chars(text: &str) -> u64 {
    text.chars().count() as u64
}

fn worst_case_usd() -> f64 {
    let system = chars(agent::BASE_SYSTEM_PROMPT)
        + turn_lines::started()
            .hooks
            .table
            .sentences()
            .iter()
            .map(|s| chars(s) + 1)
            .sum::<u64>();
    let first = system + chars(&first_prompt()) + FRAMING_TOKENS;
    let second = first + u64::from(MAX_TOKENS) + chars(SECOND);
    let request = |prompt: u64| worst_usd(MODEL, 0, prompt, 0, MAX_TOKENS);
    request(first) + request(second) * 2.0
}

#[test]
fn the_worst_case_fits_under_the_cap() {
    let worst = worst_case_usd();
    assert!(worst > 0.0 && worst < CAP_USD, "{worst}");
}

fn header_end(buf: &[u8]) -> Option<usize> {
    buf.windows(4).position(|w| w == b"\r\n\r\n").map(|i| i + 4)
}

async fn read_request(sock: &mut TcpStream) -> Option<(String, Vec<(String, String)>, Vec<u8>)> {
    let mut buf = Vec::new();
    let mut chunk = [0u8; 16 * 1024];
    let end = loop {
        let n = sock.read(&mut chunk).await.ok()?;
        if n == 0 {
            return None;
        }
        buf.extend_from_slice(&chunk[..n]);
        if let Some(end) = header_end(&buf) {
            break end;
        }
    };
    let head = String::from_utf8_lossy(&buf[..end]).into_owned();
    let mut lines = head.split("\r\n");
    let path = lines.next()?.split(' ').nth(1)?.to_owned();
    let headers: Vec<(String, String)> = lines
        .filter_map(|l| l.split_once(':'))
        .map(|(k, v)| (k.trim().to_ascii_lowercase(), v.trim().to_owned()))
        .collect();
    let len: usize = headers
        .iter()
        .find(|(k, _)| k == "content-length")
        .and_then(|(_, v)| v.parse().ok())
        .unwrap_or(0);
    let mut body = buf[end..].to_vec();
    while body.len() < len {
        let n = sock.read(&mut chunk).await.ok()?;
        if n == 0 {
            break;
        }
        body.extend_from_slice(&chunk[..n]);
    }
    Some((path, headers, body))
}

async fn forward(mut sock: TcpStream, upstream: Arc<String>, seen: Arc<AtomicUsize>) {
    let Some((path, headers, body)) = read_request(&mut sock).await else {
        return;
    };
    let index = seen.fetch_add(1, Ordering::SeqCst);
    let client = reqwest::Client::new();
    let mut request = client.post(format!("{upstream}{path}")).body(body);
    for (name, value) in &headers {
        if !matches!(
            name.as_str(),
            "host" | "content-length" | "connection" | "accept-encoding"
        ) {
            request = request.header(name.as_str(), value.as_str());
        }
    }
    let response = request
        .send()
        .await
        .unwrap_or_else(|e| panic!("the upstream answered: {}", scrubbed(&e)));
    let mut head = format!("HTTP/1.1 {}\r\n", response.status());
    for name in ["content-type", "request-id", "retry-after"] {
        if let Some(v) = response.headers().get(name).and_then(|v| v.to_str().ok()) {
            head.push_str(&format!("{name}: {v}\r\n"));
        }
    }
    head.push_str("connection: close\r\n\r\n");
    sock.write_all(head.as_bytes()).await.unwrap();
    let cut = index == CUT_REQUEST;
    let mut stream = response.bytes_stream();
    let mut sent = String::new();
    while let Some(Ok(bytes)) = stream.next().await {
        sock.write_all(&bytes).await.unwrap();
        sent.push_str(&String::from_utf8_lossy(&bytes));
        if cut && sent.contains("content_block_delta") {
            eprintln!("live: proxy cut request {index} mid-stream");
            break;
        }
    }
    let _ = sock.shutdown().await;
}

async fn proxy(upstream: String) -> (String, Arc<AtomicUsize>) {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let upstream = Arc::new(upstream.trim_end_matches('/').to_owned());
    let seen = Arc::new(AtomicUsize::new(0));
    let counter = seen.clone();
    tokio::spawn(async move {
        while let Ok((sock, _)) = listener.accept().await {
            tokio::spawn(forward(sock, upstream.clone(), counter.clone()));
        }
    });
    (format!("http://{addr}"), seen)
}

async fn turn(
    commands: &mpsc::Sender<ClientCommand>,
    events: &mut mpsc::Receiver<AgentEvent>,
    id: &str,
    text: String,
) -> (TurnOutcome, Vec<AgentEvent>) {
    commands.send(ClientCommand::send(id, text)).await.unwrap();
    let mut seen = Vec::new();
    loop {
        let event = next_event(events).await;
        if let AgentEvent::TurnEnded { outcome, .. } = &event {
            return (outcome.clone(), seen);
        }
        seen.push(event);
    }
}

fn step_usage(events: &[AgentEvent]) -> Option<Usage> {
    events.iter().rev().find_map(|e| match e {
        AgentEvent::StepEnded { usage, .. } => *usage,
        _ => None,
    })
}

#[tokio::test(flavor = "multi_thread")]
#[ignore = "live: costs real money (capped at $0.10); run by hand with credentials in the environment"]
async fn live_a_retried_step_reads_the_cache() {
    assert!(worst_case_usd() < CAP_USD);
    let env = |name: &str| std::env::var(name).ok();
    let upstream = env("ANTHROPIC_BASE_URL").unwrap_or_else(|| DEFAULT_BASE_URL.to_owned());
    let (base, seen) = proxy(upstream).await;
    let through = |name: &str| {
        if name == "ANTHROPIC_BASE_URL" {
            Some(base.clone())
        } else {
            std::env::var(name).ok()
        }
    };
    let settings = Settings::resolve(through, None).unwrap();
    let provider = AnthropicProvider::from_settings(&settings).unwrap();
    let (commands, mut events, _task) = agent::spawn(with_turn_lines(Agent::new(
        Arc::new(provider),
        live_session(MODEL, MAX_TOKENS),
    )));

    let (outcome, first_events) = turn(&commands, &mut events, "live-1", first_prompt()).await;
    assert!(
        matches!(outcome, TurnOutcome::Completed { .. }),
        "{outcome:?}"
    );
    let previous = step_usage(&first_events).expect("the first step had usage");

    let (outcome, second_events) = turn(&commands, &mut events, "live-2", SECOND.into()).await;
    drop(commands);
    assert!(
        matches!(outcome, TurnOutcome::Completed { .. }),
        "{outcome:?}"
    );
    let failed = second_events.iter().find_map(|e| match e {
        AgentEvent::Retrying { usage, .. } => Some(*usage),
        _ => None,
    });
    let failed = failed.expect("the cut attempt was retried");
    let retried = step_usage(&second_events).expect("the retry had usage");
    eprintln!(
        "live: previous step prompt {} tokens; failed attempt {failed:?}; retry read {} and wrote {}; requests {}",
        previous.prompt_tokens(),
        retried.cache_read_input_tokens,
        retried.cache_creation_input_tokens,
        seen.load(Ordering::SeqCst),
    );
    if let TurnOutcome::Completed {
        cost: Some(cost), ..
    } = &outcome
    {
        eprintln!("live: session cost ${:.4}", cost.session_usd);
        assert!(cost.session_usd < CAP_USD, "{cost:?}");
    }
    assert_eq!(seen.load(Ordering::SeqCst), 3);
    assert!(
        retried.cache_read_input_tokens >= previous.prompt_tokens(),
        "the retry read the previous step's prefix from the cache"
    );
}
