use futures::StreamExt;
use serde_json::{Value, json};
use wisp_protocol::{CacheTtl, ContentBlock, ErrorKind, Message, Role};

use super::forms::invalid;
use super::{api_key, http, mock_server, session};
use crate::agent::testing::{body_of, text_of};
use crate::provider::anthropic::AnthropicProvider;
use crate::provider::{Features, Provider, ProviderError, Refused, Rendering, Request};
use crate::test_guard::{opaque, refusing_addr};

const OFF: &str =
    "Web search is not enabled for this organization. An admin can enable it in the Console.";
const SCHEMA: &str = "tools.1.web_search: input_schema is not allowed for this tool type";
const ENCRYPTED: &str = "messages.1.content.1.content.0.encrypted_content: could not be decrypted";

fn gateway_text(message: &str) -> String {
    let chunk = json!({"id": "x", "object": "chat.completion.chunk", "choices": [{"index": 0, "delta": {"role": "assistant", "content": message}, "finish_reason": "stop"}]});
    http::sse(&format!("data: {chunk}\n\ndata: [DONE]\n\n"))
}

fn searched() -> Vec<Message> {
    vec![
        Message::user_text("go"),
        Message {
            role: Role::Assistant,
            content: vec![
                opaque(
                    json!({"type": "server_tool_use", "id": "s1", "name": "web_search", "input": {"query": "rust"}}),
                ),
                opaque(
                    json!({"type": "web_search_tool_result", "tool_use_id": "s1", "content": [{"type": "web_search_result", "url": "https://example.com", "title": "Example", "encrypted_content": "enc"}]}),
                ),
                ContentBlock::Text { text: "ok".into() },
            ],
        },
        Message::user_text("next"),
    ]
}

async fn error_for(response: String, search: bool, history: bool) -> ProviderError {
    error_with(response, search, history, false).await
}

async fn error_with(
    response: String,
    search: bool,
    history: bool,
    provisional: bool,
) -> ProviderError {
    sent(response, Sent::own(search, history, provisional)).await
}

#[derive(Clone, Copy, Default)]
struct Sent {
    gateway: bool,
    search: bool,
    history: bool,
    provisional: bool,
    rendered: bool,
}

impl Sent {
    fn own(search: bool, history: bool, provisional: bool) -> Self {
        Self {
            search,
            history,
            provisional,
            ..Self::default()
        }
    }

    fn gateway() -> Self {
        Self {
            gateway: true,
            search: true,
            ..Self::default()
        }
    }
}

async fn sent(response: String, how: Sent) -> ProviderError {
    let messages = if how.history {
        searched()
    } else {
        vec![Message::user_text("go")]
    };
    sent_with(response, how, &messages).await
}

async fn sent_with(response: String, how: Sent, messages: &[Message]) -> ProviderError {
    let Sent {
        gateway,
        search,
        provisional,
        rendered,
        ..
    } = how;
    let (base, _) = mock_server(vec![response]).await;
    let provider = AnthropicProvider::new(&api_key("k"), &base).unwrap();
    let provider = if gateway {
        provider
    } else {
        provider.on_official_origin()
    };
    let s = session(None, CacheTtl::OneHour);
    let rendering = Rendering {
        provisional,
        native_as_text: if rendered {
            vec![false, true, false]
        } else {
            Vec::new()
        },
        ..Rendering::default()
    };
    let server_tools = if search {
        crate::agent::testing::search_tool("web_search_20250305")
    } else {
        &[]
    };
    let request = Request {
        session: &s,
        tools: &[],
        deferred: &[],
        by_value: &[],
        server_tools,
        messages,
        previous_response_id: None,
        features: Features::default(),
        options: Default::default(),
        rendering: &rendering,
        compaction: None,
    };
    let items: Vec<_> = provider.stream(&request).collect().await;
    match items.last() {
        Some(Err(e)) => e.clone(),
        other => panic!("{other:?}"),
    }
}

#[tokio::test]
async fn the_server_tool_is_sent_sorted_by_name_with_no_schema() {
    let s = session(Some("sys"), CacheTtl::OneHour);
    let tools = vec![
        crate::tool::ToolSpec {
            description: "z".into(),
            input_schema: json!({"type": "object"}),
            name: "zz".into(),
        },
        crate::tool::ToolSpec {
            description: "a".into(),
            input_schema: json!({"type": "object"}),
            name: "aa".into(),
        },
    ];
    let messages = vec![Message::user_text("go")];
    let request = Request {
        session: &s,
        tools: &tools,
        deferred: &[],
        by_value: &[],
        server_tools: crate::agent::testing::search_tool("web_search_20250305"),
        messages: &messages,
        previous_response_id: None,
        features: Features::default(),
        options: Default::default(),
        rendering: &Rendering::default(),
        compaction: None,
    };
    let body: Value = body_of(&request);
    let names: Vec<&str> = body["tools"]
        .as_array()
        .unwrap()
        .iter()
        .map(|t| t["name"].as_str().unwrap())
        .collect();
    assert_eq!(names, ["aa", "web_search", "zz"]);
    assert_eq!(
        body["tools"][1],
        json!({"name": "web_search", "type": "web_search_20250305"})
    );
}

#[tokio::test]
async fn the_organisation_refusal_is_matched_in_every_error_form() {
    let cases = [
        ("a 400", http::json("400 Bad Request", &invalid(OFF))),
        ("a JSON error body", http::json("200 OK", &invalid(OFF))),
        (
            "an error event before any other",
            http::sse(&http::event("error", invalid(OFF))),
        ),
        ("a gateway's 200 chunk", gateway_text(OFF)),
        (
            "other words for the same thing",
            http::json(
                "400 Bad Request",
                &invalid("web_search tool is NOT ENABLED on this account"),
            ),
        ),
        (
            "a model that can't search",
            http::json(
                "400 Bad Request",
                &invalid("Web search is not supported for this model"),
            ),
        ),
    ];
    for (name, response) in cases {
        let error = error_for(response.clone(), true, false).await;
        assert_eq!(
            error.refused,
            Some(Refused::NativeSearch),
            "{name}: {error:?}"
        );
        let without = error_for(response, false, false).await;
        assert_eq!(
            without.refused, None,
            "{name} without the tool: {without:?}"
        );
    }
}

#[tokio::test]
async fn other_errors_never_drop_search() {
    let start = http::event(
        "message_start",
        r#"{"type":"message_start","message":{"id":"m1","model":"claude-opus-5-5","usage":{"input_tokens":10,"output_tokens":1}}}"#,
    );
    let cases = [
        (
            "a schema error naming web_search",
            http::json("400 Bad Request", &invalid(SCHEMA)),
        ),
        (
            "search named but nothing about enabling it",
            http::json("400 Bad Request", &invalid("web search failed")),
        ),
        (
            "a search item's include value",
            http::json(
                "400 Bad Request",
                &invalid("include web_search_call.action.sources is not supported"),
            ),
        ),
        (
            "the words in a 403",
            http::json(
                "403 Forbidden",
                &json!({"type": "error", "error": {"type": "permission_error", "message": OFF}})
                    .to_string(),
            ),
        ),
        (
            "the words after the stream started",
            http::sse(&format!("{start}{}", http::event("error", invalid(OFF)))),
        ),
        (
            "the words past the gateway text Wisp reads",
            http::sse(&format!(
                "data: {}\n\ndata: {}\n\n",
                json!({"choices": [{"index": 0, "delta": {"content": "x".repeat(9000)}}]}),
                json!({"choices": [{"index": 0, "delta": {"content": OFF}, "finish_reason": "stop"}]})
            )),
        ),
    ];
    for (name, response) in cases {
        let error = error_for(response, true, false).await;
        assert_eq!(error.refused, None, "{name}: {error:?}");
    }
}

#[tokio::test]
async fn an_encrypted_field_refusal_is_cross_account_only_with_native_history() {
    let response = http::json("400 Bad Request", &invalid(ENCRYPTED));
    let error = error_for(response.clone(), true, true).await;
    assert_eq!(error.kind, ErrorKind::InvalidRequest);
    assert_eq!(error.refused, Some(Refused::CrossAccount));
    let error = error_for(response, true, false).await;
    assert_eq!(error.refused, None);
    let error = error_for(http::json("400 Bad Request", &invalid(OFF)), true, true).await;
    assert_eq!(error.refused, Some(Refused::NativeSearch));
}

const UNREADABLE_CONTENT: &str =
    "messages.36.content.0: Invalid `encrypted_content` in `search_result` block";
const UNREADABLE_INDEX: &str =
    "messages.1.content.1.citations.0: Invalid `encrypted_index` in `text` block";

fn cited_only() -> Vec<Message> {
    vec![
        Message::user_text("go"),
        Message {
            role: Role::Assistant,
            content: vec![opaque(
                json!({"type": "text", "text": "Rust is fast.", "citations": [{"type": "web_search_result_location", "url": "https://example.com", "title": "Example", "encrypted_index": "ei", "cited_text": "fast"}]}),
            )],
        },
        Message::user_text("next"),
    ]
}

#[tokio::test]
async fn an_unreadable_search_result_is_cross_account_with_native_history_and_cited_text_alone() {
    for message in [UNREADABLE_CONTENT, UNREADABLE_INDEX] {
        let response = http::json("400 Bad Request", &invalid(message));
        for (name, history, refused) in [
            ("searches", searched(), Some(Refused::CrossAccount)),
            ("cited text", cited_only(), Some(Refused::CrossAccount)),
            ("none", vec![Message::user_text("go")], None),
        ] {
            let error = sent_with(response.clone(), Sent::own(true, false, false), &history).await;
            assert_eq!(error.kind, ErrorKind::InvalidRequest, "{name}: {message}");
            assert_eq!(error.refused, refused, "{name}: {message}");
        }
    }
}

#[tokio::test]
async fn a_search_refusal_beside_a_provisional_image_is_never_taken_for_the_image() {
    let off = error_with(
        http::json("400 Bad Request", &invalid(OFF)),
        true,
        false,
        true,
    )
    .await;
    assert_eq!(off.refused, Some(Refused::NativeSearch));
    let foreign = error_with(
        http::json("400 Bad Request", &invalid(ENCRYPTED)),
        true,
        true,
        true,
    )
    .await;
    assert_eq!(foreign.refused, Some(Refused::CrossAccount));
    let other = error_with(
        http::json("400 Bad Request", &invalid("bad image")),
        true,
        false,
        true,
    )
    .await;
    assert_eq!(other.refused, Some(Refused::Attachment));
}

#[tokio::test]
async fn a_searching_request_with_a_project_block_carries_three_markers() {
    let mut s = session(Some("sys"), CacheTtl::OneHour);
    s.instructions = Some("project".into());
    let messages = vec![Message::user_text("go")];
    let request = Request {
        session: &s,
        tools: &[],
        deferred: &[],
        by_value: &[],
        server_tools: crate::agent::testing::search_tool("web_search_20250305"),
        messages: &messages,
        previous_response_id: None,
        features: Features::default(),
        options: Default::default(),
        rendering: &Rendering::default(),
        compaction: None,
    };
    let text = text_of(&request);
    assert_eq!(text.matches("cache_control").count(), 3, "{text}");
    let value: Value = serde_json::from_str(&text).unwrap();
    let hour = json!({"type": "ephemeral", "ttl": "1h"});
    assert_eq!(value["system"][0]["cache_control"], hour);
    assert_eq!(value["system"][1]["cache_control"], hour);
    assert_eq!(value["tools"][0].get("cache_control"), None);
}

fn written(total: u64, five: Option<u64>, hour: Option<u64>) -> wisp_protocol::Usage {
    wisp_protocol::Usage {
        input_tokens: 6,
        output_tokens: 234,
        cache_read_input_tokens: 3_056,
        cache_creation_input_tokens: total,
        cache_creation_5m_input_tokens: five,
        cache_creation_1h_input_tokens: hour,
        web_search_requests: 1,
        ..wisp_protocol::Usage::default()
    }
}

#[test]
fn writes_past_the_breakdown_count_at_the_given_ttl() {
    use crate::provider::anthropic::complete_writes;
    let live = written(11_635, Some(158), Some(2_898));
    let five = complete_writes(live, CacheTtl::FiveMinutes);
    assert_eq!(five.cache_creation_5m_input_tokens, Some(158 + 8_579));
    assert_eq!(five.cache_creation_1h_input_tokens, Some(2_898));
    let hour = complete_writes(live, CacheTtl::OneHour);
    assert_eq!(hour.cache_creation_5m_input_tokens, Some(158));
    assert_eq!(hour.cache_creation_1h_input_tokens, Some(2_898 + 8_579));
    let breakpoint = complete_writes(written(17_891, Some(165), Some(14)), CacheTtl::FiveMinutes);
    assert_eq!(
        breakpoint.cache_creation_5m_input_tokens,
        Some(165 + 17_712)
    );
    assert_eq!(breakpoint.cache_creation_1h_input_tokens, Some(14));
    for unchanged in [
        written(3_056, Some(158), Some(2_898)),
        written(3_056, None, None),
        written(0, Some(0), Some(0)),
    ] {
        assert_eq!(complete_writes(unchanged, CacheTtl::FiveMinutes), unchanged);
    }
    let only_hour = complete_writes(written(500, None, Some(200)), CacheTtl::FiveMinutes);
    assert_eq!(only_hour.cache_creation_5m_input_tokens, Some(300));
    assert_eq!(only_hour.cache_creation_1h_input_tokens, Some(200));
}

#[test]
fn every_written_token_is_priced_after_the_normalisation() {
    use crate::pricing::{estimate_usd, model_info};
    use crate::provider::anthropic::complete_writes;
    let info = model_info("claude-haiku-5-5").unwrap();
    let raw = written(11_635, Some(158), Some(2_898));
    let normalised = complete_writes(raw, CacheTtl::FiveMinutes);
    let missing = estimate_usd(info, &normalised, CacheTtl::FiveMinutes)
        - estimate_usd(info, &raw, CacheTtl::FiveMinutes);
    let expected = estimate_usd(
        info,
        &wisp_protocol::Usage {
            cache_creation_input_tokens: 8_579,
            ..wisp_protocol::Usage::default()
        },
        CacheTtl::FiveMinutes,
    );
    assert!(
        (missing - expected).abs() < 1e-12,
        "{missing} != {expected}"
    );
    assert!(missing > 0.0);
}

async fn reported(
    start: Value,
    delta: Value,
    ttl: CacheTtl,
) -> (wisp_protocol::Usage, Vec<crate::provider::Iteration>) {
    let body = [
        http::event("message_start", start),
        http::event("message_delta", delta),
        http::event("message_stop", r#"{"type":"message_stop"}"#),
    ]
    .concat();
    let (base, _) = mock_server(vec![http::sse(&body)]).await;
    let provider = AnthropicProvider::new(&api_key("k"), &base).unwrap();
    let s = session(None, ttl);
    let messages = vec![Message::user_text("go")];
    let rendering = Rendering::default();
    let request = Request {
        session: &s,
        tools: &[],
        deferred: &[],
        by_value: &[],
        server_tools: crate::agent::testing::search_tool("web_search_20250305"),
        messages: &messages,
        previous_response_id: None,
        features: Features::default(),
        options: Default::default(),
        rendering: &rendering,
        compaction: None,
    };
    let items: Vec<_> = provider.stream(&request).collect().await;
    items
        .into_iter()
        .rev()
        .find_map(|i| match i {
            Ok(crate::provider::ProviderEvent::MessageDelta {
                usage, iterations, ..
            }) => Some((usage, iterations)),
            _ => None,
        })
        .unwrap()
}

fn written_by(iterations: &[crate::provider::Iteration]) -> Vec<(Option<u64>, Option<u64>)> {
    iterations
        .iter()
        .map(|i| {
            (
                i.usage.cache_creation_5m_input_tokens,
                i.usage.cache_creation_1h_input_tokens,
            )
        })
        .collect()
}

fn opus_start() -> Value {
    json!({"type": "message_start", "message": {"id": "m1", "model": "claude-opus-5-5", "usage": {
        "input_tokens": 6, "output_tokens": 1, "cache_read_input_tokens": 1_000,
        "cache_creation_input_tokens": 3_000,
        "cache_creation": {"ephemeral_5m_input_tokens": 0, "ephemeral_1h_input_tokens": 3_000}
    }}})
}

fn opus_delta(searches: u64, iterations: Value) -> Value {
    json!({"type": "message_delta", "delta": {"stop_reason": "end_turn"}, "usage": {
        "input_tokens": 12, "output_tokens": 300, "cache_read_input_tokens": 5_000,
        "cache_creation_input_tokens": 8_000,
        "server_tool_use": {"web_search_requests": searches},
        "iterations": iterations
    }})
}

fn iteration(kind: &str, total: u64, breakdown: Value) -> Value {
    let mut usage = json!({
        "type": kind, "input_tokens": 6, "output_tokens": 150,
        "cache_read_input_tokens": 1_000, "cache_creation_input_tokens": total
    });
    if !breakdown.is_null() {
        usage["cache_creation"] = breakdown;
    }
    usage
}

fn partial_iterations(second: &str) -> Value {
    json!([
        iteration(
            "message",
            3_000,
            json!({"ephemeral_1h_input_tokens": 2_000})
        ),
        iteration(second, 5_000, json!({"ephemeral_5m_input_tokens": 1_000})),
    ])
}

#[tokio::test]
async fn a_searching_reply_at_five_minutes_reports_the_whole_write_at_five_minutes() {
    let start = json!({"type": "message_start", "message": {"id": "m1", "model": "claude-haiku-5-5", "usage": {
        "input_tokens": 6, "output_tokens": 1, "cache_read_input_tokens": 0,
        "cache_creation_input_tokens": 3_056,
        "cache_creation": {"ephemeral_5m_input_tokens": 158, "ephemeral_1h_input_tokens": 2_898}
    }}});
    let delta = json!({"type": "message_delta", "delta": {"stop_reason": "end_turn"}, "usage": {
        "input_tokens": 6, "output_tokens": 234, "cache_read_input_tokens": 3_056,
        "cache_creation_input_tokens": 11_635,
        "server_tool_use": {"web_search_requests": 1}
    }});
    let (usage, _) = reported(start, delta, CacheTtl::FiveMinutes).await;
    assert_eq!(usage.cache_creation_input_tokens, 11_635);
    assert_eq!(usage.cache_creation_5m_input_tokens, Some(158 + 8_579));
    assert_eq!(usage.cache_creation_1h_input_tokens, Some(2_898));
    assert_eq!(usage.cache_read_input_tokens, 3_056);
    assert_eq!(usage.web_search_requests, 1);
}

#[tokio::test]
async fn a_searching_stream_without_iterations_counts_the_rest_at_five_minutes() {
    use crate::pricing::{estimate_usd, model_info};
    let start = json!({"type": "message_start", "message": {"id": "m1", "model": "claude-haiku-5-5", "usage": {
        "input_tokens": 3, "output_tokens": 1, "cache_read_input_tokens": 0,
        "cache_creation_input_tokens": 4_309,
        "cache_creation": {"ephemeral_5m_input_tokens": 0, "ephemeral_1h_input_tokens": 4_309}
    }}});
    let delta = json!({"type": "message_delta", "delta": {"stop_reason": "end_turn"}, "usage": {
        "input_tokens": 3, "output_tokens": 180, "cache_read_input_tokens": 4_309,
        "cache_creation_input_tokens": 11_744,
        "server_tool_use": {"web_search_requests": 1}
    }});
    let (usage, iterations) = reported(start, delta, CacheTtl::OneHour).await;
    assert!(iterations.is_empty());
    assert_eq!(usage.cache_creation_input_tokens, 11_744);
    assert_eq!(usage.cache_creation_1h_input_tokens, Some(4_309));
    assert_eq!(usage.cache_creation_5m_input_tokens, Some(7_435));
    let info = model_info("claude-haiku-5-5").unwrap();
    let unwritten = wisp_protocol::Usage {
        cache_creation_input_tokens: 0,
        cache_creation_5m_input_tokens: Some(0),
        cache_creation_1h_input_tokens: Some(0),
        ..usage
    };
    let writes = estimate_usd(info, &usage, CacheTtl::OneHour)
        - estimate_usd(info, &unwritten, CacheTtl::OneHour);
    let input = info.rates_for(&usage).input_per_mtok;
    let expected = (4_309.0 * 2.0 + 7_435.0 * 1.25) * input / 1_000_000.0;
    assert!((writes - expected).abs() < 1e-12, "{writes} != {expected}");
}

#[tokio::test]
async fn a_searching_stream_with_iterations_sums_their_writes_at_the_top() {
    use crate::pricing::{estimate_usd, model_info};
    let wire = [(None, Some(3_000)), (Some(5_000), Some(0))];
    let iterations = json!([
        iteration(
            "message",
            3_000,
            json!({"ephemeral_1h_input_tokens": 3_000})
        ),
        iteration(
            "message",
            5_000,
            json!({"ephemeral_5m_input_tokens": 5_000, "ephemeral_1h_input_tokens": 0})
        ),
    ]);
    let (usage, iterations) =
        reported(opus_start(), opus_delta(1, iterations), CacheTtl::OneHour).await;
    assert_eq!(usage.cache_creation_input_tokens, 8_000);
    assert_eq!(usage.cache_creation_5m_input_tokens, Some(5_000));
    assert_eq!(usage.cache_creation_1h_input_tokens, Some(3_000));
    assert_eq!(written_by(&iterations), wire);
    let info = model_info("claude-opus-5-5").unwrap();
    let priced: f64 = iterations
        .iter()
        .map(|i| estimate_usd(info, &i.usage, CacheTtl::OneHour))
        .sum();
    let as_sent: f64 = iterations
        .iter()
        .zip(wire)
        .map(|(i, (five, hour))| {
            let usage = wisp_protocol::Usage {
                cache_creation_5m_input_tokens: five,
                cache_creation_1h_input_tokens: hour,
                ..i.usage
            };
            estimate_usd(info, &usage, CacheTtl::OneHour)
        })
        .sum();
    assert!((priced - as_sent).abs() < 1e-12, "{priced} != {as_sent}");
}

#[tokio::test]
async fn a_later_iteration_without_a_breakdown_counts_at_five_minutes_after_a_search() {
    let (usage, iterations) = reported(
        opus_start(),
        opus_delta(1, partial_iterations("message")),
        CacheTtl::OneHour,
    )
    .await;
    assert_eq!(
        written_by(&iterations),
        [(Some(0), Some(3_000)), (Some(5_000), Some(0))]
    );
    assert_eq!(usage.cache_creation_5m_input_tokens, Some(5_000));
    assert_eq!(usage.cache_creation_1h_input_tokens, Some(3_000));
}

#[tokio::test]
async fn an_iteration_without_write_counts_counts_at_5m() {
    let iterations = json!([
        iteration(
            "message",
            3_000,
            json!({"ephemeral_1h_input_tokens": 3_000})
        ),
        iteration("message", 5_000, Value::Null),
    ]);
    let (usage, iterations) =
        reported(opus_start(), opus_delta(1, iterations), CacheTtl::OneHour).await;
    assert_eq!(
        written_by(&iterations),
        [(None, Some(3_000)), (Some(5_000), Some(0))]
    );
    assert_eq!(usage.cache_creation_5m_input_tokens, Some(5_000));
    assert_eq!(usage.cache_creation_1h_input_tokens, Some(3_000));
}

#[tokio::test]
async fn a_first_iteration_without_write_counts_leaves_the_top_levels_rest_at_5m() {
    let iterations = json!([
        iteration("message", 3_000, Value::Null),
        iteration(
            "message",
            5_000,
            json!({"ephemeral_5m_input_tokens": 5_000, "ephemeral_1h_input_tokens": 0})
        ),
    ]);
    let (usage, iterations) =
        reported(opus_start(), opus_delta(1, iterations), CacheTtl::OneHour).await;
    assert_eq!(
        written_by(&iterations),
        [(None, None), (Some(5_000), Some(0))]
    );
    assert_eq!(usage.cache_creation_5m_input_tokens, Some(5_000));
    assert_eq!(usage.cache_creation_1h_input_tokens, Some(3_000));
}

#[tokio::test]
async fn a_zero_write_iteration_without_a_breakdown_still_sums_at_the_top() {
    let iterations = json!([
        iteration(
            "message",
            3_000,
            json!({"ephemeral_1h_input_tokens": 3_000})
        ),
        iteration("message", 0, Value::Null),
        iteration(
            "message",
            5_000,
            json!({"ephemeral_5m_input_tokens": 5_000})
        ),
    ]);
    let (usage, iterations) =
        reported(opus_start(), opus_delta(1, iterations), CacheTtl::OneHour).await;
    assert_eq!(
        written_by(&iterations),
        [(None, Some(3_000)), (None, None), (Some(5_000), None)]
    );
    assert_eq!(usage.cache_creation_5m_input_tokens, Some(5_000));
    assert_eq!(usage.cache_creation_1h_input_tokens, Some(3_000));
}

#[tokio::test]
async fn a_fallback_reply_keeps_todays_figures() {
    let (usage, iterations) = reported(
        opus_start(),
        opus_delta(1, partial_iterations("fallback_message")),
        CacheTtl::OneHour,
    )
    .await;
    assert_eq!(
        written_by(&iterations),
        [(Some(0), Some(3_000)), (Some(1_000), Some(4_000))]
    );
    assert_eq!(usage.cache_creation_5m_input_tokens, Some(0));
    assert_eq!(usage.cache_creation_1h_input_tokens, Some(8_000));
}

#[tokio::test]
async fn a_reply_without_server_tools_is_unchanged() {
    let (usage, iterations) = reported(
        opus_start(),
        opus_delta(0, partial_iterations("message")),
        CacheTtl::OneHour,
    )
    .await;
    assert_eq!(
        written_by(&iterations),
        [(Some(0), Some(3_000)), (Some(1_000), Some(4_000))]
    );
    assert_eq!(usage.cache_creation_5m_input_tokens, Some(0));
    assert_eq!(usage.cache_creation_1h_input_tokens, Some(8_000));
}

const UNKNOWN_TOOL: &str = "tools.1: Input tag 'web_search_20250305' found using 'type' does not match any of the expected tags";

const TOO_LONG: &str = "prompt is too long: 300000 tokens > 200000 maximum";

async fn suspect_for(response: String, how: Sent) -> Option<Refused> {
    let error = sent(response, how).await;
    error.refused
}

#[tokio::test]
async fn a_gateway_refusing_the_tool_is_suspected_in_each_qualifying_form() {
    let cases = [
        (
            "a typed 400",
            http::json("400 Bad Request", &invalid(UNKNOWN_TOOL)),
        ),
        (
            "an untyped JSON 400",
            http::json(
                "400 Bad Request",
                &json!({"error": {"message": UNKNOWN_TOOL}}).to_string(),
            ),
        ),
        (
            "a plain-text 400",
            http::response(
                "400 Bad Request",
                &[("content-type", "text/plain")],
                UNKNOWN_TOOL,
            ),
        ),
        (
            "a 422",
            http::json("422 Unprocessable Entity", &invalid(UNKNOWN_TOOL)),
        ),
        (
            "a 200 JSON error object",
            http::json("200 OK", &invalid(UNKNOWN_TOOL)),
        ),
        (
            "an error event before any other",
            http::sse(&http::event("error", invalid(UNKNOWN_TOOL))),
        ),
    ];
    for (name, response) in cases {
        assert_eq!(
            suspect_for(response.clone(), Sent::gateway()).await,
            Some(Refused::SearchSuspect),
            "{name}"
        );
        let own = Sent {
            gateway: false,
            ..Sent::gateway()
        };
        assert_eq!(
            suspect_for(response.clone(), own).await,
            None,
            "{name} on the own origin"
        );
        let without = Sent {
            search: false,
            ..Sent::gateway()
        };
        assert_eq!(
            suspect_for(response, without).await,
            None,
            "{name} without the tool"
        );
    }
}

#[tokio::test]
async fn a_gateway_suspects_nothing_for_other_failures() {
    let start = http::event(
        "message_start",
        r#"{"type":"message_start","message":{"id":"m1","model":"claude-opus-5-5","usage":{"input_tokens":10,"output_tokens":1}}}"#,
    );
    let typed =
        |kind: &str| json!({"type": "error", "error": {"type": kind, "message": "no"}}).to_string();
    let cases = [
        (
            "an overflow",
            http::json(
                "400 Bad Request",
                &invalid("prompt is too long: 300000 tokens > 200000 maximum"),
            ),
        ),
        (
            "a 401",
            http::json("401 Unauthorized", &typed("authentication_error")),
        ),
        (
            "a 403",
            http::json("403 Forbidden", &typed("permission_error")),
        ),
        (
            "a 404",
            http::json("404 Not Found", &typed("not_found_error")),
        ),
        (
            "a 429",
            http::json("429 Too Many Requests", &typed("rate_limit_error")),
        ),
        (
            "a 500",
            http::json("500 Internal Server Error", &typed("api_error")),
        ),
        (
            "a 529",
            http::json("529 Overloaded", &typed("overloaded_error")),
        ),
        (
            "a rate limit typed in a 400",
            http::json("400 Bad Request", &typed("rate_limit_error")),
        ),
        (
            "a 200 JSON error of another type",
            http::json("200 OK", &typed("overloaded_error")),
        ),
        (
            "an in-stream error",
            http::sse(&format!(
                "{start}{}",
                http::event("error", invalid(UNKNOWN_TOOL))
            )),
        ),
        (
            "an overflow in plain text",
            http::response(
                "400 Bad Request",
                &[("content-type", "text/plain")],
                TOO_LONG,
            ),
        ),
        (
            "an overflow in an untyped 422",
            http::json(
                "422 Unprocessable Entity",
                &json!({"error": {"message": TOO_LONG}}).to_string(),
            ),
        ),
        (
            "a 200 JSON not_found_error",
            http::json("200 OK", &typed("not_found_error")),
        ),
        (
            "a first SSE error of another type",
            http::sse(&http::event("error", typed("not_found_error"))),
        ),
        ("a gateway's text chunk", gateway_text(UNKNOWN_TOOL)),
        (
            "a gateway's rate-limit text",
            gateway_text("rate limit exceeded, try again later"),
        ),
        (
            "the organisation's refusal",
            http::json("400 Bad Request", &invalid(OFF)),
        ),
    ];
    for (name, response) in cases {
        let refused = suspect_for(response, Sent::gateway()).await;
        assert_ne!(refused, Some(Refused::SearchSuspect), "{name}");
    }
    let rendered = Sent {
        history: true,
        rendered: true,
        ..Sent::gateway()
    };
    let refused = suspect_for(
        http::json("400 Bad Request", &invalid(UNKNOWN_TOOL)),
        rendered,
    )
    .await;
    assert_ne!(refused, Some(Refused::SearchSuspect), "a rendered block");
    assert_eq!(
        suspect_for(
            http::json("400 Bad Request", &invalid(OFF)),
            Sent::gateway()
        )
        .await,
        Some(Refused::NativeSearch)
    );
}

#[tokio::test]
async fn a_network_failure_is_never_suspected() {
    let (_socket, addr) = refusing_addr();
    let provider = AnthropicProvider::new(&api_key("k"), &format!("http://{addr}")).unwrap();
    let s = session(None, CacheTtl::OneHour);
    let messages = vec![Message::user_text("go")];
    let request = Request {
        session: &s,
        tools: &[],
        deferred: &[],
        by_value: &[],
        server_tools: crate::agent::testing::search_tool("web_search_20250305"),
        messages: &messages,
        previous_response_id: None,
        features: Features::default(),
        options: Default::default(),
        rendering: &Rendering::default(),
        compaction: None,
    };
    let items: Vec<_> = provider.stream(&request).collect().await;
    let Some(Err(error)) = items.last() else {
        panic!("{items:?}");
    };
    assert_eq!(error.refused, None, "{error:?}");
}

#[tokio::test]
async fn the_suspicion_comes_before_an_image_refusal() {
    let image = Sent {
        provisional: true,
        ..Sent::gateway()
    };
    let response = http::json("400 Bad Request", &invalid("bad image"));
    assert_eq!(
        suspect_for(response.clone(), image).await,
        Some(Refused::SearchSuspect)
    );
    let resent = Sent {
        search: false,
        ..image
    };
    assert_eq!(
        suspect_for(response, resent).await,
        Some(Refused::Attachment)
    );
}

const TAMPERED: &str = "messages.1.content.0: Invalid `signature` in `thinking` block";
const REDACTED: &str = "messages.1.content.0: Invalid `data` in `redacted_thinking` block";

#[tokio::test]
async fn a_refused_signature_or_redacted_block_is_a_binding_refusal_that_starts_no_probe_or_withdrawal()
 {
    for message in [TAMPERED, REDACTED] {
        for provisional in [false, true] {
            let how = Sent {
                provisional,
                ..Sent::gateway()
            };
            let error = sent(http::json("400 Bad Request", &invalid(message)), how).await;
            assert_eq!(error.kind, ErrorKind::ThinkingBinding, "{message}");
            assert_eq!(error.refused, None, "{message}, {provisional}");
        }
    }
}

#[test]
fn a_rejected_feature_or_an_earlier_match_is_never_suspected() {
    use crate::agent::testing::classified;
    use crate::provider::Feature;
    use crate::provider::anthropic::{Carries, suspect_search};
    use wisp_protocol::{FailurePhase, RetryClass};
    let carries = Carries {
        added_tools: false,
        rendered: false,
        search: true,
        native: false,
        gateway: true,
        provisional: false,
        media: false,
    };
    let fresh = || {
        classified(
            ErrorKind::InvalidRequest,
            UNKNOWN_TOOL,
            FailurePhase::Response,
            RetryClass::Never,
        )
    };
    assert_eq!(
        suspect_search(fresh(), carries, true).refused,
        Some(Refused::SearchSuspect)
    );
    assert_eq!(suspect_search(fresh(), carries, false).refused, None);
    let rejected = fresh().rejecting(vec![Feature::PerMessageEffort]);
    assert_eq!(suspect_search(rejected, carries, true).refused, None);
    let earlier = fresh().refusing(Refused::CrossAccount);
    assert_eq!(
        suspect_search(earlier, carries, true).refused,
        Some(Refused::CrossAccount)
    );
    for flipped in [
        Carries {
            gateway: false,
            ..carries
        },
        Carries {
            search: false,
            ..carries
        },
        Carries {
            rendered: true,
            ..carries
        },
    ] {
        assert_eq!(
            suspect_search(fresh(), flipped, true).refused,
            None,
            "{flipped:?}"
        );
    }
}
