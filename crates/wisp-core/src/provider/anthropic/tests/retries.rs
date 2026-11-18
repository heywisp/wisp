use std::time::Duration;

use tokio::sync::mpsc;
use wisp_protocol::{AgentEvent, ClientCommand, GaveUp, TurnOutcome};

use super::*;
use crate::agent::{Agent, default_session, retry};
use crate::test_guard::retries;

const TURN_18: &str =
    include_str!("../../../../tests/fixtures/streams/turn-18-upstream-interrupted.sse");
const START: &str = "event: message_start\ndata: {\"type\":\"message_start\",\"message\":{\"id\":\"m1\",\"model\":\"claude-opus-5-5\",\"usage\":{\"input_tokens\":10,\"output_tokens\":1}}}\n\n";
const TEXT: &str = "event: content_block_start\ndata: {\"type\":\"content_block_start\",\"index\":0,\"content_block\":{\"type\":\"text\",\"text\":\"\"}}\n\nevent: content_block_delta\ndata: {\"type\":\"content_block_delta\",\"index\":0,\"delta\":{\"type\":\"text_delta\",\"text\":\"Partial answer\"}}\n\n";

async fn run(
    base: &str,
    text: &str,
    policy: retry::RetryPolicy,
) -> (TurnOutcome, Vec<AgentEvent>, Agent) {
    let provider = AnthropicProvider::new(&api_key("k"), base).unwrap();
    let mut agent = Agent::new(Arc::new(provider), default_session())
        .with_clock(crate::clock::TickingClock::new())
        .with_retry_policy(policy);
    let (_tx, mut rx) = mpsc::channel::<ClientCommand>(4);
    let (ev_tx, mut ev_rx) = mpsc::channel::<AgentEvent>(512);
    let outcome = agent.run_turn(text.into(), &mut rx, &ev_tx).await.unwrap();
    drop(ev_tx);
    let mut events = Vec::new();
    while let Some(e) = ev_rx.recv().await {
        events.push(e);
    }
    (outcome, events, agent)
}

#[tokio::test]
async fn turn_18s_interrupted_stream_is_retried_with_the_same_bytes() {
    let (base, seen) = mock_server(vec![http::sse(TURN_18), http::sse(THINKING_AND_TEXT)]).await;
    let (outcome, events, agent) = run(&base, "hi", retry::quick()).await;
    assert!(
        matches!(
            outcome,
            TurnOutcome::Completed {
                committed: true,
                ..
            }
        ),
        "{outcome:?}"
    );
    assert_eq!(retries(&events), 1);
    assert_eq!(agent.last_turn_stats().unwrap().retries, 1);
    let reqs = seen.lock().unwrap();
    assert_eq!(reqs.len(), 2);
    assert_eq!(http::body(&reqs[0]), http::body(&reqs[1]));
    assert_eq!(agent.history().len(), 2);
    let retry = events
        .iter()
        .find_map(|e| match e {
            AgentEvent::Retrying {
                kind,
                detail: Some(detail),
                reason,
                ..
            } => Some((*kind, detail.clone(), reason.clone())),
            _ => None,
        })
        .unwrap();
    assert_eq!(retry.0, ErrorKind::Server);
    assert_eq!(retry.1.phase, FailurePhase::Stream);
    assert_eq!(retry.1.error_type.as_deref(), Some("api_error"));
    assert!(
        retry
            .2
            .contains("Upstream provider stream was interrupted."),
        "{}",
        retry.2
    );
}

#[tokio::test]
async fn a_partial_reply_that_breaks_is_never_committed() {
    let error = http::event(
        "error",
        r#"{"type":"error","error":{"type":"api_error","message":"Internal"}}"#,
    );
    let broken = format!("{START}{TEXT}{error}");
    let (base, seen) = mock_server(vec![http::sse(&broken), http::sse(THINKING_AND_TEXT)]).await;
    let (outcome, events, agent) = run(&base, "hi", retry::quick()).await;
    assert!(
        matches!(outcome, TurnOutcome::Completed { .. }),
        "{outcome:?}"
    );
    let discarded = events.iter().find_map(|e| match e {
        AgentEvent::Retrying {
            discarded_blocks,
            discarded_chars,
            ..
        } => Some((*discarded_blocks, *discarded_chars)),
        _ => None,
    });
    assert_eq!(discarded, Some((1, 14)));
    let history = serde_json::to_string(&*agent.history()).unwrap();
    assert!(!history.contains("Partial answer"), "{history}");
    let reqs = seen.lock().unwrap();
    assert_eq!(http::body(&reqs[0]), http::body(&reqs[1]));
}

#[tokio::test]
async fn transient_failures_of_every_shape_are_retried() {
    let api_error = r#"{"type":"error","error":{"type":"api_error","message":"Internal"}}"#;
    let overloaded =
        r#"{"type":"error","error":{"type":"overloaded_error","message":"Overloaded"}}"#;
    let rate = r#"{"type":"error","error":{"type":"rate_limit_error","message":"slow down"}}"#;
    let openai_rate = r#"{"error":{"message":"Rate limit reached","type":"requests","code":"rate_limit_exceeded"}}"#;
    let reset = format!(
        "HTTP/1.1 200 OK\r\ncontent-type: text/event-stream\r\ncontent-length: 100000\r\nconnection: close\r\n\r\n{START}{TEXT}"
    );
    let cases = [
        ("a reset mid-body", reset),
        (
            "a cut before message_stop",
            http::sse(&format!("{START}{TEXT}")),
        ),
        (
            "a 502 with api_error",
            http::json("502 Bad Gateway", api_error),
        ),
        ("a 529", http::json("529 Overloaded", overloaded)),
        (
            "a 429 with retry-after-ms",
            json_reply("429 Too Many Requests", &[("retry-after-ms", "1")], rate),
        ),
        (
            "a 429 with an OpenAI-style body",
            http::json("429 Too Many Requests", openai_rate),
        ),
        (
            "a 200 chunk holding an error with a status",
            http::sse(
                "data: {\"error\":{\"message\":\"bad gateway\",\"type\":\"server_error\",\"status\":502}}\n\n",
            ),
        ),
        ("turn 18's api_error event", http::sse(TURN_18)),
    ];
    for (name, first) in cases {
        let (base, seen) = mock_server(vec![first, http::sse(THINKING_AND_TEXT)]).await;
        let (outcome, events, _) = run(&base, "hi", retry::quick()).await;
        assert!(
            matches!(
                outcome,
                TurnOutcome::Completed {
                    committed: true,
                    ..
                }
            ),
            "{name}: {outcome:?}"
        );
        assert_eq!(retries(&events), 1, "{name}");
        let reqs = seen.lock().unwrap();
        assert_eq!(reqs.len(), 2, "{name}");
        assert_eq!(http::body(&reqs[0]), http::body(&reqs[1]), "{name}");
    }
}

#[tokio::test]
async fn failures_that_would_fail_again_end_the_turn_at_once() {
    let invalid = r#"{"type":"error","error":{"type":"invalid_request_error","message":"bad"}}"#;
    let auth =
        r#"{"type":"error","error":{"type":"authentication_error","message":"invalid x-api-key"}}"#;
    let billing = r#"{"type":"error","error":{"type":"billing_error","message":"no credit"}}"#;
    let overflow = r#"{"error":{"message":"too long","type":"invalid_request_error","code":"context_length_exceeded"}}"#;
    let malformed = format!(
        "{START}{}",
        http::event("content_block_start", "{not json}")
    );
    let cases = [
        (
            "400",
            http::json("400 Bad Request", invalid),
            ErrorKind::InvalidRequest,
        ),
        ("401", http::json("401 Unauthorized", auth), ErrorKind::Auth),
        (
            "402",
            http::json("402 Payment Required", billing),
            ErrorKind::Billing,
        ),
        (
            "overflow",
            http::json("400 Bad Request", overflow),
            ErrorKind::ContextOverflow,
        ),
        (
            "malformed event",
            http::sse(&malformed),
            ErrorKind::InvalidResponse,
        ),
        (
            "a text stream",
            http::sse(GATEWAY_ERROR),
            ErrorKind::InvalidResponse,
        ),
    ];
    for (name, first, kind) in cases {
        let (base, seen) = mock_server(vec![first, http::sse(THINKING_AND_TEXT)]).await;
        let (outcome, events, _) = run(&base, "hi", retry::quick()).await;
        let TurnOutcome::Error {
            kind: got,
            attempts: 1,
            gave_up: Some(GaveUp::NotRetryable),
            ..
        } = outcome
        else {
            panic!("{name}: {outcome:?}")
        };
        assert_eq!(got, kind, "{name}");
        assert_eq!(retries(&events), 0, "{name}");
        assert_eq!(seen.lock().unwrap().len(), 1, "{name}");
    }
}

#[tokio::test]
async fn a_stalled_stream_times_out_once_the_first_event_has_arrived() {
    let (addr, server) = holding_server(format!(
        "HTTP/1.1 200 OK\r\ncontent-type: text/event-stream\r\nrequest-id: req_stall\r\n\r\n{START}"
    ))
    .await;
    let mut provider = AnthropicProvider::new(&api_key("k"), &format!("http://{addr}")).unwrap();
    provider.waits.idle = Duration::from_millis(50);
    let s = session(None, CacheTtl::OneHour);
    let items: Vec<_> = provider.stream(&hi_request(&s)).collect().await;
    server.abort();
    let Some(Err(e)) = items.last() else {
        panic!("{items:?}")
    };
    assert_eq!(e.kind, ErrorKind::Timeout);
    assert_eq!(e.detail.phase, FailurePhase::Stream);
    assert_eq!(e.detail.source, wisp_protocol::FailureSource::Local);
    assert_eq!(e.detail.retry, RetryClass::Transient);
    assert_eq!(e.detail.request_id.as_deref(), Some("req_stall"));
    assert!(e.possibly_billed);
    assert!(e.message.contains("stalled"), "{}", e.message);
    assert!(
        items
            .iter()
            .any(|i| matches!(i, Ok(ProviderEvent::MessageStart { .. }))),
        "the watchdog started after the first event"
    );
}

#[test]
fn the_idle_watchdog_is_five_minutes() {
    assert_eq!(IDLE_TIMEOUT, Duration::from_secs(300));
    assert_eq!(FIRST_BYTE, Duration::from_secs(15 * 60));
    assert_eq!(
        crate::provider::openai::FIRST_BYTE,
        Duration::from_secs(5 * 60)
    );
}

#[tokio::test]
async fn an_anthropic_503_overload_gets_counted_unbilled_retries() {
    let overloaded =
        r#"{"type":"error","error":{"type":"overloaded_error","message":"Overloaded"}}"#;
    let replies = vec![http::json("503 Service Unavailable", overloaded); 6];
    let (base, seen) = mock_server(replies).await;
    let policy = retry::RetryPolicy {
        busy_window: retry::RetryPolicy::default().for_fallback().busy_window,
        ..retry::quick()
    };
    let (outcome, events, _) = run(&base, "hi", policy).await;
    let retried: Vec<_> = pick!(
        &events,
        AgentEvent::Retrying {
            busy,
            max_attempts,
            detail,
            lower_bound,
            ..
        } => (
            busy.is_none(),
            *max_attempts,
            detail.as_ref().map(|d| d.possibly_billed),
            *lower_bound,
        )
    );
    assert_eq!(retried, vec![(true, 5, Some(false), false); 5]);
    assert!(
        !events.iter().any(|e| matches!(
            e,
            AgentEvent::StepEnded {
                lower_bound: true,
                ..
            }
        )),
        "{events:?}"
    );
    assert!(
        matches!(
            outcome,
            TurnOutcome::Error {
                kind: ErrorKind::Overloaded,
                gave_up: Some(GaveUp::StepLimit),
                attempts: 6,
                ..
            }
        ),
        "{outcome:?}"
    );
    assert_eq!(seen.lock().unwrap().len(), 6);
}
