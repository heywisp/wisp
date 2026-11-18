use std::collections::BTreeMap;
use std::time::Duration;

use futures::StreamExt;
use serde_json::{Value, json};
use tokio::net::TcpListener;
use wisp_protocol::{Billing, CacheTtl, ContentBlock, ErrorKind, Message};

use super::{
    THINKING_AND_TEXT, UNKNOWN_TYPES, assistant, json_reply, mock_server, plain_request, session,
};
use crate::agent::testing::body_of;
use crate::provider::anthropic::subscription::{Profile, SYSTEM_IDENTITY, data};
use crate::provider::anthropic::{
    AnthropicProvider, ConfigError, DEFAULT_BASE_URL, MESSAGE_THREADS_BETA, Marks, encode, layout,
};
use crate::provider::credential::{Credential, SubscriptionKind};
use crate::provider::{
    Assembler, Provider, ProviderEvent, Request, RequestOptions, TailMarker, Thread, TokenCount,
};
use crate::test_guard::{http, until_turn_end};
use crate::tool::ToolSpec;

pub(super) fn credential(token: &str) -> Credential {
    Credential::Subscription {
        kind: SubscriptionKind::Anthropic,
        secret: token.to_owned(),
        profile: None,
    }
}

fn text(text: &str) -> ContentBlock {
    ContentBlock::Text {
        text: text.to_owned(),
    }
}

fn provider() -> AnthropicProvider {
    AnthropicProvider::new(&credential("subscription-test-token"), DEFAULT_BASE_URL).unwrap()
}

pub(super) fn local_provider(base: &str) -> AnthropicProvider {
    let mut provider = provider();
    provider.url = format!("{base}/v1/messages");
    provider
}

fn encoded(request: &Request<'_>) -> Vec<u8> {
    encode(
        request,
        &layout(request),
        Marks::Automatic,
        Profile::Manifest,
        true,
    )
    .unwrap()
}

const ECHOED: &str = "echoed-test-token";

fn headers(raw: &str) -> BTreeMap<String, String> {
    http::header_lines(raw).into_iter().collect()
}

#[test]
fn subscription_mode_is_explicit_and_provider_specific() {
    for credential in [
        Credential::ApiKey("api-test-key".into()),
        Credential::AuthToken("sk-ant-oat-test-token".into()),
    ] {
        let provider = AnthropicProvider::with_auth(
            super::super::header(&credential).unwrap(),
            &credential,
            DEFAULT_BASE_URL,
            None,
        )
        .unwrap();
        assert_eq!(provider.profile, Profile::Api);
    }
    assert_eq!(provider().profile, Profile::Manifest);
    let credential = credential("missing-identity-test-token");
    let error = AnthropicProvider::with_auth(
        super::super::header(&credential).unwrap(),
        &credential,
        DEFAULT_BASE_URL,
        None,
    )
    .unwrap_err();
    assert!(matches!(error, ConfigError::SubscriptionIdentity));
    assert_eq!(
        error.to_string(),
        "the Claude subscription identity data is unavailable"
    );
    let wrong = Credential::Subscription {
        kind: SubscriptionKind::ChatGpt,
        secret: "different-provider-test-token".into(),
        profile: None,
    };
    assert!(matches!(
        AnthropicProvider::new(&wrong, DEFAULT_BASE_URL),
        Err(ConfigError::WrongSubscription)
    ));
}

#[test]
fn subscription_credentials_are_pinned_to_the_exact_https_origin() {
    for base in [DEFAULT_BASE_URL, "https://api.anthropic.com/"] {
        assert!(AnthropicProvider::new(&credential("test-token"), base).is_ok());
    }
    for base in [
        "http://api.anthropic.com",
        "https://api.anthropic.com:8443",
        "https://api.anthropic.com.evil.invalid",
        "https://api.anthropic.com@evil.invalid",
        "https://user@api.anthropic.com",
        "https://api.anthropic.com/v1",
        "https://api.anthropic.com/?token=test",
        "https://api.anthropic.com/#fragment",
        "http://127.0.0.1:4000",
        "not a URL",
    ] {
        assert!(
            matches!(
                AnthropicProvider::new(&credential("test-token"), base),
                Err(ConfigError::SubscriptionOrigin)
            ),
            "{base}"
        );
    }
}

#[test]
fn subscription_identity_is_the_only_native_body_change() {
    let mut session = session(Some("You are Wisp."), CacheTtl::FiveMinutes);
    session.instructions = Some("Answer briefly.".into());
    let native =
        json!({"type": "future_block", "signature": "native-signature", "payload": [1, 2]});
    let messages = [
        Message::user_text("list files"),
        assistant(vec![
            ContentBlock::Thinking {
                thinking: "Inspect the files.".into(),
                signature: "signed-thinking-test".into(),
            },
            ContentBlock::RedactedThinking {
                data: "native-encrypted-block".into(),
            },
            ContentBlock::opaque("anthropic", native),
            ContentBlock::ToolUse {
                id: "tool-1".into(),
                name: "bash".into(),
                input: json!({"command": "ls"}),
            },
        ]),
        Message {
            role: wisp_protocol::Role::User,
            content: vec![ContentBlock::ToolResult {
                tool_use_id: "tool-1".into(),
                content: vec![text("a.rs")],
                is_error: false,
            }],
        },
    ];
    let tools = [ToolSpec {
        name: "bash".into(),
        description: "Run a command.".into(),
        input_schema: json!({"type": "object", "properties": {"command": {"type": "string"}}}),
    }];
    let request = Request {
        tools: &tools,
        ..plain_request(RequestOptions::default(), &session, &messages, None)
    };
    let mut expected: Value = body_of(&request);
    expected["system"]
        .as_array_mut()
        .unwrap()
        .insert(0, json!({"type": "text", "text": SYSTEM_IDENTITY}));
    let body: Value = serde_json::from_slice(&encoded(&request)).unwrap();
    assert_eq!(body, expected);
    assert_eq!(body["system"][1]["cache_control"]["ttl"], "1h");
    assert_eq!(body["system"][2]["cache_control"]["ttl"], "1h");
    assert_eq!(body["cache_control"], json!({"type": "ephemeral"}));
}

#[test]
fn changing_from_openai_keeps_projections_and_omits_its_private_sidecar() {
    let session = session(Some("You are Wisp."), CacheTtl::OneHour);
    let messages = [
        Message::user_text("hello"),
        assistant(vec![
            ContentBlock::opaque(
                "openai",
                json!({
                    "type": "wisp_native_item",
                    "item": {"type": "reasoning", "encrypted_content": "foreign-native-state"},
                    "projection_blocks": 1,
                    "identity": "local-binding",
                }),
            ),
            ContentBlock::Thinking {
                thinking: "foreign reasoning summary".into(),
                signature: String::new(),
            },
            text("Semantic answer."),
        ]),
        Message::user_text("continue"),
    ];
    let request = plain_request(RequestOptions::default(), &session, &messages, None);
    let encoded = encoded(&request);
    let body: Value = serde_json::from_slice(&encoded).unwrap();
    assert_eq!(
        body["messages"][1]["content"],
        json!([{"type": "text", "text": "Semantic answer."}])
    );
    let body = String::from_utf8(encoded).unwrap();
    assert!(!body.contains("foreign-native-state"));
    assert!(!body.contains("local-binding"));
    assert!(!body.contains("foreign reasoning summary"));
}

#[test]
fn foreign_text_from_openai_drops_nested_identity() {
    let session = session(Some("You are Wisp."), CacheTtl::OneHour);
    let messages = [
        Message::user_text("hello"),
        assistant(vec![ContentBlock::opaque(
            "openai",
            json!({
                "type": "wisp_native_item",
                "item": {"type": "web_search_call", "action": {"query": "rust", "user_id": "private-id"}},
                "projection_blocks": 0,
            }),
        )]),
        Message::user_text("continue"),
    ];
    let request = plain_request(RequestOptions::default(), &session, &messages, None);
    let body: Value = serde_json::from_slice(&encoded(&request)).unwrap();
    let sent = body["messages"][1]["content"][0]["text"].as_str().unwrap();
    assert_eq!(sent, "[web search: \"rust\"]");
    assert!(!sent.contains("user_id"), "{sent}");
    assert!(!sent.contains("private-id"), "{sent}");
}

#[test]
fn profile_prefix_and_actual_sent_body_stay_equal_across_turns_and_token_changes() {
    let session = session(Some("You are Wisp."), CacheTtl::OneHour);
    let mut messages = vec![Message::user_text("hello")];
    let first = plain_request(RequestOptions::default(), &session, &messages, None);
    let first_bytes = provider().prefix_bytes(&first).unwrap();
    let first_prefix = provider().cache_prefix(&first).unwrap();
    let first_body = String::from_utf8(encoded(&first)).unwrap();
    assert!(first_body.contains(&format!(
        "\"system\":{}",
        std::str::from_utf8(&first_bytes.system).unwrap()
    )));
    assert_ne!(
        first_bytes,
        AnthropicProvider::new(
            &Credential::AuthToken("ordinary-test-token".into()),
            DEFAULT_BASE_URL
        )
        .unwrap()
        .prefix_bytes(&first)
        .unwrap()
    );
    assert_ne!(
        first_prefix,
        crate::provider::anthropic::sent_prefix(&first).unwrap()
    );
    messages.push(assistant(vec![text("Hello.")]));
    messages.push(Message::user_text("again"));
    let second = plain_request(
        RequestOptions::default(),
        &session,
        &messages,
        Some("msg-test"),
    );
    let replacement =
        AnthropicProvider::new(&credential("replacement-test-token"), DEFAULT_BASE_URL).unwrap();
    assert_eq!(replacement.prefix_bytes(&second).unwrap(), first_bytes);
    let second_body = String::from_utf8(encoded(&second)).unwrap();
    assert!(second_body.contains(&format!(
        "\"system\":{}",
        std::str::from_utf8(&first_bytes.system).unwrap()
    )));
    assert!(!second_body.contains("test-token"));
}

#[test]
fn explicit_tail_markers_use_the_subscription_prefix() {
    let session = session(Some("You are Wisp."), CacheTtl::OneHour);
    let messages = [Message::user_text("hello"), Message::user_text("again")];
    let anchor = plain_request(RequestOptions::default(), &session, &messages[..1], None);
    let prefix = provider().cache_prefix(&anchor).unwrap();
    let api_prefix = crate::provider::anthropic::sent_prefix(&anchor).unwrap();
    let request = plain_request(RequestOptions::default(), &session, &messages, None);
    for (prefix, placed) in [(prefix, true), (api_prefix, false)] {
        let body = encode(
            &request,
            &layout(&request),
            Marks::Explicit(Some(TailMarker {
                view_len: 1,
                last_blocks: None,
                ttl: CacheTtl::OneHour,
                prefix,
            })),
            Profile::Manifest,
            true,
        )
        .unwrap();
        let body: Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(
            body["messages"][0]["content"][0]
                .get("cache_control")
                .is_some(),
            placed
        );
    }
}

#[tokio::test]
async fn subscription_http_profile_and_native_sse_are_preserved() {
    let (base, seen) = mock_server(vec![
        http::sse_with(&[("request-id", ECHOED)], THINKING_AND_TEXT),
        http::sse_with(&[("request-id", ECHOED)], UNKNOWN_TYPES),
    ])
    .await;
    let provider = local_provider(&base);
    let session = session(Some("You are Wisp."), CacheTtl::OneHour);
    let mut history = vec![Message::user_text("hello")];
    let first = plain_request(RequestOptions::default(), &session, &history, None);
    let mut assembler = Assembler::default();
    for event in provider.stream(&first).collect::<Vec<_>>().await {
        let event = event.unwrap();
        if let ProviderEvent::MessageStart {
            usage, iterations, ..
        }
        | ProviderEvent::MessageDelta {
            usage, iterations, ..
        } = &event
        {
            assert_eq!(usage.billing, Billing::AnthropicPlan);
            assert!(
                iterations
                    .iter()
                    .all(|iteration| iteration.usage.billing == Billing::AnthropicPlan)
            );
        }
        assembler.apply(event).unwrap();
    }
    let message = assembler.finish().unwrap();
    assert_eq!(message.usage.billing, Billing::AnthropicPlan);
    assert!(matches!(
        &message.content[0],
        ContentBlock::Thinking { signature, .. } if signature == "EqQBCkYIBxgCKkCq7n+Z/xw=="
    ));
    history.push(message.into_message());
    history.push(Message::user_text("again"));
    let mut second = plain_request(RequestOptions::default(), &session, &history, Some("msg_1"));
    second.features.thread = Some(Thread::Continue { held: 2 });
    let mut assembler = Assembler::default();
    for event in provider.stream(&second).collect::<Vec<_>>().await {
        assembler.apply(event.unwrap()).unwrap();
    }
    assert!(assembler.finish().unwrap().content.iter().any(|block| {
        matches!(block, ContentBlock::Opaque { raw, .. } if raw["type"] == "hologram")
    }));
    let requests = seen.lock().unwrap();
    assert_eq!(requests.len(), 2);
    let sent_headers = headers(&requests[0]);
    for (name, expected) in [
        ("authorization", "Bearer subscription-test-token"),
        ("anthropic-version", "2023-06-01"),
        ("content-type", "application/json"),
    ] {
        assert_eq!(
            sent_headers.get(name).map(String::as_str),
            Some(expected),
            "{name}"
        );
    }
    let identity = data().unwrap();
    for (name, expected) in &identity.headers {
        assert_eq!(
            sent_headers.get(name.as_str()).map(String::as_str),
            Some(expected.to_str().unwrap()),
            "{name}"
        );
    }
    assert!(!sent_headers.contains_key("x-api-key"));
    let betas = sent_headers["anthropic-beta"]
        .split(',')
        .collect::<Vec<_>>();
    let mut expected_betas: Vec<_> = identity.betas.iter().map(String::as_str).collect();
    expected_betas.push(crate::provider::anthropic::FALLBACK_BETA);
    assert_eq!(betas, expected_betas);
    assert_eq!(
        provider.wire_print(&second).unwrap().betas,
        crate::session::sha256_hex(&identity.betas.join(","))
    );
    assert_eq!(
        headers(&requests[1])["anthropic-beta"],
        format!("{},{MESSAGE_THREADS_BETA}", sent_headers["anthropic-beta"])
    );
    let second: Value = serde_json::from_str(http::body(&requests[1])).unwrap();
    assert!(second.get("system").is_none() && second.get("tools").is_none());
    assert_eq!(
        second["messages"],
        json!([{"role": "user", "content": [{"type": "text", "text": "again"}]}])
    );
    assert_eq!(second["thread"]["previous_message_id"], "msg_1");
}

#[tokio::test]
async fn api_key_and_ordinary_bearer_usage_keep_metered_provenance() {
    for credential in [
        Credential::ApiKey("synthetic-api-key".into()),
        Credential::AuthToken("synthetic-bearer-token".into()),
    ] {
        let (base, _) = mock_server(vec![http::sse_with(
            &[("request-id", ECHOED)],
            THINKING_AND_TEXT,
        )])
        .await;
        let provider = AnthropicProvider::new(&credential, &base).unwrap();
        let session = session(None, CacheTtl::OneHour);
        let messages = [Message::user_text("hello")];
        let request = plain_request(RequestOptions::default(), &session, &messages, None);
        let mut usages = Vec::new();
        for event in provider.stream(&request).collect::<Vec<_>>().await {
            if let ProviderEvent::MessageStart {
                usage, iterations, ..
            }
            | ProviderEvent::MessageDelta {
                usage, iterations, ..
            } = event.unwrap()
            {
                usages.push(usage);
                usages.extend(iterations.into_iter().map(|iteration| iteration.usage));
            }
        }
        assert!(usages.len() >= 2);
        assert!(usages.iter().all(|usage| usage.billing == Billing::Metered));
    }
}

#[tokio::test]
async fn subscription_errors_keep_authentication_kind_and_hide_provider_auth_text() {
    let error =
        json!({"type": "error", "error": {"type": "authentication_error", "message": ECHOED}});
    let responses = [
        json_reply(
            "401 Unauthorized",
            &[("request-id", ECHOED)],
            &error.to_string(),
        ),
        json_reply(
            "403 Forbidden",
            &[("request-id", ECHOED)],
            &error.to_string(),
        ),
        http::sse_with(&[("request-id", ECHOED)], &http::event("error", error)),
    ];
    let session = session(None, CacheTtl::OneHour);
    let messages = [Message::user_text("hello")];
    let request = plain_request(RequestOptions::default(), &session, &messages, None);
    for response in responses {
        let (base, _) = mock_server(vec![response]).await;
        let events = local_provider(&base)
            .stream(&request)
            .collect::<Vec<_>>()
            .await;
        let error = events.into_iter().find_map(Result::err).unwrap();
        assert_eq!(error.kind, ErrorKind::Auth);
        assert!(!format!("{error:?}").contains(ECHOED));
        assert_eq!(
            error.message,
            "Anthropic subscription authentication failed; sign in again with /account"
        );
        assert!(error.detail.error_type.is_none());
        assert!(error.detail.request_id.is_none());
    }
}

async fn refusal(
    status: &str,
    name: Option<&str>,
    message: &str,
) -> crate::provider::ProviderError {
    let body = match name {
        Some(name) => json!({"type": "error", "error": {"type": name, "message": message}}),
        None => json!(message),
    };
    let reply = json_reply(status, &[("request-id", ECHOED)], &body.to_string());
    let (base, _) = mock_server(vec![reply]).await;
    let session = session(None, CacheTtl::OneHour);
    let messages = [Message::user_text("hello")];
    let request = plain_request(RequestOptions::default(), &session, &messages, None);
    let events = local_provider(&base)
        .stream(&request)
        .collect::<Vec<_>>()
        .await;
    events.into_iter().find_map(Result::err).unwrap()
}

#[tokio::test]
async fn subscription_keeps_a_request_refusals_type_and_scrubbed_text_and_masks_the_rest() {
    const SECRET: &str = "registered-canary-token";
    const KEY: &str = "sk-ant-CANARY0123456789abcdefghijklmnop";
    crate::session::register_secrets([SECRET.to_owned()]);
    let error = refusal(
        "404 Not Found",
        Some("not_found_error"),
        "model: claude-sonnet-4-5-20250929",
    )
    .await;
    assert_eq!(error.kind, ErrorKind::InvalidRequest);
    assert_eq!(error.message, "model: claude-sonnet-4-5-20250929");
    assert_eq!(error.detail.error_type.as_deref(), Some("not_found_error"));
    assert!(error.detail.request_id.is_none());
    assert!(!format!("{error:?}").contains(ECHOED));

    let text = format!(
        "model: claude-sonnet-4-5-20250929 beta context-1m-2025-08-07 messages.0: bad {SECRET} and {KEY}"
    );
    let error = refusal("400 Bad Request", Some("invalid_request_error"), &text).await;
    assert_eq!(
        error.message,
        "model: claude-sonnet-4-5-20250929 beta context-1m-2025-08-07 messages.0: bad [redacted] and [redacted]"
    );
    assert_eq!(
        error.detail.error_type.as_deref(),
        Some("invalid_request_error")
    );
    let filler = "x ".repeat(989);
    let split = format!("{filler}{SECRET} and more words");
    let error = refusal("400 Bad Request", Some("invalid_request_error"), &split).await;
    assert_eq!(error.message, format!("{filler}[redacted] and more "));

    let masked = "Anthropic subscription request failed (invalid request)";
    for (name, message) in [
        (Some("weird_error"), "PRIVATE_CANARY"),
        (None, "PRIVATE_CANARY"),
    ] {
        let error = refusal("400 Bad Request", name, message).await;
        assert_eq!(error.message, masked);
        assert!(error.detail.error_type.is_none());
        assert!(!format!("{error:?}").contains("CANARY"));
    }
    for (status, name) in [
        ("401 Unauthorized", "invalid_request_error"),
        ("403 Forbidden", "not_found_error"),
        ("429 Too Many Requests", "invalid_request_error"),
        ("402 Payment Required", "not_found_error"),
        ("500 Internal Server Error", "not_found_error"),
    ] {
        let error = refusal(status, Some(name), "PRIVATE_CANARY").await;
        assert!(error.detail.error_type.is_none(), "{status}");
        assert!(!format!("{error:?}").contains("CANARY"), "{status}");
    }
}

#[tokio::test]
async fn a_subscription_token_that_fails_sends_nothing_and_emits_no_sending() {
    let scratch = crate::test_guard::ScratchDir::new("wisp-subscription-test-", "token");
    let credential = Credential::Subscription {
        kind: SubscriptionKind::Anthropic,
        secret: "subscription-test-token".to_owned(),
        profile: Some(scratch.root.join("profile.json")),
    };
    let (base, seen) = mock_server(vec![http::sse_with(
        &[("request-id", ECHOED)],
        THINKING_AND_TEXT,
    )])
    .await;
    let mut provider = AnthropicProvider::new(&credential, DEFAULT_BASE_URL).unwrap();
    provider.url = format!("{base}/v1/messages");
    let session = session(None, CacheTtl::OneHour);
    let messages = [Message::user_text("hello")];
    let request = plain_request(RequestOptions::default(), &session, &messages, None);
    let events = provider.stream(&request).collect::<Vec<_>>().await;
    assert!(matches!(events.as_slice(), [Err(_)]), "{events:?}");
    assert!(seen.lock().unwrap().is_empty());
}

#[tokio::test]
async fn subscription_does_not_follow_redirects_or_infer_token_count_support() {
    let destination = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let stolen = format!("http://{}/stolen", destination.local_addr().unwrap());
    let redirect = http::response("307 Temporary Redirect", &[("location", &stolen)], "");
    let (base, _) = mock_server(vec![redirect]).await;
    let provider = local_provider(&base);
    let session = session(None, CacheTtl::OneHour);
    let messages = [Message::user_text("hello")];
    let request = plain_request(RequestOptions::default(), &session, &messages, None);
    let events = provider.stream(&request).collect::<Vec<_>>().await;
    assert!(events.iter().any(Result::is_err));
    assert_eq!(
        events
            .iter()
            .filter(|event| matches!(event, Ok(ProviderEvent::Sending)))
            .count(),
        1
    );
    assert!(
        tokio::time::timeout(Duration::from_millis(100), destination.accept())
            .await
            .is_err()
    );
    assert!(!provider.features_documented());
    assert_eq!(
        provider.count_tokens(&request).await,
        TokenCount::Unsupported
    );
    assert_eq!(
        provider
            .count_tokens_probe("test-model", Duration::ZERO)
            .await,
        crate::endpoints::CheckOutcome::NotOffered
    );
}

#[tokio::test]
async fn claude_usage_lookup_is_disabled_and_makes_zero_requests() {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let provider = local_provider(&format!("http://{}", listener.local_addr().unwrap()));
    assert!(provider.configure_plan_usage(true));
    assert!(provider.configure_plan_usage(false));
    assert_eq!(
        provider.plan_usage().await,
        Err(wisp_protocol::plan_usage::PlanUsageError::Disabled)
    );
    assert!(
        tokio::time::timeout(Duration::from_millis(10), listener.accept())
            .await
            .is_err()
    );
}

#[tokio::test]
async fn subscription_long_retry_after_is_a_limit_but_plain_429_remains_transient() {
    use crate::clock::{Clock, StillClock};
    use wisp_protocol::{FailurePhase, RetryClass};
    for after in [None, Some("270000"), Some("2"), Some("broken")] {
        let header: Vec<_> = after
            .map(|value| ("Retry-After", value))
            .into_iter()
            .collect();
        let body = r#"{"error":{"type":"rate_limit_error","message":"PRIVATE_CANARY"}}"#;
        let reply = http::response("429 Too Many Requests", &header, body);
        let (url, _) = mock_server(vec![reply]).await;
        let provider = local_provider(&url).with_clock(std::sync::Arc::new(StillClock));
        let session = session(None, CacheTtl::OneHour);
        let request = plain_request(RequestOptions::default(), &session, &[], None);
        let errors: Vec<_> = provider
            .stream(&request)
            .filter_map(|event| async { event.err() })
            .collect()
            .await;
        assert_eq!(errors.len(), 1);
        let error = &errors[0];
        assert_eq!(error.kind, ErrorKind::RateLimit);
        assert_eq!(error.detail.phase, FailurePhase::Response);
        assert!(!format!("{error:?}").contains("CANARY"));
        match after {
            Some("270000") => {
                assert_eq!(error.detail.retry, RetryClass::After { ms: 270_000_000 });
                let guard = error.detail.limit_reached.as_deref().unwrap();
                assert!(guard.is_retry_guard());
                assert_eq!(guard.label, "Retry after");
                assert_eq!(
                    guard.scope,
                    wisp_protocol::plan_usage::UsageScope::Model(request.session.model.clone())
                );
                assert_eq!(guard.used_percent, None);
                assert_eq!(
                    error.detail.limit_reached.as_ref().unwrap().resets_at,
                    Some(StillClock.now().as_second() + 270000)
                );
            }
            Some("2") => {
                assert_eq!(error.detail.retry, RetryClass::After { ms: 2000 });
                assert!(error.detail.limit_reached.is_none());
            }
            _ => {
                assert_eq!(error.detail.retry, RetryClass::Transient);
                assert!(error.detail.limit_reached.is_none());
            }
        }
    }
}

#[tokio::test]
async fn seventy_five_hour_subscription_refusal_ends_with_a_retry_offer() {
    use crate::clock::{Clock, StillClock};
    use crate::plan_usage::{Display, PlanUsageCache, Room};
    use tokio::sync::mpsc;
    use wisp_protocol::plan_usage::{PlanUsage, UsageScope, UsageSource, UsageWindow};
    use wisp_protocol::{AgentEvent, ClientCommand, GaveUp, RequestId, TurnOutcome};
    let now = StillClock.now().as_second();
    let refusal = http::response(
        "429 Too Many Requests",
        &[("Retry-After", "270000")],
        r#"{"error":{"type":"rate_limit_error","message":"PRIVATE_CANARY"}}"#,
    );
    let success = http::response(
        "200 OK",
        &[("Content-Type", "text/event-stream")],
        &THINKING_AND_TEXT.replace("claude-opus-5-5", "example-served-model"),
    );
    let (url, requests) = mock_server(vec![refusal, success]).await;
    let provider =
        std::sync::Arc::new(local_provider(&url).with_clock(std::sync::Arc::new(StillClock)));
    let cache = PlanUsageCache::default();
    let mut agent = crate::agent::Agent::new(provider.clone(), session(None, CacheTtl::OneHour))
        .with_clock(std::sync::Arc::new(StillClock))
        .with_plan_usage(cache.clone(), Display::Disabled);
    let alias = agent.endpoint_name().to_owned();
    let model = agent.session().model.clone();
    let generation = cache.bind(&alias, provider).unwrap();
    let reading = UsageWindow {
        key: "example-model".into(),
        label: "Example model".into(),
        duration_seconds: None,
        scope: UsageScope::Model(model.clone()),
        used_percent: Some(100.0.try_into().unwrap()),
        resets_at: Some(now + 7200),
        observed_at: now,
        source: UsageSource::UsageEndpoint,
    };
    cache.observe(
        &alias,
        generation,
        Ok(PlanUsage::new(None, vec![reading.clone()]).unwrap()),
        now,
    );
    let (send, mut commands) = mpsc::channel(4);
    let (events, mut received) = mpsc::channel(128);
    let task = tokio::spawn(async move {
        agent.serve(&mut commands, &events).await;
        agent.history().to_vec()
    });
    send.send(ClientCommand::send("one", "hello"))
        .await
        .unwrap();
    let mut seen = Vec::new();
    let AgentEvent::TurnEnded { outcome, .. } = until_turn_end(&mut received, &mut seen).await
    else {
        unreachable!()
    };
    assert!(
        matches!(&outcome, TurnOutcome::Error {
        kind: ErrorKind::RateLimit,
        committed_steps: 0,
        attempts: 1,
        gave_up: Some(GaveUp::RetryAfterTooLong),
        offer: Some(offer),
        detail: Some(detail),
        ..
    } if offer.retry && offer.retries_at == Some(now + 270060)
        && detail.limit_reached.as_ref().unwrap().is_retry_guard()
        && detail.limit_reached.as_ref().unwrap().resets_at == Some(now + 270000)),
        "{outcome:?}"
    );
    assert!(
        matches!(&outcome, TurnOutcome::Error { detail: Some(detail), .. }
        if detail.model_limit.as_deref() == Some(&reading)),
        "the actual failed turn carries its bound reading"
    );
    assert_eq!(
        cache.room(&alias, &model, now),
        Room::Used {
            model: Some(model.clone()),
            resets_at: Some(now + 270000),
            cause: wisp_tool_text::agents::Cause::Retry,
        }
    );
    assert_eq!(cache.room(&alias, "example-other", now), Room::Open);
    assert!(!serde_json::to_string(&outcome).unwrap().contains("CANARY"));
    let refused_at = seen
        .iter()
        .filter(|event| matches!(event, AgentEvent::TurnEnded { .. }))
        .count();
    assert_eq!(refused_at, 1);
    send.send(ClientCommand::Retry {
        turn: 1,
        endpoint: None,
        request: RequestId(2),
    })
    .await
    .unwrap();
    let AgentEvent::TurnEnded { outcome, .. } = until_turn_end(&mut received, &mut seen).await
    else {
        unreachable!()
    };
    assert!(
        matches!(&outcome, TurnOutcome::Completed { model, .. } if model == "example-served-model"),
        "{outcome:?}"
    );
    assert_eq!(
        cache.room(&alias, &model, now),
        Room::Used {
            model: Some(model.clone()),
            resets_at: reading.resets_at,
            cause: wisp_tool_text::agents::Cause::Limit
        },
    );
    cache.reset_lookup(&alias, now);
    assert_eq!(cache.room(&alias, &model, now), Room::Open);
    drop(send);
    assert!(!task.await.unwrap().is_empty());
    while let Some(event) = received.recv().await {
        seen.push(event);
    }
    assert_eq!(
        seen.iter()
            .filter(|event| matches!(event, AgentEvent::TurnEnded { .. }))
            .count(),
        2
    );
    assert!(
        !seen
            .iter()
            .any(|event| matches!(event, AgentEvent::Retrying { .. }))
    );
    assert_eq!(requests.lock().unwrap().len(), 2);
}
