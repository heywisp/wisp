use wisp_protocol::{ErrorDetail, FailureSource};

use super::*;

const TURN_18: &str =
    include_str!("../../../../tests/fixtures/streams/turn-18-upstream-interrupted.sse");
const TEXT_ONLY_CHUNKS: &str =
    include_str!("../../../../tests/fixtures/streams/text-only-chat-chunks.sse");

const START: &str = "event: message_start\ndata: {\"type\":\"message_start\",\"message\":{\"id\":\"m1\",\"model\":\"claude-opus-5-5\",\"usage\":{\"input_tokens\":10,\"cache_read_input_tokens\":190000,\"output_tokens\":1}}}\n\nevent: ping\ndata: {\"type\":\"ping\"}\n\n";
const TEXT: &str = "event: content_block_start\ndata: {\"type\":\"content_block_start\",\"index\":0,\"content_block\":{\"type\":\"text\",\"text\":\"\"}}\n\nevent: content_block_delta\ndata: {\"type\":\"content_block_delta\",\"index\":0,\"delta\":{\"type\":\"text_delta\",\"text\":\"Partial\"}}\n\n";

fn data(json: &str) -> String {
    format!("data: {json}\n\n")
}

fn failure(sse: &str) -> ProviderError {
    match run_fixture(sse) {
        (_, Err(e)) => e,
        (_, Ok(m)) => panic!("expected a failure, got {m:?}"),
    }
}

async fn last_error(response: String) -> ProviderError {
    let s = session(None, CacheTtl::OneHour);
    last_error_for(
        |base| AnthropicProvider::new(&api_key("k"), base).unwrap(),
        &hi_request(&s),
        response,
    )
    .await
}

pub(super) async fn last_error_for(
    provider: impl FnOnce(&str) -> AnthropicProvider,
    request: &Request<'_>,
    response: String,
) -> ProviderError {
    let (base, _) = mock_server(vec![response]).await;
    let items: Vec<_> = provider(&base).stream(request).collect().await;
    match items.last() {
        Some(Err(e)) => e.clone(),
        other => panic!("{other:?}"),
    }
}

fn assert_detail(e: &ProviderError, kind: ErrorKind, phase: FailurePhase, retry: RetryClass) {
    assert_eq!(e.kind, kind, "{}", e.message);
    assert_eq!(e.detail.phase, phase, "{}", e.message);
    assert_eq!(e.detail.retry, retry, "{}", e.message);
    assert_eq!(e.detail.possibly_billed, e.possibly_billed, "{}", e.message);
}

#[test]
fn replaying_turn_18_ends_in_a_mid_stream_server_error() {
    for sse in [
        TURN_18.to_owned(),
        format!("{START}{TURN_18}"),
        format!("{START}{TEXT}{TURN_18}"),
    ] {
        let e = failure(&sse);
        assert_detail(
            &e,
            ErrorKind::Server,
            FailurePhase::Stream,
            RetryClass::Transient,
        );
        assert_eq!(e.detail.error_type.as_deref(), Some("api_error"));
        assert_eq!(e.detail.source, FailureSource::Endpoint);
        assert!(e.possibly_billed);
        assert_eq!(e.message, "Upstream provider stream was interrupted.");
    }
}

#[tokio::test]
async fn turn_18_through_the_adapter_keeps_the_request_id() {
    let e = last_error(http::sse_with(&[("request-id", "req_018")], TURN_18)).await;
    assert_detail(
        &e,
        ErrorKind::Server,
        FailurePhase::Stream,
        RetryClass::Transient,
    );
    assert_eq!(e.detail.request_id.as_deref(), Some("req_018"));
    assert_eq!(e.detail.status, Some(200));
    assert!(e.possibly_billed);
}

#[test]
fn every_documented_stream_error_type_has_its_kind() {
    for (error_type, kind) in [
        ("api_error", ErrorKind::Server),
        ("overloaded_error", ErrorKind::Overloaded),
        ("rate_limit_error", ErrorKind::RateLimit),
        ("timeout_error", ErrorKind::Timeout),
        ("billing_error", ErrorKind::Billing),
        ("authentication_error", ErrorKind::Auth),
        ("invalid_request_error", ErrorKind::InvalidRequest),
    ] {
        let event = http::event(
            "error",
            format!(r#"{{"type":"error","error":{{"type":"{error_type}","message":"m"}}}}"#),
        );
        let e = failure(&format!("{START}{event}"));
        assert_eq!(e.kind, kind, "{error_type}");
        assert_eq!(e.detail.phase, FailurePhase::Stream, "{error_type}");
        assert_eq!(e.detail.error_type.as_deref(), Some(error_type));
        assert!(e.possibly_billed, "{error_type}");
    }
    let unknown = failure(&format!(
        "{START}{}",
        http::event(
            "error",
            r#"{"type":"error","error":{"type":"brand_new_error","message":"m"}}"#
        )
    ));
    assert_detail(
        &unknown,
        ErrorKind::Other,
        FailurePhase::Stream,
        RetryClass::Never,
    );
    assert_eq!(
        unknown.detail.error_type.as_deref(),
        Some("brand_new_error")
    );
    assert_eq!(unknown.detail.status, Some(200));
}

#[test]
fn error_objects_in_a_200_stream_are_read_in_every_shape() {
    let anthropic_chunk = data(r#"{"type":"error","error":{"type":"api_error","message":"boom"}}"#);
    let openai_with_status = data(
        r#"{"error":{"message":"bad gateway","type":"server_error","code":null,"status":502}}"#,
    );
    let openai_without_status = data(
        r#"{"error":{"message":"slow down {provider}, org-PLACEHOLDER0000000000000000, Bearer 'CANARY.+/=_token'","type":"requests","code":"rate_limit_exceeded"}}"#,
    );
    let bare = data(r#"{"error":"the upstream went away"}"#);
    let typed = data(r#"{"type":"error","code":"rate_limit_exceeded","message":"slow down"}"#);
    let typed_event = http::event(
        "error",
        r#"{"type":"error","code":"insufficient_quota","message":"top up"}"#,
    );
    let cases = [
        (
            &anthropic_chunk,
            ErrorKind::Server,
            FailurePhase::Response,
            RetryClass::Transient,
        ),
        (
            &openai_with_status,
            ErrorKind::Server,
            FailurePhase::Response,
            RetryClass::Transient,
        ),
        (
            &openai_without_status,
            ErrorKind::RateLimit,
            FailurePhase::Response,
            RetryClass::Transient,
        ),
        (
            &bare,
            ErrorKind::InvalidResponse,
            FailurePhase::Response,
            RetryClass::Never,
        ),
        (
            &typed,
            ErrorKind::RateLimit,
            FailurePhase::Response,
            RetryClass::Transient,
        ),
        (
            &typed_event,
            ErrorKind::Billing,
            FailurePhase::Stream,
            RetryClass::Never,
        ),
    ];
    for (sse, kind, phase, retry) in cases {
        let e = failure(sse);
        assert_detail(&e, kind, phase, retry);
        assert_eq!(e.detail.source, FailureSource::Endpoint, "{sse}");
        let after = failure(&format!("{START}{TEXT}{sse}"));
        assert_detail(&after, kind, FailurePhase::Stream, retry);
        assert!(after.possibly_billed, "{sse}");
    }
    assert!(failure(&bare).message.contains("the upstream went away"));
    assert_eq!(
        failure(&openai_without_status).message,
        "slow down {provider}, [redacted], Bearer '[redacted]'"
    );
    let org = "org-PLACEHOLDER0000000000000000";
    let chunk = |text: &str, finish: Option<&str>| {
        data(&json!({"choices": [{"index": 0, "delta": {"content": text}, "finish_reason": finish}]}).to_string())
    };
    let straddles = format!(
        "{}{}",
        chunk(&format!("{} {}", "x".repeat(1980), &org[..19]), None),
        chunk(&format!("{} tail", &org[19..]), Some("stop"))
    );
    let shown = failure(&straddles).message;
    assert!(shown.ends_with(" [redacted] tail"), "{shown}");

    let secret_head = "CANARY-opaque-gateway";
    let secret_tail = "[private]";
    crate::session::register_secrets([format!("{secret_head}{secret_tail}")]);
    let trace = format!("trace CANARY1{} key: {secret_head}", "A".repeat(8190));
    for finish in [Some("stop"), None] {
        let sse = chunk(&trace, None) + &chunk(&format!("{secret_tail} was rejected"), finish);
        let error = failure(&sse);
        assert_detail(
            &error,
            ErrorKind::InvalidResponse,
            FailurePhase::Response,
            RetryClass::Never,
        );
        assert_eq!(error.detail.source, FailureSource::Endpoint);
        assert_eq!(error.detail.status, Some(200));
        assert!(error.possibly_billed);
        assert_eq!(
            error.message,
            "the server replied with text Wisp can't read as a reply: no message"
        );
    }
    assert!(!failure(&openai_without_status).possibly_billed);
    assert!(failure(&openai_with_status).possibly_billed);
}

#[test]
fn identity_shaped_tokens_leave_provider_text() {
    use crate::provider::{ErrorText, scrub_error_text};

    for (text, kept) in [
        (
            "your organization (00000000-0000-0000-0000-000000000000) of 50,000",
            "your organization ([redacted]) of 50,000",
        ),
        (
            "org FFFFFFFF-FFFF-FFFF-FFFF-FFFFFFFFFFFF.",
            "org [redacted].",
        ),
        (
            "in org-PLACEHOLDER0000000000000000 for user-PLACEHOLDER0000000000000000 in proj_PLACEHOLDER0000000000000000 or org-PLACEHOLDER000000000",
            "in [redacted] for [redacted] in [redacted] or [redacted]",
        ),
        (
            "req_PLACEHOLDER0000000000000000 toolu_PLACEHOLDER0000 user-provided org-wide messages.1.content.0",
            "[redacted] [redacted] user-provided org-wide messages.1.content.0",
        ),
        (
            "0000000-0000-0000-0000-000000000000 x00000000-0000-0000-0000-000000000000 org-placeholderplaceholderx- 00000000000000000000000000000000 org-placeholder",
            "0000000-0000-0000-0000-000000000000 x00000000-0000-0000-0000-000000000000 org-placeholderplaceholderx- 00000000000000000000000000000000 org-placeholder",
        ),
    ] {
        assert_eq!(scrub_error_text(text, ErrorText::Whole), kept, "{text}");
    }
}

#[test]
fn text_only_chat_chunks_are_an_invalid_response_that_is_never_retried() {
    let e = failure(TEXT_ONLY_CHUNKS);
    assert_detail(
        &e,
        ErrorKind::InvalidResponse,
        FailurePhase::Response,
        RetryClass::Never,
    );
    assert_eq!(e.detail.error_type, None);
    assert!(
        e.message
            .starts_with("the server replied with text Wisp can't read as a reply: "),
        "{}",
        e.message
    );
    assert!(
        e.message
            .contains("[gateway M500] Something went wrong on our side. Please try again."),
        "{}",
        e.message
    );
}

#[tokio::test]
async fn error_objects_in_a_200_json_body_are_read_in_every_shape() {
    let cases = [
        (
            r#"{"type":"error","error":{"type":"overloaded_error","message":"busy"},"request_id":"req_b"}"#,
            ErrorKind::Overloaded,
            RetryClass::Transient,
            false,
        ),
        (
            r#"{"error":{"message":"bad gateway","type":"server_error","status":502}}"#,
            ErrorKind::Server,
            RetryClass::Transient,
            true,
        ),
        (
            r#"{"error":{"message":"quota","type":"insufficient_quota","code":"insufficient_quota"}}"#,
            ErrorKind::Billing,
            RetryClass::Never,
            false,
        ),
        (
            r#"{"error":"model not allowed on this plan"}"#,
            ErrorKind::InvalidResponse,
            RetryClass::Never,
            true,
        ),
        (
            r#"{"type":"error","code":"rate_limit_exceeded","message":"slow"}"#,
            ErrorKind::RateLimit,
            RetryClass::Transient,
            false,
        ),
        (
            r#"{"choices":[{"message":{"content":"[gateway M500] internal error"},"finish_reason":"stop"}]}"#,
            ErrorKind::InvalidResponse,
            RetryClass::Never,
            true,
        ),
    ];
    for (body, kind, retry, billed) in cases {
        let e = last_error(http::json("200 OK", body)).await;
        assert_detail(&e, kind, FailurePhase::Response, retry);
        assert_eq!(e.possibly_billed, billed, "{body}");
        assert_eq!(e.detail.source, FailureSource::Endpoint, "{body}");
    }
    let e = last_error(http::json(
        "200 OK",
        r#"{"choices":[{"message":{"content":"[gateway M500] internal error"},"finish_reason":"stop"}]}"#,
    ))
    .await;
    assert!(
        e.message.contains("[gateway M500] internal error"),
        "{}",
        e.message
    );
    let e = last_error(http::json(
        "200 OK",
        r#"{"type":"error","error":{"type":"overloaded_error","message":"busy"},"request_id":"req_b"}"#,
    ))
    .await;
    assert_eq!(e.detail.request_id.as_deref(), Some("req_b"));
}

#[tokio::test]
async fn http_error_bodies_are_classified_by_status_and_error_object() {
    let cases = [
        (
            "402 Payment Required",
            None,
            r#"{"type":"error","error":{"type":"billing_error","message":"pay"}}"#,
            ErrorKind::Billing,
            RetryClass::Never,
            false,
        ),
        (
            "400 Bad Request",
            None,
            r#"{"error":{"message":"too long","type":"invalid_request_error","code":"context_length_exceeded"}}"#,
            ErrorKind::ContextOverflow,
            RetryClass::Never,
            false,
        ),
        (
            "409 Conflict",
            None,
            r#"{"type":"error","error":{"type":"conflict_error","message":"again"}}"#,
            ErrorKind::Server,
            RetryClass::Transient,
            false,
        ),
        (
            "504 Gateway Timeout",
            None,
            "upstream timed out",
            ErrorKind::Timeout,
            RetryClass::Transient,
            true,
        ),
        (
            "502 Bad Gateway",
            Some("req_502"),
            r#"{"type":"error","error":{"type":"api_error","message":"Upstream failed"}}"#,
            ErrorKind::Server,
            RetryClass::Transient,
            true,
        ),
        (
            "403 Forbidden",
            None,
            r#"{"error":{"message":"bad key","type":"invalid_request_error","code":"invalid_api_key"}}"#,
            ErrorKind::Auth,
            RetryClass::Never,
            false,
        ),
    ];
    for (status, id, body, kind, retry, billed) in cases {
        let extra: Vec<_> = id.map(|id| ("request-id", id)).into_iter().collect();
        let e = last_error(json_reply(status, &extra, body)).await;
        assert_detail(&e, kind, FailurePhase::Response, retry);
        assert_eq!(e.possibly_billed, billed, "{status}");
        assert_eq!(e.detail.source, FailureSource::Endpoint);
        let code: u16 = status[..3].parse().unwrap();
        assert_eq!(e.detail.status, Some(code));
    }
    let e = last_error(json_reply(
        "502 Bad Gateway",
        &[("request-id", "req_502")],
        r#"{"type":"error","error":{"type":"api_error","message":"Upstream failed"}}"#,
    ))
    .await;
    assert_eq!(e.detail.request_id.as_deref(), Some("req_502"));
    assert_eq!(e.detail.error_type.as_deref(), Some("api_error"));
    assert_eq!(e.message, "Upstream failed");

    let prefix = "x".repeat(474);
    let email = ["canary-account", "example.invalid"].join("@");
    let raw = format!("{prefix} sk-ant-api03-CANARY_ab1_cd2_ef3 {email}");
    let error = last_error(json_reply("504 Gateway Timeout", &[], &raw)).await;
    assert_eq!(error.message, format!("{prefix} [redacted] [redacted]"));
}

#[tokio::test]
async fn a_429_with_an_openai_style_body_carries_its_retry_after() {
    let body =
        r#"{"error":{"message":"slow down","type":"requests","code":"rate_limit_exceeded"}}"#;
    let (base, _) = mock_server(vec![json_reply(
        "429 Too Many Requests",
        &[("retry-after-ms", "1500"), ("retry-after", "0")],
        body,
    )])
    .await;
    let provider = AnthropicProvider::new(&api_key("k"), &base).unwrap();
    let s = session(None, CacheTtl::OneHour);
    let items: Vec<_> = provider.stream(&hi_request(&s)).collect().await;
    let Some(Err(e)) = items.last() else {
        panic!("{items:?}")
    };
    assert_detail(
        e,
        ErrorKind::RateLimit,
        FailurePhase::Response,
        RetryClass::After { ms: 1500 },
    );
    assert_eq!(e.detail.error_type.as_deref(), Some("rate_limit_exceeded"));
}

#[tokio::test]
async fn transport_failures_are_local_and_name_their_phase() {
    let (_socket, addr) = refusing_addr();
    let refused = reqwest::Client::new()
        .get(format!("http://{addr}"))
        .send()
        .await
        .unwrap_err();
    let connect = send_failure(refused);
    assert_detail(
        &connect,
        ErrorKind::Network,
        FailurePhase::Connect,
        RetryClass::Transient,
    );
    assert_eq!(connect.detail.source, FailureSource::Local);
    assert!(!connect.possibly_billed);

    let lost = last_error(String::new()).await;
    assert_detail(
        &lost,
        ErrorKind::Network,
        FailurePhase::Response,
        RetryClass::Transient,
    );
    assert_eq!(lost.detail.source, FailureSource::Local);
    assert!(lost.possibly_billed);

    let cut = failure(CUT_OFF);
    assert_detail(
        &cut,
        ErrorKind::Network,
        FailurePhase::Stream,
        RetryClass::Transient,
    );
    assert_eq!(cut.detail.source, FailureSource::Local);

    let empty = last_error(http::sse("")).await;
    assert_detail(
        &empty,
        ErrorKind::InvalidResponse,
        FailurePhase::Stream,
        RetryClass::Transient,
    );
    assert!(empty.possibly_billed);

    let malformed = failure(&format!(
        "{START}{}",
        http::event(
            "content_block_start",
            r#"{"type":"content_block_start","index":"zero"}"#
        )
    ));
    assert_detail(
        &malformed,
        ErrorKind::InvalidResponse,
        FailurePhase::Stream,
        RetryClass::Never,
    );
    assert_eq!(malformed.detail.source, FailureSource::Local);

    let bad_delta = failure(&format!(
        "{START}{}",
        http::event(
            "content_block_delta",
            r#"{"type":"content_block_delta","index":4,"delta":{"type":"text_delta","text":"x"}}"#
        )
    ));
    assert_detail(
        &bad_delta,
        ErrorKind::InvalidResponse,
        FailurePhase::Stream,
        RetryClass::Never,
    );
}

#[tokio::test]
async fn a_redirect_is_an_invalid_response_from_the_endpoint() {
    let e = last_error(http::response(
        "302 Found",
        &[("location", "http://127.0.0.1:1/")],
        "",
    ))
    .await;
    assert_detail(
        &e,
        ErrorKind::InvalidResponse,
        FailurePhase::Response,
        RetryClass::Never,
    );
    assert_eq!(e.detail.status, Some(302));
}

#[test]
fn a_feature_rejection_or_binding_in_the_first_event_is_never_retried() {
    let binding = http::event(
        "error",
        format!(
            r#"{{"type":"error","error":{{"type":"invalid_request_error","message":"{THINKING_BINDING_TEXT}."}}}}"#
        ),
    );
    let e = failure(&binding);
    assert_eq!(e.kind, ErrorKind::ThinkingBinding);
    assert_eq!(e.detail.retry, RetryClass::Never);

    let rejection = http::event(
        "error",
        format!(
            r#"{{"type":"error","error":{{"type":"invalid_request_error","message":"Unexpected value(s) `{PER_MESSAGE_EFFORT_BETA}` for the `anthropic-beta` header."}}}}"#
        ),
    );
    let mut decoder = StreamDecoder::new(
        "claude-opus-5-5",
        Features::trying(&[Feature::PerMessageEffort]),
        None,
    );
    let mut items = decoder.push(rejection.as_bytes());
    let Some(Err(e)) = items.pop() else {
        panic!("{items:?}")
    };
    let e = billed_in_stream(e);
    assert_eq!(e.rejected, vec![Feature::PerMessageEffort]);
    assert_eq!(e.kind, ErrorKind::InvalidRequest);
    assert_eq!(e.detail.retry, RetryClass::Never);
    assert!(!e.possibly_billed);
    assert!(!e.detail.possibly_billed);
}

#[test]
fn the_detail_round_trips_through_the_wire_format() {
    let e = failure(TURN_18);
    let json = serde_json::to_string(&e.detail).unwrap();
    assert_eq!(
        json,
        r#"{"phase":"stream","source":"endpoint","status":200,"error_type":"api_error","retry":{"type":"transient"},"possibly_billed":true}"#
    );
    let back: ErrorDetail = serde_json::from_str(&json).unwrap();
    assert_eq!(back, *e.detail);
}

#[tokio::test]
async fn turn_18_ends_the_turn_with_its_detail() {
    use crate::agent::Agent;
    use tokio::sync::mpsc;
    use wisp_protocol::{AgentEvent, ClientCommand, TurnOutcome};

    let (base, _) = mock_server(vec![http::sse_with(&[("request-id", "req_018")], TURN_18)]).await;
    let provider = AnthropicProvider::new(&api_key("k"), &base).unwrap();
    let mut agent = Agent::new(Arc::new(provider), crate::agent::default_session())
        .with_retry_policy(crate::agent::retry::RetryPolicy {
            step_retries: Some(0),
            ..crate::agent::retry::quick()
        });
    let (_tx, mut rx) = mpsc::channel::<ClientCommand>(4);
    let (ev_tx, mut ev_rx) = mpsc::channel::<AgentEvent>(256);
    let outcome = agent.run_turn("hi".into(), &mut rx, &ev_tx).await.unwrap();
    drop(ev_tx);
    while ev_rx.recv().await.is_some() {}
    let TurnOutcome::Error { kind, detail, .. } = &outcome else {
        panic!("{outcome:?}")
    };
    assert_eq!(*kind, ErrorKind::Server);
    let detail = detail.as_ref().unwrap();
    assert_eq!(detail.phase, FailurePhase::Stream);
    assert_eq!(detail.source, FailureSource::Endpoint);
    assert_eq!(detail.error_type.as_deref(), Some("api_error"));
    assert_eq!(detail.request_id.as_deref(), Some("req_018"));
    assert_eq!(detail.retry, RetryClass::Transient);
    assert!(detail.possibly_billed);
}
