use std::collections::BTreeMap;
use std::sync::Mutex;

use futures::StreamExt;
use serde_json::json;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;
use wisp_protocol::{Effort, Message, MessageTime, RetryClass, SessionConfig};

use super::*;
use crate::agent::testing::{body_of, fallback_marker, text_of, turn};
use crate::provider::sse::{after_200, billed_in_stream, interrupted, send_failure};
use crate::provider::{AssembledMessage, Assembler, Features, RequestOptions};
use crate::test_guard::{assistant, pick};
use crate::test_guard::{holding_server, http, refusing_addr, until_turn_end, within};

mod classification;
mod compaction;
mod deferred;
mod forms;
mod inline;
mod list;
mod marker;
mod media;
mod native;
mod prefix;
mod rendering;
mod retries;
mod search;
mod sections;
mod subscription;
mod switches;
mod threads;
mod withdraw;

const THINKING_AND_TEXT: &str = r#"event: message_start
data: {"type":"message_start","message":{"id":"msg_01XFDUDYJgAACzvnptvVoYEL","type":"message","role":"assistant","model":"claude-opus-5-5","content":[],"stop_reason":null,"stop_sequence":null,"usage":{"input_tokens":14,"cache_creation_input_tokens":620,"cache_read_input_tokens":1800,"cache_creation":{"ephemeral_5m_input_tokens":0,"ephemeral_1h_input_tokens":620},"output_tokens":1}}}

event: content_block_start
data: {"type":"content_block_start","index":0,"content_block":{"type":"thinking","thinking":"","signature":""}}

event: ping
data: {"type": "ping"}

event: content_block_delta
data: {"type":"content_block_delta","index":0,"delta":{"type":"thinking_delta","thinking":"The user wants a haiku"}}

event: content_block_delta
data: {"type":"content_block_delta","index":0,"delta":{"type":"thinking_delta","thinking":" about autumn."}}

event: content_block_delta
data: {"type":"content_block_delta","index":0,"delta":{"type":"signature_delta","signature":"EqQBCkYIBxgCKkCq7n+Z/xw=="}}

event: content_block_stop
data: {"type":"content_block_stop","index":0}

event: content_block_start
data: {"type":"content_block_start","index":1,"content_block":{"type":"text","text":""}}

event: content_block_delta
data: {"type":"content_block_delta","index":1,"delta":{"type":"text_delta","text":"Crisp leaves let go—\n"}}

event: content_block_delta
data: {"type":"content_block_delta","index":1,"delta":{"type":"text_delta","text":"the maple keeps its secrets."}}

event: content_block_stop
data: {"type":"content_block_stop","index":1}

event: message_delta
data: {"type":"message_delta","delta":{"stop_reason":"end_turn","stop_sequence":null},"usage":{"output_tokens":87}}

event: message_stop
data: {"type":"message_stop"}

"#;

const MID_STREAM_ERROR: &str = r#"event: message_start
data: {"type":"message_start","message":{"id":"msg_02","type":"message","role":"assistant","model":"claude-opus-5-5","content":[],"stop_reason":null,"usage":{"input_tokens":20,"output_tokens":1}}}

event: content_block_start
data: {"type":"content_block_start","index":0,"content_block":{"type":"text","text":""}}

event: content_block_delta
data: {"type":"content_block_delta","index":0,"delta":{"type":"text_delta","text":"Partial"}}

event: error
data: {"type":"error","error":{"type":"overloaded_error","message":"Overloaded"}}

"#;

const MAX_TOKENS: &str = r#"event: message_start
data: {"type":"message_start","message":{"id":"msg_03","model":"claude-opus-5-5","usage":{"input_tokens":5,"output_tokens":1}}}

event: content_block_start
data: {"type":"content_block_start","index":0,"content_block":{"type":"text","text":""}}

event: content_block_delta
data: {"type":"content_block_delta","index":0,"delta":{"type":"text_delta","text":"Once upon a"}}

event: content_block_stop
data: {"type":"content_block_stop","index":0}

event: message_delta
data: {"type":"message_delta","delta":{"stop_reason":"max_tokens","stop_sequence":null,"stop_details":{"ignored":true}},"usage":{"output_tokens":64000}}

event: message_stop
data: {"type":"message_stop"}

"#;

const REFUSAL: &str = r#"event: message_start
data: {"type":"message_start","message":{"id":"msg_04","model":"claude-opus-5-5","content":[],"usage":{"input_tokens":40,"output_tokens":0}}}

event: message_delta
data: {"type":"message_delta","delta":{"stop_reason":"refusal","stop_sequence":null,"stop_details":{"category":"bio","explanation":null}},"usage":{"output_tokens":0}}

event: message_stop
data: {"type":"message_stop"}

"#;

const UNKNOWN_TYPES: &str = r#"event: message_start
data: {"type":"message_start","message":{"id":"msg_05","model":"claude-opus-5","usage":{"input_tokens":9,"output_tokens":1}}}

event: content_block_start
data: {"type":"content_block_start","index":0,"content_block":{"type":"fallback","from":{"model":"claude-opus-5-5"},"to":{"model":"claude-opus-5"}}}

event: content_block_stop
data: {"type":"content_block_stop","index":0}

event: brand_new_event
data: {"type":"brand_new_event","whatever":[1,2,3]}

event: content_block_start
data: {"type":"content_block_start","index":1,"content_block":{"type":"hologram","payload":{"x":1}}}

event: content_block_delta
data: {"type":"content_block_delta","index":1,"delta":{"type":"hologram_delta","bits":"..."}}

event: content_block_stop
data: {"type":"content_block_stop","index":1}

event: content_block_start
data: {"type":"content_block_start","index":2,"content_block":{"type":"text","text":"ok"}}

event: content_block_stop
data: {"type":"content_block_stop","index":2}

event: message_delta
data: {"type":"message_delta","delta":{"stop_reason":"end_turn"},"usage":{"output_tokens":3}}

event: message_stop
data: {"type":"message_stop"}

"#;

const CUT_OFF: &str = r#"event: message_start
data: {"type":"message_start","message":{"id":"msg_06","model":"claude-opus-5-5","usage":{"input_tokens":9,"output_tokens":1}}}

event: content_block_start
data: {"type":"content_block_start","index":0,"content_block":{"type":"text","text":""}}

event: content_block_delta
data: {"type":"content_block_delta","index":0,"delta":{"type":"text_d"#;

fn run_fixture(sse: &str) -> (Vec<ProviderEvent>, Result<AssembledMessage, ProviderError>) {
    let mut decoder = StreamDecoder::default();
    let mut asm = Assembler::default();
    let mut events = Vec::new();
    for chunk in sse.as_bytes().chunks(7) {
        for item in decoder.push(chunk) {
            match item {
                Ok(ev) => {
                    events.push(ev.clone());
                    if let Err(e) = asm.apply(ev) {
                        return (events, Err(e));
                    }
                }
                Err(e) => return (events, Err(e)),
            }
        }
    }
    if let Some(e) = decoder.finish() {
        return (events, Err(e));
    }
    (events, asm.finish())
}

#[test]
fn assembles_thinking_signature_and_text() {
    let (_, msg) = run_fixture(THINKING_AND_TEXT);
    let msg = msg.unwrap();
    assert_eq!(msg.id.as_deref(), Some("msg_01XFDUDYJgAACzvnptvVoYEL"));
    assert_eq!(msg.stop_reason, StopReason::EndTurn);
    assert_eq!(
        msg.content,
        vec![
            ContentBlock::Thinking {
                thinking: "The user wants a haiku about autumn.".into(),
                signature: "EqQBCkYIBxgCKkCq7n+Z/xw==".into(),
            },
            ContentBlock::Text {
                text: "Crisp leaves let go—\nthe maple keeps its secrets.".into()
            },
        ]
    );
    assert_eq!(
        msg.usage,
        Usage {
            input_tokens: 14,
            output_tokens: 87,
            cache_read_input_tokens: 1800,
            cache_creation_input_tokens: 620,
            cache_creation_5m_input_tokens: Some(0),
            cache_creation_1h_input_tokens: Some(620),
            web_search_requests: 0,
            ..Usage::default()
        }
    );
}

#[test]
fn mid_stream_error_is_typed() {
    let (events, result) = run_fixture(MID_STREAM_ERROR);
    let err = result.unwrap_err();
    assert_eq!(err.kind, ErrorKind::Overloaded);
    assert!(err.message.contains("Overloaded"));
    assert!(
        events
            .iter()
            .any(|e| matches!(e, ProviderEvent::BlockDelta { .. }))
    );
}

#[test]
fn max_tokens_ignores_stop_details() {
    let msg = run_fixture(MAX_TOKENS).1.unwrap();
    assert_eq!(msg.stop_reason, StopReason::MaxTokens);
    assert_eq!(msg.stop_details, None);
    assert_eq!(msg.usage.output_tokens, 64000);
}

#[test]
fn refusal_keeps_stop_details() {
    let msg = run_fixture(REFUSAL).1.unwrap();
    assert_eq!(msg.stop_reason, StopReason::Refusal);
    assert!(msg.content.is_empty());
    assert_eq!(
        msg.stop_details,
        Some(json!({"category": "bio", "explanation": null}))
    );
}

#[test]
fn unknown_events_and_blocks_are_kept_opaque() {
    let (events, msg) = run_fixture(UNKNOWN_TYPES);
    let msg = msg.unwrap();
    assert!(events.contains(&ProviderEvent::Unknown {
        provider: "anthropic".into(),
        raw: json!({"type": "brand_new_event", "whatever": [1, 2, 3]}),
    }));
    assert!(events.contains(&ProviderEvent::Fallback {
        from: "claude-opus-5-5".into(),
        to: "claude-opus-5".into(),
    }));
    assert_eq!(msg.content.len(), 3);
    assert_eq!(opaque_type(&msg.content[0]), Some("fallback"));
    assert_eq!(
        msg.content[1],
        ContentBlock::Opaque {
            provider: "anthropic".into(),
            raw: json!({"type": "hologram", "payload": {"x": 1}}),
            deltas: vec![json!({"type": "hologram_delta", "bits": "..."})],
        }
    );
    assert_eq!(msg.content[2], ContentBlock::Text { text: "ok".into() });
    let echoed = serde_json::to_value(assistant_blocks(
        &msg.content,
        0,
        &mut Default::default(),
        &crate::provider::UNRENDERED,
    ))
    .unwrap();
    assert_eq!(echoed[1], json!({"type": "hologram", "payload": {"x": 1}}));
}

#[test]
fn unknown_deltas_drop_nested_identity_while_resent_content_stays_exact() {
    let sse = r#"event: message_start
data: {"type":"message_start","message":{"id":"m","model":"claude-opus-5-5","usage":{"input_tokens":1,"output_tokens":1}}}

event: content_block_start
data: {"type":"content_block_start","index":0,"content_block":{"type":"hologram","payload":{"user_id":"stored"}}}

event: content_block_delta
data: {"type":"content_block_delta","index":0,"delta":{"type":"citations_delta","citation":{"type":"char_location","user":"stored"}}}

event: content_block_delta
data: {"type":"content_block_delta","index":0,"delta":{"type":"hologram_delta","bits":[{"safety_identifier":"private","user_id":"private","account_id":"private","email":"me@example.com","plan_type":"pro","credits":{"balance":"CREDIT_CANARY"},"promo":"PROMO_CANARY","kept":7}]}}

event: content_block_stop
data: {"type":"content_block_stop","index":0}

event: message_delta
data: {"type":"message_delta","delta":{"stop_reason":"end_turn"},"usage":{"output_tokens":9}}

event: message_stop
data: {"type":"message_stop"}

"#;
    let msg = run_fixture(sse).1.unwrap();
    let raw = json!({
        "type": "hologram",
        "payload": {"user_id": "stored"},
        "citations": [{"type": "char_location", "user": "stored"}],
    });
    assert_eq!(
        msg.content,
        vec![ContentBlock::Opaque {
            provider: "anthropic".into(),
            raw: raw.clone(),
            deltas: vec![json!({"type": "hologram_delta", "bits": [{"kept": 7}]})],
        }]
    );
    let echoed = serde_json::to_value(assistant_blocks(
        &msg.content,
        0,
        &mut Default::default(),
        &crate::provider::UNRENDERED,
    ))
    .unwrap();
    assert_eq!(echoed, json!([raw]));
}

#[test]
fn citations_and_stray_signatures_are_kept_and_echoed() {
    let sse = r#"event: message_start
data: {"type":"message_start","message":{"id":"m","model":"claude-opus-5-5","usage":{"input_tokens":1,"output_tokens":1}}}

event: content_block_start
data: {"type":"content_block_start","index":0,"content_block":{"type":"text","text":""}}

event: content_block_delta
data: {"type":"content_block_delta","index":0,"delta":{"type":"text_delta","text":"Paris "}}

event: content_block_delta
data: {"type":"content_block_delta","index":0,"delta":{"type":"citations_delta","citation":{"type":"char_location","cited_text":"Paris"}}}

event: content_block_delta
data: {"type":"content_block_delta","index":0,"delta":{"type":"text_delta","text":"is the capital."}}

event: content_block_stop
data: {"type":"content_block_stop","index":0}

event: content_block_start
data: {"type":"content_block_start","index":1,"content_block":{"type":"redacted_thinking","data":"blob"}}

event: content_block_delta
data: {"type":"content_block_delta","index":1,"delta":{"type":"signature_delta","signature":"sig=="}}

event: content_block_stop
data: {"type":"content_block_stop","index":1}

event: message_delta
data: {"type":"message_delta","delta":{"stop_reason":"end_turn"},"usage":{"output_tokens":9}}

event: message_stop
data: {"type":"message_stop"}

"#;
    let msg = run_fixture(sse).1.unwrap();
    let text = json!({
        "type": "text",
        "text": "Paris is the capital.",
        "citations": [{"type": "char_location", "cited_text": "Paris"}],
    });
    let redacted = json!({"type": "redacted_thinking", "data": "blob", "signature": "sig=="});
    assert_eq!(
        msg.content,
        vec![
            ContentBlock::opaque("anthropic", text.clone()),
            ContentBlock::opaque("anthropic", redacted.clone()),
        ]
    );
    let json = serde_json::to_string(&msg.content).unwrap();
    let back: Vec<ContentBlock> = serde_json::from_str(&json).unwrap();
    assert_eq!(back, msg.content);
    assert_eq!(
        serde_json::to_value(assistant_blocks(
            &back,
            0,
            &mut Default::default(),
            &crate::provider::UNRENDERED
        ))
        .unwrap(),
        json!([text, redacted])
    );
}

#[test]
fn fallback_usage_iterations_are_attributed_to_their_models() {
    let sse = r#"event: message_start
data: {"type":"message_start","message":{"id":"m","model":"claude-opus-5-5","usage":{"input_tokens":10,"output_tokens":1}}}

event: content_block_start
data: {"type":"content_block_start","index":0,"content_block":{"type":"text","text":"Sure"}}

event: content_block_stop
data: {"type":"content_block_stop","index":0}

event: content_block_start
data: {"type":"content_block_start","index":1,"content_block":{"type":"fallback","from":{"model":"claude-opus-5-5"},"to":{"model":"claude-opus-4-8"}}}

event: content_block_stop
data: {"type":"content_block_stop","index":1}

event: content_block_start
data: {"type":"content_block_start","index":2,"content_block":{"type":"text","text":", here."}}

event: content_block_stop
data: {"type":"content_block_stop","index":2}

event: message_delta
data: {"type":"message_delta","delta":{"stop_reason":"end_turn"},"usage":{"output_tokens":30,"iterations":[{"type":"message","input_tokens":10,"output_tokens":5},{"type":"fallback_message","input_tokens":12,"output_tokens":30}]}}

event: message_stop
data: {"type":"message_stop"}

"#;
    let mut decoder = StreamDecoder::new("claude-opus-5-5", Features::default(), None);
    let mut asm = Assembler::default();
    for item in decoder.push(sse.as_bytes()) {
        asm.apply(item.unwrap()).unwrap();
    }
    let msg = asm.finish().unwrap();
    assert_eq!(msg.model, "claude-opus-4-8");
    let attributed: Vec<(&str, &str, u64)> = msg
        .iterations
        .iter()
        .map(|i| (i.kind.as_str(), i.model.as_str(), i.usage.output_tokens))
        .collect();
    assert_eq!(
        attributed,
        [
            ("message", "claude-opus-5-5", 5),
            ("fallback_message", "claude-opus-4-8", 30)
        ]
    );
}

const GATEWAY_ERROR: &str = "data: {\"id\":\"x\",\"object\":\"chat.completion.chunk\",\"choices\":[{\"index\":0,\"delta\":{\"role\":\"assistant\",\"content\":\"Upstream quota exceeded \"}}]}\n\ndata: {\"id\":\"x\",\"object\":\"chat.completion.chunk\",\"choices\":[{\"index\":0,\"delta\":{\"content\":\"for this key.\"},\"finish_reason\":\"stop\"}]}\n\ndata: [DONE]\n\n";

#[test]
fn non_anthropic_stream_is_a_typed_error_with_the_gateway_text() {
    let (events, result) = run_fixture(GATEWAY_ERROR);
    assert!(events.is_empty());
    let err = result.unwrap_err();
    assert_eq!(err.kind, ErrorKind::InvalidResponse);
    assert!(
        err.message
            .contains("Upstream quota exceeded for this key."),
        "{}",
        err.message
    );
    assert!(!err.message.contains("missing field"));

    let (_, result) = run_fixture(&http::event(
        "message",
        r#"{"error":{"message":"bad gateway key"}}"#,
    ));
    let err = result.unwrap_err();
    assert_eq!(err.kind, ErrorKind::InvalidResponse);
    assert!(err.message.contains("bad gateway key"), "{}", err.message);
}

#[test]
fn cut_off_stream_is_an_error() {
    let (_, result) = run_fixture(CUT_OFF);
    let err = result.unwrap_err();
    assert_eq!(err.kind, ErrorKind::Network);
    assert!(err.message.contains("cut off"), "{}", err.message);
}

#[test]
fn tool_use_input_is_assembled() {
    let sse = r#"event: message_start
data: {"type":"message_start","message":{"id":"m","model":"claude-opus-5-5","usage":{"input_tokens":1,"output_tokens":1}}}

event: content_block_start
data: {"type":"content_block_start","index":0,"content_block":{"type":"tool_use","id":"toolu_1","name":"get_weather","input":{}}}

event: content_block_delta
data: {"type":"content_block_delta","index":0,"delta":{"type":"input_json_delta","partial_json":"{\"location\": \"Pa"}}

event: content_block_delta
data: {"type":"content_block_delta","index":0,"delta":{"type":"input_json_delta","partial_json":"ris\"}"}}

event: content_block_stop
data: {"type":"content_block_stop","index":0}

event: message_delta
data: {"type":"message_delta","delta":{"stop_reason":"tool_use"},"usage":{"output_tokens":20}}

event: message_stop
data: {"type":"message_stop"}

"#;
    let msg = run_fixture(sse).1.unwrap();
    assert_eq!(msg.stop_reason, StopReason::ToolUse);
    assert_eq!(
        msg.content,
        vec![ContentBlock::ToolUse {
            id: "toolu_1".into(),
            name: "get_weather".into(),
            input: json!({"location": "Paris"}),
        }]
    );
}

fn session(system: Option<&str>, ttl: CacheTtl) -> SessionConfig {
    SessionConfig {
        model: "claude-opus-5-5".into(),
        system: system.map(str::to_owned),
        effort: Effort::High,
        cache_ttl: ttl,
        max_tokens: 64000,
        message_time: MessageTime::Local,
        instructions: None,
        context_note: true,
    }
}

fn body(session: &SessionConfig, messages: &[Message], prev: Option<&str>) -> String {
    body_with(RequestOptions::default(), session, messages, prev)
}

fn plain_request<'a>(
    options: RequestOptions,
    session: &'a SessionConfig,
    messages: &'a [Message],
    prev: Option<&'a str>,
) -> Request<'a> {
    Request {
        session,
        tools: &[],
        deferred: &[],
        by_value: &[],
        server_tools: &[],
        messages,
        previous_response_id: prev,
        features: Features::default(),
        options,
        rendering: &crate::provider::UNRENDERED,
        compaction: None,
    }
}

fn body_with(
    options: RequestOptions,
    session: &SessionConfig,
    messages: &[Message],
    prev: Option<&str>,
) -> String {
    let request = plain_request(options, session, messages, prev);
    text_of(&request)
}

fn header_with(options: RequestOptions, messages: &[Message]) -> String {
    let session = session(None, CacheTtl::OneHour);
    beta_header(&plain_request(options, &session, messages, None))
}

fn conversation() -> Vec<Message> {
    let first = run_fixture(THINKING_AND_TEXT).1.unwrap().into_message();
    vec![
        Message::user_text("Write a haiku about autumn."),
        first,
        Message::user_text("Another, about winter."),
        assistant(vec![
            ContentBlock::Thinking {
                thinking: "Winter: snow, silence.".into(),
                signature: "Sig/2+abc==".into(),
            },
            ContentBlock::RedactedThinking {
                data: "EncryptedBlob==".into(),
            },
            ContentBlock::Text {
                text: "Snow hushes the field.".into(),
            },
        ]),
    ]
}

#[test]
fn request_sets_model_effort_thinking_and_caching() {
    let s = session(Some("You are Wisp."), CacheTtl::OneHour);
    let v: Value = serde_json::from_str(&body(&s, &conversation(), None)).unwrap();
    assert_eq!(v["model"], "claude-opus-5-5");
    assert_eq!(v["max_tokens"], 64000);
    assert_eq!(v["stream"], true);
    assert_eq!(
        v["thinking"],
        json!({"type": "adaptive", "display": "summarized"})
    );
    assert_eq!(v["output_config"], json!({"effort": "high"}));
    assert_eq!(
        v["cache_control"],
        json!({"type": "ephemeral", "ttl": "1h"})
    );
    assert_eq!(
        v["system"],
        json!([{"type": "text", "text": "You are Wisp.", "cache_control": {"type": "ephemeral", "ttl": "1h"}}])
    );
    assert_eq!(v["fallbacks"], "default");
    for absent in ["temperature", "top_p", "top_k", "diagnostics"] {
        assert!(v.get(absent).is_none(), "{absent} must not be sent");
    }
    assert_eq!(
        header_with(RequestOptions::default(), &[]),
        "server-side-fallback-2026-07-01"
    );
}

#[test]
fn five_minute_session_keeps_the_system_marker_at_one_hour() {
    let s = session(Some("sys"), CacheTtl::FiveMinutes);
    let v: Value = serde_json::from_str(&body(&s, &conversation(), None)).unwrap();
    assert_eq!(v["cache_control"], json!({"type": "ephemeral"}));
    assert_eq!(
        v["system"][0]["cache_control"],
        json!({"type": "ephemeral", "ttl": "1h"})
    );
}

#[test]
fn thinking_blocks_round_trip_unchanged() {
    let s = session(None, CacheTtl::OneHour);
    let v: Value = serde_json::from_str(&body(&s, &conversation(), None)).unwrap();
    assert_eq!(
        v["messages"][1]["content"],
        json!([
            {"type": "thinking", "thinking": "The user wants a haiku about autumn.", "signature": "EqQBCkYIBxgCKkCq7n+Z/xw=="},
            {"type": "text", "text": "Crisp leaves let go—\nthe maple keeps its secrets."},
        ])
    );
    assert_eq!(
        v["messages"][3]["content"],
        json!([
            {"type": "thinking", "thinking": "Winter: snow, silence.", "signature": "Sig/2+abc=="},
            {"type": "redacted_thinking", "data": "EncryptedBlob=="},
            {"type": "text", "text": "Snow hushes the field."},
        ])
    );
}

#[test]
fn empty_system_prompt_has_no_breakpoint() {
    for system in [None, Some("")] {
        let s = session(system, CacheTtl::OneHour);
        let v: Value = serde_json::from_str(&body(&s, &conversation(), None)).unwrap();
        assert!(v.get("system").is_none(), "{system:?}");
    }
}

#[test]
fn no_explicit_markers_on_message_blocks() {
    let s = session(Some("sys"), CacheTtl::OneHour);
    let v: Value = serde_json::from_str(&body(&s, &conversation(), None)).unwrap();
    let rendered = serde_json::to_string(&v["messages"]).unwrap();
    assert!(!rendered.contains("cache_control"));
}

#[test]
fn blank_text_blocks_are_never_echoed() {
    for text in ["", " \n"] {
        let content = vec![ContentBlock::Text { text: text.into() }];
        assert!(
            echoed_blocks(&content, &Default::default()).is_empty(),
            "{text:?}"
        );
    }
}

fn strip_cache_control(body: &str) -> String {
    let mut v: Value = serde_json::from_str(body).unwrap();
    fn strip(v: &mut Value) {
        match v {
            Value::Object(map) => {
                map.remove("cache_control");
                map.values_mut().for_each(strip);
            }
            Value::Array(items) => items.iter_mut().for_each(strip),
            _ => {}
        }
    }
    strip(&mut v);
    serde_json::to_string(&v).unwrap()
}

fn split_body(body: &str) -> (&str, &str) {
    let m = body.find(r#""messages":"#).unwrap();
    let messages = &body[m + r#""messages":"#.len()..body.len() - 1];
    let system = body[..m]
        .find(r#""system":"#)
        .map(|s| &body[s + r#""system":"#.len()..m - 1])
        .unwrap_or("");
    (system, messages)
}

fn stamped(minute: u32, text: &str) -> Message {
    let stamp = format!("Sent: Wednesday 2026-10-21 23:{minute:02}:00 UTC-04:00\n");
    Message::user_stamped(Some(&stamp), text)
}

#[test]
fn prompt_prefix_is_stable_across_turns() {
    let options = RequestOptions {
        refusal_fallback: true,
        debug_cache: true,
        message_threads: true,
    };
    let s = session(Some("You are Wisp."), CacheTtl::OneHour);
    let mut history = conversation();
    history[0] = stamped(56, "Write a haiku about autumn.");
    history[2] = stamped(57, "Another, about winter.");
    history.push(stamped(58, "And spring?"));
    let turns = [(1, "msg_a"), (3, "msg_b"), (5, "msg_c")];
    let raws: Vec<String> = turns
        .iter()
        .map(|(n, prev)| body_with(options, &s, &history[..*n], Some(prev)))
        .collect();

    for pair in raws.windows(2) {
        let (sys_n, msgs_n) = split_body(&pair[0]);
        let (sys_n1, msgs_n1) = split_body(&pair[1]);
        assert_eq!(sys_n, sys_n1);
        let open = msgs_n.strip_suffix(']').unwrap();
        assert!(msgs_n1.starts_with(open), "\n{msgs_n}\n{msgs_n1}");

        let (v_n, v_n1): (Value, Value) = (
            serde_json::from_str(&strip_cache_control(&pair[0])).unwrap(),
            serde_json::from_str(&strip_cache_control(&pair[1])).unwrap(),
        );
        assert_eq!(v_n["system"].to_string(), v_n1["system"].to_string());
        let (msgs_n, msgs_n1) = (v_n["messages"].to_string(), v_n1["messages"].to_string());
        assert!(msgs_n1.starts_with(msgs_n.strip_suffix(']').unwrap()));
    }

    let threaded = |n: usize, prev, thread| {
        let logical = plain_request(options, &s, &history[..n], Some(prev));
        let features = Features {
            thread,
            ..logical.features
        };
        Request {
            features,
            ..logical
        }
    };
    for (n, prev, raw) in [(3, "msg_b", &raws[1]), (5, "msg_c", &raws[2])] {
        let continued = threaded(n, prev, Some(Thread::Continue { held: n - 1 }));
        let body: Value = body_of(&continued);
        let full: Value = serde_json::from_str(raw).unwrap();
        let tail = &full["messages"].as_array().unwrap()[n - 1..];
        assert_eq!(body["messages"].as_array().unwrap()[..], *tail);
        let logical = threaded(n, prev, None);
        for request in [threaded(n, prev, Some(Thread::Create)), continued] {
            assert_eq!(sent_prefix(&request), sent_prefix(&logical));
            assert_eq!(prefix_bytes(&request), prefix_bytes(&logical));
            assert_eq!(wire_print(&request), wire_print(&logical));
        }
    }

    let last: Value = serde_json::from_str(&raws[2]).unwrap();
    let users: Vec<&Value> = last["messages"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|m| m["role"] == "user")
        .collect();
    assert_eq!(users.len(), 3);
    for (user, (minute, text)) in users.iter().zip([
        (56, "Write a haiku about autumn."),
        (57, "Another, about winter."),
        (58, "And spring?"),
    ]) {
        assert_eq!(
            user["content"],
            json!([
                {"type": "text", "text": format!("Sent: Wednesday 2026-10-21 23:{minute:02}:00 UTC-04:00\n")},
                {"type": "text", "text": text},
            ])
        );
    }

    assert_eq!(raws[2], body_with(options, &s, &history, Some("msg_c")));

    let capped = SessionConfig {
        max_tokens: s.max_tokens / 2,
        ..s.clone()
    };
    for ((n, prev), raw) in turns.into_iter().zip(&raws) {
        let lowered = body_with(options, &capped, &history[..n], Some(prev));
        assert_eq!(split_body(&lowered), split_body(raw));
        let (mut low, full): (Value, Value) = (
            serde_json::from_str(&lowered).unwrap(),
            serde_json::from_str(raw).unwrap(),
        );
        assert_eq!(low["max_tokens"], capped.max_tokens);
        low["max_tokens"] = full["max_tokens"].clone();
        assert_eq!(low, full);
    }

    let one_token = SessionConfig {
        max_tokens: 1,
        ..s.clone()
    };
    let refresh_options = RequestOptions {
        refusal_fallback: false,
        ..options
    };
    let refresh = body_with(refresh_options, &one_token, &history[..3], Some("msg_c"));
    let refreshing = plain_request(refresh_options, &one_token, &history[..3], Some("msg_c"));
    let continued = threaded(3, "msg_b", Some(Thread::Continue { held: 2 }));
    assert_eq!(sent_prefix(&refreshing), sent_prefix(&continued));
    let (anchor, next) = (&raws[1], &raws[2]);
    let (sys_a, msgs_a) = split_body(anchor);
    let (sys_r, msgs_r) = split_body(&refresh);
    let (sys_n, msgs_n) = split_body(next);
    assert_eq!(sys_r, sys_a);
    assert_eq!(sys_r, sys_n);
    assert_eq!(msgs_r, msgs_a, "a refresh resends the anchor's messages");
    assert!(msgs_n.starts_with(msgs_r.strip_suffix(']').unwrap()));
    let stripped: Vec<Value> = [anchor.as_str(), refresh.as_str(), next.as_str()]
        .iter()
        .map(|b| serde_json::from_str(&strip_cache_control(b)).unwrap())
        .collect();
    assert_eq!(stripped[1]["system"], stripped[0]["system"]);
    assert_eq!(stripped[1]["system"], stripped[2]["system"]);
    assert_eq!(stripped[1]["messages"], stripped[0]["messages"]);
    let refreshed = stripped[1]["messages"].to_string();
    assert!(
        stripped[2]["messages"]
            .to_string()
            .starts_with(refreshed.strip_suffix(']').unwrap())
    );
    let mut bare: Value = serde_json::from_str(&refresh).unwrap();
    let mut sent: Value = serde_json::from_str(anchor).unwrap();
    for body in [&mut bare, &mut sent] {
        let map = body.as_object_mut().unwrap();
        map.remove("max_tokens");
        map.remove("fallbacks");
        map.remove("diagnostics");
    }
    assert_eq!(bare, sent);
}

fn searched_reply(id: &str, query: &str) -> Message {
    Message {
        role: Role::Assistant,
        content: vec![
            ContentBlock::opaque(
                "anthropic",
                json!({"type": "server_tool_use", "id": id, "name": "web_search", "input": {"query": query}}),
            ),
            ContentBlock::opaque(
                "anthropic",
                json!({"type": "web_search_tool_result", "tool_use_id": id, "content": [
                    {"type": "web_search_result", "title": query, "url": format!("https://example.com/{query}"), "encrypted_content": "Eq1"}
                ]}),
            ),
            ContentBlock::Text {
                text: format!("Found {query}."),
            },
        ],
    }
}

#[test]
fn a_boundary_rendering_keeps_the_prefix_stable_across_turns() {
    let s = session(Some("You are Wisp."), CacheTtl::OneHour);
    let history = [
        stamped(56, "Look up rg."),
        searched_reply("srvtoolu_1", "rg"),
        stamped(57, "And fd?"),
        searched_reply("srvtoolu_2", "fd"),
        stamped(58, "Thanks."),
        assistant(vec![ContentBlock::Text {
            text: "You're welcome.".into(),
        }]),
        stamped(59, "Bye."),
    ];
    let boundary = 2;
    let raws: Vec<String> = [3, 5, 7]
        .iter()
        .map(|n| {
            let rendering = crate::provider::Rendering {
                native_as_text: (0..*n).map(|i| i < boundary).collect(),
                ..crate::provider::Rendering::default()
            };
            let request = Request {
                rendering: &rendering,
                ..plain_request(RequestOptions::default(), &s, &history[..*n], None)
            };
            text_of(&request)
        })
        .collect();
    for pair in raws.windows(2) {
        let (sys_n, msgs_n) = split_body(&pair[0]);
        let (sys_n1, msgs_n1) = split_body(&pair[1]);
        assert_eq!(sys_n, sys_n1);
        let open = msgs_n.strip_suffix(']').unwrap();
        assert!(msgs_n1.starts_with(open), "\n{msgs_n}\n{msgs_n1}");
    }
    let last: Value = serde_json::from_str(&raws[2]).unwrap();
    assert!(!last["messages"][1].to_string().contains("encrypted_"));
    assert_eq!(
        last["messages"][1]["content"][0],
        json!({"type": "text", "text": "[web search: \"rg\"]"})
    );
    assert_eq!(
        last["messages"][3]["content"][1]["type"],
        "web_search_tool_result"
    );
}

#[test]
fn attachments_as_notes_keep_the_prefix_stable_and_off_sends_the_original_bytes() {
    let s = session(Some("You are Wisp."), CacheTtl::OneHour);
    let mut shot = stamped(56, "What does the error say?");
    shot.content
        .push(ContentBlock::image(wisp_protocol::ImageType::Png, "AAAA"));
    let history = [
        shot,
        assistant(vec![ContentBlock::ToolUse {
            id: "toolu_1".into(),
            name: "read".into(),
            input: json!({}),
        }]),
        Message {
            role: wisp_protocol::Role::User,
            content: vec![ContentBlock::ToolResult {
                tool_use_id: "toolu_1".into(),
                content: vec![
                    ContentBlock::Text {
                        text: "spec.pdf".into(),
                    },
                    ContentBlock::media("application/pdf".parse().unwrap(), "BBBB"),
                ],
                is_error: false,
            }],
        },
        assistant(vec![ContentBlock::Text {
            text: "Read.".into(),
        }]),
        stamped(57, "Thanks."),
    ];
    let raw = |n: usize, notes: bool| {
        let rendering = crate::provider::Rendering {
            attachments_as_notes: notes,
            ..crate::provider::Rendering::default()
        };
        let request = Request {
            rendering: &rendering,
            ..plain_request(RequestOptions::default(), &s, &history[..n], None)
        };
        text_of(&request)
    };
    let raws: Vec<String> = [1, 3, 5].iter().map(|n| raw(*n, true)).collect();
    for pair in raws.windows(2) {
        let (sys_n, msgs_n) = split_body(&pair[0]);
        let (sys_n1, msgs_n1) = split_body(&pair[1]);
        assert_eq!(sys_n, sys_n1);
        let open = msgs_n.strip_suffix(']').unwrap();
        assert!(msgs_n1.starts_with(open), "\n{msgs_n}\n{msgs_n1}");
    }
    let last: Value = serde_json::from_str(&raws[2]).unwrap();
    assert!(!raws[2].contains("AAAA") && !raws[2].contains("BBBB"));
    assert_eq!(
        last["messages"][0]["content"]
            .as_array()
            .unwrap()
            .last()
            .unwrap(),
        &json!({"type": "text", "text": "[image/png file not sent: this model can't take it]"})
    );
    assert_eq!(
        last["messages"][2]["content"][0]["content"][1],
        json!({"type": "text", "text": "[application/pdf file not sent: this model can't take it]"})
    );
    let off: Value = serde_json::from_str(&raw(5, false)).unwrap();
    assert_eq!(
        off["messages"][0]["content"]
            .as_array()
            .unwrap()
            .last()
            .unwrap()["type"],
        "image"
    );
    assert_eq!(
        off["messages"][2]["content"][0]["content"][1]["type"],
        "document"
    );
    let unrendered = Request {
        rendering: &crate::provider::UNRENDERED,
        ..plain_request(RequestOptions::default(), &s, &history, None)
    };
    assert_eq!(raw(5, false), text_of(&unrendered));
}

fn hooked(minute: u32, text: &str) -> Message {
    let mut message = stamped(minute, text);
    message.content.insert(
        0,
        ContentBlock::Text {
            text: format!("hook line {minute}\n"),
        },
    );
    message
}

#[test]
fn hook_lines_in_the_lead_keep_the_prefix_stable_across_turns() {
    let options = RequestOptions {
        refusal_fallback: true,
        debug_cache: true,
        message_threads: true,
    };
    let s = session(Some("You are Wisp."), CacheTtl::OneHour);
    let mut plain = conversation();
    plain[0] = stamped(56, "Write a haiku about autumn.");
    plain[2] = stamped(57, "Another, about winter.");
    plain.push(stamped(58, "And spring?"));
    let mut history = plain.clone();
    history[0] = hooked(56, "Write a haiku about autumn.");
    history[2] = hooked(57, "Another, about winter.");
    history[4] = hooked(58, "And spring?");
    let raws: Vec<String> = [1, 3, 5]
        .iter()
        .zip(["msg_a", "msg_b", "msg_c"])
        .map(|(n, prev)| body_with(options, &s, &history[..*n], Some(prev)))
        .collect();
    let bare = body_with(options, &s, &plain, Some("msg_c"));
    for pair in raws.windows(2) {
        let (sys_n, msgs_n) = split_body(&pair[0]);
        let (sys_n1, msgs_n1) = split_body(&pair[1]);
        assert_eq!(sys_n, sys_n1);
        let open = msgs_n.strip_suffix(']').unwrap();
        assert!(msgs_n1.starts_with(open), "\n{msgs_n}\n{msgs_n1}");
    }
    let (bare_system, _) = split_body(&bare);
    assert_eq!(split_body(&raws[2]).0, bare_system);
    let (with, without): (Value, Value) = (
        serde_json::from_str(&raws[2]).unwrap(),
        serde_json::from_str(&bare).unwrap(),
    );
    assert_eq!(with["tools"], without["tools"]);
    let first = &with["messages"][0]["content"];
    assert_eq!(first[0]["text"], "hook line 56\n");
    assert_eq!(
        first[1]["text"],
        "Sent: Wednesday 2026-10-21 23:56:00 UTC-04:00\n"
    );
}

#[tokio::test]
async fn prompt_prefix_survives_a_retried_step() {
    let start = r#"{"type":"message_start","message":{"id":"m0","model":"claude-opus-5-5","usage":{"input_tokens":10,"output_tokens":1}}}"#;
    let text =
        r#"{"type":"content_block_start","index":0,"content_block":{"type":"text","text":""}}"#;
    let error = r#"{"type":"error","error":{"type":"api_error","message":"Internal"}}"#;
    let broken = [
        http::event("message_start", start),
        http::event("content_block_start", text),
        http::event("error", error),
    ]
    .concat();
    let (base, seen) = mock_server(vec![
        http::sse(&broken),
        http::sse(THINKING_AND_TEXT),
        http::sse(THINKING_AND_TEXT),
    ])
    .await;
    let provider = AnthropicProvider::new(&api_key("k"), &base).unwrap();
    let mut agent = crate::agent::Agent::new(Arc::new(provider), crate::agent::default_session())
        .with_clock(crate::clock::TickingClock::new())
        .with_retry_policy(crate::agent::retry::quick());
    let (_tx, mut rx) = tokio::sync::mpsc::channel(4);
    let (ev_tx, mut ev_rx) = tokio::sync::mpsc::channel(512);
    for text in ["Write a haiku about autumn.", "Another, about winter."] {
        agent.run_turn(text.into(), &mut rx, &ev_tx).await.unwrap();
    }
    drop(ev_tx);
    let mut retried = 0;
    while let Some(event) = ev_rx.recv().await {
        retried += usize::from(matches!(event, wisp_protocol::AgentEvent::Retrying { .. }));
    }
    assert_eq!(retried, 1);

    let requests = seen.lock().unwrap();
    let raws: Vec<&str> = requests.iter().map(|r| http::body(r)).collect();
    assert_eq!(raws.len(), 3);
    assert_eq!(raws[0], raws[1], "the retry re-sends the failed bytes");
    let (sys_failed, msgs_failed) = split_body(raws[1]);
    let (sys_next, msgs_next) = split_body(raws[2]);
    assert_eq!(sys_failed, sys_next);
    let open = msgs_failed.strip_suffix(']').unwrap();
    assert!(
        msgs_next.starts_with(open),
        "the next turn extends the retried request\n{msgs_failed}\n{msgs_next}"
    );
}

#[tokio::test]
async fn prompt_prefix_survives_a_queued_step() {
    let (base, seen) = mock_server(vec![
        http::sse(THINKING_AND_TEXT),
        http::sse(THINKING_AND_TEXT),
        http::sse(THINKING_AND_TEXT),
    ])
    .await;
    let lanes = crate::lanes::Lanes::new(
        BTreeMap::from([(crate::agent::IMPLICIT_ENDPOINT.to_owned(), Some(1))]),
        None,
    );
    let provider = AnthropicProvider::new(&api_key("k"), &base).unwrap();
    let mut agent = crate::agent::Agent::new(Arc::new(provider), crate::agent::default_session())
        .with_clock(crate::clock::TickingClock::new())
        .with_lanes(lanes.clone());
    let (_tx, mut rx) = tokio::sync::mpsc::channel(4);
    let (ev_tx, mut ev_rx) = tokio::sync::mpsc::channel(512);
    agent
        .run_turn("Write a haiku about autumn.".into(), &mut rx, &ev_tx)
        .await
        .unwrap();
    let held = match lanes.enter(crate::agent::IMPLICIT_ENDPOINT, 1) {
        Ok(pass) => pass,
        Err(_) => panic!("the lane was busy"),
    };
    let release = async {
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
        drop(held);
    };
    let (queued, ()) = tokio::join!(
        agent.run_turn("Another, about winter.".into(), &mut rx, &ev_tx),
        release
    );
    queued.unwrap();
    agent
        .run_turn("And spring?".into(), &mut rx, &ev_tx)
        .await
        .unwrap();
    drop(ev_tx);
    let mut waits = (0, 0);
    while let Some(event) = ev_rx.recv().await {
        match event {
            wisp_protocol::AgentEvent::RequestQueued { .. } => waits.0 += 1,
            wisp_protocol::AgentEvent::RequestAdmitted { .. } => waits.1 += 1,
            _ => {}
        }
    }
    assert_eq!(waits, (1, 1));

    let requests = seen.lock().unwrap();
    let raws: Vec<&str> = requests.iter().map(|r| http::body(r)).collect();
    assert_eq!(raws.len(), 3);
    for pair in raws.windows(2) {
        let (sys_n, msgs_n) = split_body(pair[0]);
        let (sys_n1, msgs_n1) = split_body(pair[1]);
        assert_eq!(sys_n, sys_n1);
        let open = msgs_n.strip_suffix(']').unwrap();
        assert!(msgs_n1.starts_with(open), "\n{msgs_n}\n{msgs_n1}");
    }
}

#[test]
fn debug_cache_sends_diagnostics() {
    let options = RequestOptions {
        refusal_fallback: false,
        debug_cache: true,
        message_threads: true,
    };
    let s = session(None, CacheTtl::OneHour);
    let first: Value =
        serde_json::from_str(&body_with(options, &s, &conversation()[..1], None)).unwrap();
    assert_eq!(first["diagnostics"], json!({"previous_message_id": null}));
    assert!(first.get("fallbacks").is_none());
    let later: Value =
        serde_json::from_str(&body_with(options, &s, &conversation(), Some("msg_x"))).unwrap();
    assert_eq!(
        later["diagnostics"],
        json!({"previous_message_id": "msg_x"})
    );
    assert_eq!(header_with(options, &[]), "cache-diagnosis-2026-04-07");
}

#[test]
fn the_fallback_beta_stays_while_the_history_holds_a_fallback_block() {
    let off = RequestOptions {
        refusal_fallback: false,
        debug_cache: false,
        message_threads: true,
    };
    let fallback = ContentBlock::opaque("anthropic", fallback_marker("a", "b"));
    let mut history = conversation();
    history[1].content.push(fallback);
    let s = session(None, CacheTtl::OneHour);
    let body: Value = serde_json::from_str(&body_with(off, &s, &history, None)).unwrap();
    assert!(body.get("fallbacks").is_none());
    assert_eq!(
        header_with(off, &history),
        "server-side-fallback-2026-07-01"
    );
    assert_eq!(header_with(off, &conversation()), "");
}

#[test]
fn fallback_echo_omits_internal_blocks_before_boundary() {
    let fallback = ContentBlock::opaque("anthropic", fallback_marker("a", "b"));
    let content = vec![
        ContentBlock::Thinking {
            thinking: "t".into(),
            signature: "s".into(),
        },
        ContentBlock::Text {
            text: "partial".into(),
        },
        fallback,
        ContentBlock::Thinking {
            thinking: "t2".into(),
            signature: "s2".into(),
        },
        ContentBlock::Text {
            text: "rest".into(),
        },
    ];
    assert_eq!(
        echo_types(&content),
        ["text", "fallback", "thinking", "text"]
    );
}

fn echo_types(content: &[ContentBlock]) -> Vec<String> {
    let rendered = serde_json::to_value(assistant_blocks(
        content,
        0,
        &mut Default::default(),
        &crate::provider::UNRENDERED,
    ))
    .unwrap();
    rendered
        .as_array()
        .unwrap()
        .iter()
        .map(|b| b["type"].as_str().unwrap().to_owned())
        .collect()
}

#[test]
fn fallback_echo_keeps_only_paired_server_tool_blocks() {
    let opaque = |raw: Value| ContentBlock::opaque("anthropic", raw);
    let content = vec![
        opaque(
            json!({"type": "server_tool_use", "id": "srv_1", "name": "web_search", "input": {}}),
        ),
        opaque(json!({"type": "web_search_tool_result", "tool_use_id": "srv_1", "content": []})),
        opaque(
            json!({"type": "server_tool_use", "id": "srv_2", "name": "web_search", "input": {}}),
        ),
        ContentBlock::ToolUse {
            id: "toolu_1".into(),
            name: "x".into(),
            input: json!({}),
        },
        opaque(fallback_marker("a", "b")),
        ContentBlock::Text {
            text: "rest".into(),
        },
    ];
    let echoed = serde_json::to_value(assistant_blocks(
        &content,
        0,
        &mut Default::default(),
        &crate::provider::UNRENDERED,
    ))
    .unwrap();
    assert_eq!(
        echo_types(&content),
        [
            "server_tool_use",
            "web_search_tool_result",
            "fallback",
            "text"
        ]
    );
    assert_eq!(echoed[0]["id"], "srv_1");
    assert_eq!(echoed[2], fallback_marker("a", "b"));
}

#[test]
fn the_adapter_classifies_and_leaves_retrying_to_the_kernel() {
    let none = HeaderMap::new();
    for code in [408, 409, 429, 500, 502, 503, 504, 524, 529, 599] {
        let retry = classify::http_error(code, &none, None).detail.retry;
        assert_eq!(retry, RetryClass::Transient, "{code}");
    }
    for code in [400, 401, 402, 403, 404, 413, 422] {
        let retry = classify::http_error(code, &none, None).detail.retry;
        assert_eq!(retry, RetryClass::Never, "{code}");
    }
    let mut h = HeaderMap::new();
    h.insert("retry-after", HeaderValue::from_static("2"));
    assert_eq!(
        classify::http_error(429, &h, None).detail.retry,
        RetryClass::After { ms: 2000 }
    );
    assert_eq!(classify::status_kind(529), ErrorKind::Overloaded);
    assert_eq!(classify::status_kind(401), ErrorKind::Auth);
    assert_eq!(
        classify::kind_of("rate_limit_error"),
        Some(ErrorKind::RateLimit)
    );
}

#[test]
fn credential_has_no_display() {
    trait AmbiguousIfDisplay<A> {
        fn check() {}
    }
    impl<T: ?Sized> AmbiguousIfDisplay<()> for T {}
    struct IsDisplay;
    impl<T: ?Sized + std::fmt::Display> AmbiguousIfDisplay<IsDisplay> for T {}
    <Credential as AmbiguousIfDisplay<_>>::check();
}

fn api_key(key: &str) -> Credential {
    Credential::ApiKey(key.into())
}

#[test]
fn credentials_are_not_debug_printed() {
    for credential in [
        api_key("sk-ant-secret"),
        Credential::AuthToken("tok-secret".into()),
    ] {
        assert!(!format!("{credential:?}").contains("secret"));
        let p = AnthropicProvider::new(&credential, "http://x").unwrap();
        assert!(!format!("{p:?}").contains("secret"));
    }
}

fn env_of(vars: &[(&str, &str)]) -> impl Fn(&str) -> Option<String> {
    let vars: BTreeMap<String, String> = vars
        .iter()
        .map(|(k, v)| ((*k).to_owned(), (*v).to_owned()))
        .collect();
    move |name| vars.get(name).cloned()
}

fn env_file(vars: &[(&str, &str)]) -> EnvFile {
    EnvFile {
        path: "/cfg/wisp/.env".into(),
        vars: vars
            .iter()
            .map(|(k, v)| ((*k).to_owned(), (*v).to_owned()))
            .collect(),
    }
}

#[test]
fn credential_selection() {
    let resolve =
        |env: &[(&str, &str)], file: Option<&EnvFile>| Settings::resolve(env_of(env), file);
    let s = resolve(&[("ANTHROPIC_API_KEY", "key")], None).unwrap();
    assert_eq!(s.credential, api_key("key"));
    assert_eq!(s.base_url, DEFAULT_BASE_URL);
    assert_eq!(s.source, Source::Environment);
    let s = resolve(
        &[
            ("ANTHROPIC_API_KEY", " "),
            ("ANTHROPIC_AUTH_TOKEN", " tok "),
        ],
        None,
    )
    .unwrap();
    assert_eq!(s.credential, Credential::AuthToken("tok".into()));
    let none = resolve(&[("ANTHROPIC_AUTH_TOKEN", "")], None)
        .unwrap_err()
        .to_string();
    assert!(none.contains("ANTHROPIC_API_KEY") && none.contains("ANTHROPIC_AUTH_TOKEN"));
    let file = env_file(&[]);
    let none = resolve(&[], Some(&file)).unwrap_err().to_string();
    assert!(none.contains("/cfg/wisp/.env"), "{none}");
    let bad = AnthropicProvider::new(&Credential::AuthToken("bad\ntoken".into()), "http://x")
        .unwrap_err()
        .to_string();
    assert!(bad.contains("ANTHROPIC_AUTH_TOKEN") && !bad.contains("bad"));
}

#[test]
fn conflicting_credentials_name_their_source() {
    let (key, token) = (
        "sk-ant-api03-REALISTICANARYtKEY",
        "gw-tok-REALISTICsecretTOKEN",
    );
    let env = [("ANTHROPIC_API_KEY", key), ("ANTHROPIC_AUTH_TOKEN", token)];
    let err = Settings::resolve(env_of(&env), None).unwrap_err();
    assert!(matches!(
        &err,
        ConfigError::ConflictingCredentials {
            key: Source::Environment,
            token: Source::Environment
        }
    ));
    let msg = err.to_string();
    assert!(
        msg.contains("ANTHROPIC_API_KEY (from the environment)"),
        "{msg}"
    );
    assert!(!msg.contains(key) && !msg.contains(token), "{msg}");

    let file = env_file(&env);
    let msg = Settings::resolve(env_of(&[]), Some(&file))
        .unwrap_err()
        .to_string();
    assert!(
        msg.contains("ANTHROPIC_AUTH_TOKEN (from /cfg/wisp/.env)"),
        "{msg}"
    );
    assert!(!msg.contains(key) && !msg.contains(token), "{msg}");
}

#[test]
fn env_file_debug_lists_names_only() {
    let file = env_file(&[("ANTHROPIC_API_KEY", "sk-ant-api03-REALISTICANARYtKEY")]);
    let shown = format!("{file:?}");
    assert!(shown.contains("ANTHROPIC_API_KEY"), "{shown}");
    assert!(!shown.contains("CANARY"), "{shown}");
}

#[test]
fn env_file_cannot_redirect_an_environment_key() {
    let file = env_file(&[("ANTHROPIC_BASE_URL", "http://attacker.example")]);
    let s = Settings::resolve(env_of(&[("ANTHROPIC_API_KEY", "real")]), Some(&file)).unwrap();
    assert_eq!(s.base_url, DEFAULT_BASE_URL);
    assert_eq!(s.source, Source::Environment);
    assert_eq!(s.custom_endpoint(), None);

    let file = env_file(&[
        ("ANTHROPIC_AUTH_TOKEN", "file-token"),
        ("ANTHROPIC_BASE_URL", "http://attacker.example"),
    ]);
    let env = [
        ("ANTHROPIC_API_KEY", "real"),
        ("ANTHROPIC_BASE_URL", "https://gw.example.com"),
    ];
    let s = Settings::resolve(env_of(&env), Some(&file)).unwrap();
    assert_eq!(s.credential, api_key("real"));
    assert_eq!(s.base_url, "https://gw.example.com");
    assert_eq!(
        s.custom_endpoint().as_deref(),
        Some("https://gw.example.com")
    );
}

#[test]
fn file_settings_are_used_whole_when_the_environment_has_no_credential() {
    let file = env_file(&[
        ("ANTHROPIC_AUTH_TOKEN", "file-token"),
        ("ANTHROPIC_BASE_URL", "http://localhost:8080/"),
    ]);
    let env = [("ANTHROPIC_BASE_URL", "https://elsewhere.example")];
    let s = Settings::resolve(env_of(&env), Some(&file)).unwrap();
    assert_eq!(s.credential, Credential::AuthToken("file-token".into()));
    assert_eq!(s.base_url, "http://localhost:8080/");
    assert_eq!(s.source, Source::File("/cfg/wisp/.env".into()));
    assert_eq!(
        s.custom_endpoint().as_deref(),
        Some("http://localhost:8080")
    );
}

#[tokio::test]
async fn a_bound_socket_that_never_listens_refuses_connections() {
    let (_socket, addr) = refusing_addr();
    let err = tokio::net::TcpStream::connect(addr).await.unwrap_err();
    assert_eq!(err.kind(), std::io::ErrorKind::ConnectionRefused);
}

fn json_reply(status: &str, extra: &[(&str, &str)], body: &str) -> String {
    let mut headers = vec![("content-type", "application/json")];
    headers.extend_from_slice(extra);
    http::response(status, &headers, body)
}

fn request_json(raw: &str) -> Value {
    serde_json::from_str(http::body(raw)).unwrap()
}

async fn mock_server(responses: Vec<String>) -> (String, Arc<Mutex<Vec<String>>>) {
    let server = http::serve(responses.into_iter().map(http::Reply::Send).collect()).await;
    (server.url, server.requests)
}

#[tokio::test]
async fn the_stream_connects_only_when_polled_after_sending() {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base = format!("http://{}", listener.local_addr().unwrap());
    let provider = AnthropicProvider::new(&api_key("k"), &base).unwrap();
    let s = session(Some("sys"), CacheTtl::OneHour);
    let messages = [Message::user_text("hi")];
    let request = Request {
        session: &s,
        tools: &[],
        deferred: &[],
        by_value: &[],
        server_tools: &[],
        messages: &messages,
        previous_response_id: None,
        features: Features::default(),
        options: Default::default(),
        rendering: &crate::provider::UNRENDERED,
        compaction: None,
    };
    let quiet = std::time::Duration::from_millis(300);
    let mut stream = provider.stream(&request);
    assert!(
        tokio::time::timeout(quiet, listener.accept())
            .await
            .is_err(),
        "nothing connects before the first poll"
    );
    let first = stream.next().await;
    assert!(
        matches!(first, Some(Ok(ProviderEvent::Sending))),
        "{first:?}"
    );
    assert!(
        tokio::time::timeout(quiet, listener.accept())
            .await
            .is_err(),
        "yielding Sending connects nothing"
    );
    let polled = tokio::spawn(async move { stream.next().await.is_some() });
    let accepted = within("the next poll's connection", listener.accept()).await;
    assert!(accepted.is_ok(), "the next poll connects");
    drop(accepted);
    polled.abort();
}

#[tokio::test]
async fn provider_requests_send_no_user_agent_while_plugin_fetches_send_wisps() {
    use crate::plugin::http::tests::wisp_agent;
    use crate::test_guard::http::{Heard, Reply, header_lines, response, serve};
    use wisp_plugin_api::http::Http;

    let mut plugin_server = serve(vec![Reply::Send(response("200 OK", &[], ""))]).await;
    let host = crate::plugin::PluginHost::new(
        "probe",
        "0.2.0",
        &[],
        Arc::new(crate::plugin::KernelHost::new()),
    );
    host.fetch(wisp_plugin_api::http::Request {
        method: "GET".to_owned(),
        url: plugin_server.url.clone(),
        ..Default::default()
    })
    .await
    .unwrap();
    let Heard::Request(fetched) = plugin_server.next().await else {
        panic!("no request");
    };
    let agents: Vec<String> = header_lines(&fetched)
        .into_iter()
        .filter(|(name, _)| name == "user-agent")
        .map(|(_, value)| value)
        .collect();
    assert_eq!(agents, [wisp_agent()]);

    let (base, seen) = mock_server(vec![http::sse(THINKING_AND_TEXT)]).await;
    let provider = AnthropicProvider::new(&api_key("sk-test-key"), &base).unwrap();
    let s = session(Some("sys"), CacheTtl::OneHour);
    let messages = [Message::user_text("hi")];
    let request = Request {
        session: &s,
        tools: &[],
        deferred: &[],
        by_value: &[],
        server_tools: &[],
        messages: &messages,
        previous_response_id: None,
        features: Features::default(),
        options: Default::default(),
        rendering: &crate::provider::UNRENDERED,
        compaction: None,
    };
    let streamed: Vec<_> = provider.stream(&request).collect().await;
    assert!(streamed.iter().all(Result::is_ok), "{streamed:?}");
    let requests = seen.lock().unwrap();
    assert_eq!(requests.len(), 1);
    let lower = requests[0].to_ascii_lowercase();
    assert!(lower.starts_with("post /v1/messages "), "{lower}");
    assert!(!lower.contains("user-agent"), "{lower}");
    assert!(!lower.contains("wisp/"), "{lower}");
}

#[tokio::test]
async fn end_to_end_a_529_is_one_attempt_and_the_next_request_streams() {
    let overloaded =
        r#"{"type":"error","error":{"type":"overloaded_error","message":"Overloaded"}}"#;
    let responses = vec![
        json_reply("529 Overloaded", &[("retry-after", "0")], overloaded),
        http::sse(THINKING_AND_TEXT),
    ];
    let (base, seen) = mock_server(responses).await;
    let provider = AnthropicProvider::new(&api_key("sk-test-key"), &base).unwrap();
    let s = session(Some("sys"), CacheTtl::OneHour);
    let messages = [Message::user_text("hi")];
    let request = Request {
        session: &s,
        tools: &[],
        deferred: &[],
        by_value: &[],
        server_tools: &[],
        messages: &messages,
        previous_response_id: None,
        features: Features::default(),
        options: Default::default(),
        rendering: &crate::provider::UNRENDERED,
        compaction: None,
    };
    let first: Vec<_> = provider.stream(&request).collect().await;
    let [Ok(ProviderEvent::Sending), Err(err)] = first.as_slice() else {
        panic!("{first:?}")
    };
    assert_eq!(err.kind, ErrorKind::Overloaded);
    assert_eq!(err.detail.status, Some(529));
    assert_eq!(err.detail.retry, RetryClass::After { ms: 0 });
    assert!(!err.possibly_billed);
    assert_eq!(seen.lock().unwrap().len(), 1, "the adapter never re-sends");
    let mut stream = provider.stream(&request);
    let mut asm = Assembler::default();
    while let Some(item) = stream.next().await {
        asm.apply(item.unwrap()).unwrap();
    }
    let msg = asm.finish().unwrap();
    assert_eq!(msg.stop_reason, StopReason::EndTurn);
    assert_eq!(msg.content.len(), 2);

    let requests = seen.lock().unwrap();
    assert_eq!(requests.len(), 2);
    let req = requests[1].to_ascii_lowercase();
    assert!(req.starts_with("post /v1/messages "));
    assert!(req.contains("x-api-key: sk-test-key"));
    assert!(req.contains("anthropic-version: 2023-06-01"));
    assert!(req.contains("anthropic-beta: server-side-fallback-2026-07-01"));
    assert!(req.contains("content-type: application/json"));
}

const M203: &str = r#"{"type":"error","error":{"type":"rate_limit_error","message":"[Manifest M203] Too many concurrent requests"}}"#;
const SPEND_CAP: &str = r#"{"type":"error","error":{"type":"rate_limit_error","message":"You have reached your specified API usage limits.","details":{"error_code":"enforced_spend_limit_reached"}}}"#;
const OVERLOADED: &str =
    r#"{"type":"error","error":{"type":"overloaded_error","message":"Overloaded"}}"#;

#[tokio::test]
async fn end_to_end_a_busy_429_and_529_wait_and_a_spend_cap_fails_at_once() {
    let responses = vec![
        http::json("429 Too Many Requests", M203),
        http::json("429 Too Many Requests", SPEND_CAP),
        json_reply("529 Overloaded", &[("retry-after", "3")], OVERLOADED),
    ];
    let (base, _) = mock_server(responses).await;
    let provider = AnthropicProvider::new(&api_key("sk-test-key"), &base).unwrap();
    let s = session(None, CacheTtl::OneHour);
    let messages = [Message::user_text("hi")];
    let request = Request {
        session: &s,
        tools: &[],
        deferred: &[],
        by_value: &[],
        server_tools: &[],
        messages: &messages,
        previous_response_id: None,
        features: Features::default(),
        options: Default::default(),
        rendering: &crate::provider::UNRENDERED,
        compaction: None,
    };
    let mut errors = Vec::new();
    for _ in 0..3 {
        let events: Vec<_> = provider.stream(&request).collect().await;
        let Some(Err(error)) = events.last() else {
            panic!("{events:?}")
        };
        errors.push(error.clone());
    }
    let policy = crate::agent::retry::RetryPolicy::default();
    let busy = |e: &ProviderError| crate::agent::retry::is_busy(&policy, e, e.possibly_billed);
    let [m203, cap, overloaded] = errors.as_slice() else {
        panic!("{errors:?}")
    };
    assert_eq!(
        (m203.kind, m203.detail.retry, m203.possibly_billed),
        (ErrorKind::RateLimit, RetryClass::Transient, false)
    );
    assert!(busy(m203));
    assert_eq!(
        (cap.kind, cap.detail.retry),
        (ErrorKind::Billing, RetryClass::Never)
    );
    assert!(!busy(cap));
    assert_eq!(
        (overloaded.kind, overloaded.detail.retry),
        (ErrorKind::Overloaded, RetryClass::After { ms: 3_000 })
    );
    assert!(busy(overloaded));
}

#[tokio::test]
async fn end_to_end_auth_error_is_not_retried() {
    let body =
        r#"{"type":"error","error":{"type":"authentication_error","message":"invalid x-api-key"}}"#;
    let responses = vec![http::json("401 Unauthorized", body)];
    let (base, seen) = mock_server(responses).await;
    let provider = AnthropicProvider::new(&api_key("bad"), &base).unwrap();
    let s = session(None, CacheTtl::OneHour);
    let messages = [Message::user_text("hi")];
    let request = Request {
        session: &s,
        tools: &[],
        deferred: &[],
        by_value: &[],
        server_tools: &[],
        messages: &messages,
        previous_response_id: None,
        features: Features::default(),
        options: Default::default(),
        rendering: &crate::provider::UNRENDERED,
        compaction: None,
    };
    let items: Vec<_> = provider.stream(&request).collect().await;
    assert_eq!(items.len(), 2);
    assert_eq!(items[0], Ok(ProviderEvent::Sending));
    let err = items.into_iter().nth(1).unwrap().unwrap_err();
    assert_eq!(err.kind, ErrorKind::Auth);
    assert!(!err.possibly_billed);
    assert_eq!(seen.lock().unwrap().len(), 1);
}

fn hi_request(s: &SessionConfig) -> Request<'_> {
    const HI: &[Message] = &[];
    Request {
        session: s,
        tools: &[],
        deferred: &[],
        by_value: &[],
        server_tools: &[],
        messages: HI,
        previous_response_id: None,
        features: Features::default(),
        options: Default::default(),
        rendering: &crate::provider::UNRENDERED,
        compaction: None,
    }
}

#[tokio::test]
async fn end_to_end_bearer_token_auth() {
    let responses = vec![http::sse(THINKING_AND_TEXT)];
    let (base, seen) = mock_server(responses).await;
    let credential = Credential::AuthToken("gw-token".into());
    let provider = AnthropicProvider::new(&credential, &base).unwrap();
    let s = session(None, CacheTtl::OneHour);
    let items: Vec<_> = provider.stream(&hi_request(&s)).collect().await;
    assert!(items.iter().all(Result::is_ok));
    let req = seen.lock().unwrap()[0].to_ascii_lowercase();
    assert!(req.contains("authorization: bearer gw-token"));
    assert!(!req.contains("x-api-key"));
}

#[tokio::test]
async fn end_to_end_gateway_json_reply_is_a_typed_error() {
    let body = r#"{"error":{"message":"model not allowed on this plan"}}"#;
    let responses = vec![http::json("200 OK", body)];
    let (base, _) = mock_server(responses).await;
    let provider = AnthropicProvider::new(&api_key("k"), &base).unwrap();
    let s = session(None, CacheTtl::OneHour);
    let items: Vec<_> = provider.stream(&hi_request(&s)).collect().await;
    let [Ok(ProviderEvent::Sending), Err(err)] = items.as_slice() else {
        panic!("{items:?}")
    };
    assert_eq!(err.kind, ErrorKind::InvalidResponse);
    assert!(err.message.contains("model not allowed on this plan"));
    assert!(err.possibly_billed);
}

#[tokio::test]
async fn request_lost_after_sending_is_one_transient_possibly_billed_attempt() {
    let (base, seen) = mock_server(vec![String::new()]).await;
    let provider = AnthropicProvider::new(&api_key("k"), &base).unwrap();
    let s = session(None, CacheTtl::OneHour);
    let items: Vec<_> = provider.stream(&hi_request(&s)).collect().await;
    let [Ok(ProviderEvent::Sending), Err(err)] = items.as_slice() else {
        panic!("{items:?}")
    };
    assert_eq!(err.kind, ErrorKind::Network);
    assert!(err.message.contains("after sending"), "{}", err.message);
    assert_eq!(err.detail.phase, FailurePhase::Response);
    assert_eq!(err.detail.retry, RetryClass::Transient);
    assert!(err.possibly_billed);
    assert_eq!(seen.lock().unwrap().len(), 1);
}

#[tokio::test]
async fn connection_refused_is_one_unbilled_transient_attempt() {
    let (_socket, addr) = refusing_addr();
    let provider = AnthropicProvider::new(&api_key("k"), &format!("http://{addr}")).unwrap();
    let s = session(None, CacheTtl::OneHour);
    let mut stream = provider.stream(&hi_request(&s));
    assert_eq!(stream.next().await.unwrap(), Ok(ProviderEvent::Sending));
    let err = stream.next().await.unwrap().unwrap_err();
    assert_eq!(err.kind, ErrorKind::Network);
    assert_eq!(err.detail.phase, FailurePhase::Connect);
    assert_eq!(err.detail.retry, RetryClass::Transient);
    assert!(!err.possibly_billed);
    assert!(stream.next().await.is_none(), "one attempt only");
}

mod review {
    use super::*;
    use crate::agent::BASE_SYSTEM_PROMPT;
    use crate::plugin::testing::turn_line_tail;

    use crate::agent::Agent;
    use tokio::sync::mpsc;
    use wisp_protocol::{AgentEvent, ClientCommand, TurnOutcome};

    fn http_error(status: &str, body: &str) -> String {
        json_reply(status, &[("retry-after", "0")], body)
    }

    #[tokio::test]
    async fn stamped_turns_resend_identical_bodies_and_extend_the_prefix() {
        let overloaded =
            r#"{"type":"error","error":{"type":"overloaded_error","message":"Overloaded"}}"#;
        let responses = vec![
            http_error("529 Overloaded", overloaded),
            http::sse(THINKING_AND_TEXT),
            http::sse(THINKING_AND_TEXT),
        ];
        let (base, seen) = mock_server(responses).await;
        let provider = AnthropicProvider::new(&api_key("k"), &base).unwrap();
        let mut agent = Agent::new(Arc::new(provider), crate::agent::default_session())
            .with_clock(crate::clock::TickingClock::new())
            .with_retry_policy(crate::agent::retry::quick());
        for text in ["first", "second"] {
            let (outcome, _) = turn(&mut agent, text, &[]).await;
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
        }

        let reqs = seen.lock().unwrap();
        assert_eq!(reqs.len(), 3);
        let bodies: Vec<&str> = reqs.iter().map(|r| http::body(r)).collect();
        assert_eq!(bodies[0], bodies[1]);
        let (sys_1, msgs_1) = split_body(bodies[1]);
        let (sys_2, msgs_2) = split_body(bodies[2]);
        assert_eq!(sys_1, sys_2);
        assert!(
            msgs_2.starts_with(msgs_1.strip_suffix(']').unwrap()),
            "\n{msgs_1}\n{msgs_2}"
        );

        let second = request_json(&reqs[2]);
        assert_eq!(
            second["system"][0]["text"],
            format!("{BASE_SYSTEM_PROMPT} {}", turn_line_tail())
        );
        let messages = second["messages"].as_array().unwrap();
        assert_eq!(messages.len(), 3);
        assert_eq!(
            messages[0]["content"],
            json!([
                {"type": "text", "text": "Sent: Wednesday 2026-10-21 23:58:00 UTC-04:00\n"},
                {"type": "text", "text": "Context: 168 of 1M tokens (0%). Wisp compacts at 850k.\n"},
                {"type": "text", "text": "first"},
            ])
        );
        assert_eq!(
            messages[2]["content"],
            json!([
                {"type": "text", "text": "Sent: Thursday 2026-10-22 00:01:00 UTC-04:00\n"},
                {"type": "text", "text": "Context: 3k of 1M tokens (0%). Wisp compacts at 850k.\n"},
                {"type": "text", "text": "second"},
            ])
        );
    }

    #[tokio::test]
    async fn hooked_turns_resend_identical_bodies_and_extend_the_prefix() {
        use wisp_plugin_testkit::hooks::{self as kit, Calls};
        let overloaded =
            r#"{"type":"error","error":{"type":"overloaded_error","message":"Overloaded"}}"#;
        let responses = vec![
            http_error("529 Overloaded", overloaded),
            http::sse(THINKING_AND_TEXT),
            http::sse(THINKING_AND_TEXT),
            http::sse(THINKING_AND_TEXT),
            http::sse(THINKING_AND_TEXT),
        ];
        let (base, seen) = mock_server(responses).await;
        let provider = AnthropicProvider::new(&api_key("k"), &base).unwrap();
        let calls = Calls::default();
        let body = "\n[[hooks]]\nname = \"x\"\npoint = \"before-turn\"\n\n[[hooks]]\nname = \"asked\"\npoint = \"before-request\"\n\n[[hooks]]\nname = \"seen\"\npoint = \"after-turn\"\n".to_owned()
            + &kit::command("send", "");
        let armed = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let (go, sent) = (
            Arc::new(tokio::sync::Notify::new()),
            Arc::new(tokio::sync::Notify::new()),
        );
        let script: kit::Script = {
            let (armed, go, sent) = (armed.clone(), go.clone(), sent.clone());
            Arc::new(move |_, hook, _| {
                let (armed, go, sent) = (armed.clone(), go.clone(), sent.clone());
                Box::pin(async move {
                    if hook == "x" {
                        return wisp_plugin_api::hooks::Reply::Append(vec!["X\n".to_owned()]);
                    }
                    if hook == "asked" && armed.swap(false, std::sync::atomic::Ordering::SeqCst) {
                        go.notify_one();
                        sent.notified().await;
                        tokio::time::sleep(std::time::Duration::from_millis(1000)).await;
                    }
                    wisp_plugin_api::hooks::Reply::None
                })
            })
        };
        let run: kit::Run = Arc::new(move |_, _, args| {
            let (go, sent) = (go.clone(), sent.clone());
            Box::pin(async move {
                go.notified().await;
                sent.notify_one();
                wisp_plugin_api::commands::CommandReply::Send(args)
            })
        });
        let make: crate::plugin::Make = kit::commanding(script, run, calls.clone());
        let mut sources = crate::plugin::testing::turn_line_sources();
        sources.push((kit::manifest("aa", &body), make));
        let (registry, problems) =
            crate::plugin::Registry::from_sources(sources, &crate::plugin::PluginConfig::default());
        assert!(problems.is_empty(), "{problems:?}");
        let started = registry
            .start(
                &std::collections::BTreeSet::new(),
                Arc::new(crate::plugin::KernelHost::new()),
            )
            .await;
        let mut agent = Agent::new(Arc::new(provider), crate::agent::default_session())
            .with_clock(crate::clock::TickingClock::new())
            .with_retry_policy(crate::agent::retry::quick())
            .with_hooks(started.hooks, started.host);
        for text in ["first", "second"] {
            let (outcome, _) = turn(&mut agent, text, &[]).await;
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
        }
        assert_eq!(calls.count("x"), 2);
        assert_eq!(calls.count("asked"), 3);

        armed.store(true, std::sync::atomic::Ordering::SeqCst);
        let (tx, mut rx) = mpsc::channel::<ClientCommand>(4);
        tx.send(ClientCommand::RunPluginCommand {
            name: "aa:send".to_owned(),
            args: wisp_protocol::Secret::new("steered by a command".to_owned()),
            request: wisp_protocol::RequestId(7),
        })
        .await
        .unwrap();
        let (ev_tx, mut ev_rx) = mpsc::channel::<AgentEvent>(256);
        let outcome = agent
            .run_turn("third".into(), &mut rx, &ev_tx)
            .await
            .unwrap();
        drop(ev_tx);
        let mut accepted = Vec::new();
        while let Some(event) = ev_rx.recv().await {
            if let AgentEvent::InputAccepted { text, .. } = event {
                accepted.push(text);
            }
        }
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
        assert_eq!(accepted, ["steered by a command"]);

        let reqs = seen.lock().unwrap();
        assert_eq!(reqs.len(), 5);
        let bodies: Vec<&str> = reqs.iter().map(|r| http::body(r)).collect();
        for pair in bodies[2..].windows(2) {
            let (sys_a, msgs_a) = split_body(pair[0]);
            let (sys_b, msgs_b) = split_body(pair[1]);
            assert_eq!(sys_a, sys_b);
            assert!(
                msgs_b.starts_with(msgs_a.strip_suffix(']').unwrap()),
                "\n{msgs_a}\n{msgs_b}"
            );
        }
        let last = request_json(&reqs[4]);
        let steer = last["messages"]
            .as_array()
            .unwrap()
            .last()
            .unwrap()
            .to_string();
        assert!(steer.contains("steered by a command"), "{steer}");
        drop(reqs);
        let reqs = seen.lock().unwrap();
        let bodies: Vec<&str> = reqs.iter().map(|r| http::body(r)).collect();
        assert_eq!(bodies[0], bodies[1]);
        let (sys_1, msgs_1) = split_body(bodies[1]);
        let (sys_2, msgs_2) = split_body(bodies[2]);
        assert_eq!(sys_1, sys_2);
        assert!(
            msgs_2.starts_with(msgs_1.strip_suffix(']').unwrap()),
            "\n{msgs_1}\n{msgs_2}"
        );
        let second = request_json(&reqs[2]);
        assert_eq!(
            second["system"][0]["text"],
            format!("{BASE_SYSTEM_PROMPT} {}", turn_line_tail())
        );
        assert!(second.get("tools").is_none());
        let messages = second["messages"].as_array().unwrap();
        assert_eq!(messages.len(), 3);
        for (message, (stamp, text)) in [&messages[0], &messages[2]].into_iter().zip([
            ("Sent: Wednesday 2026-10-21 23:58:00 UTC-04:00\n", "first"),
            ("Sent: Thursday 2026-10-22 00:01:00 UTC-04:00\n", "second"),
        ]) {
            let content = message["content"].as_array().unwrap();
            assert_eq!(content.len(), 4);
            assert_eq!(content[0], json!({"type": "text", "text": "X\n"}));
            assert_eq!(content[1], json!({"type": "text", "text": stamp}));
            assert!(
                content[2]["text"]
                    .as_str()
                    .unwrap()
                    .starts_with("Context: "),
                "{content:?}"
            );
            assert_eq!(content[3], json!({"type": "text", "text": text}));
        }
    }

    #[tokio::test]
    async fn rewritten_and_continued_turns_resend_identical_bodies_and_extend_the_prefix() {
        use wisp_plugin_api::hooks::{Event, Reply};
        use wisp_plugin_testkit::hooks::{self as kit, Calls};
        let overloaded =
            r#"{"type":"error","error":{"type":"overloaded_error","message":"Overloaded"}}"#;
        let responses = vec![
            http_error("529 Overloaded", overloaded),
            http::sse(THINKING_AND_TEXT),
            http::sse(THINKING_AND_TEXT),
            http::sse(THINKING_AND_TEXT),
        ];
        let (base, seen) = mock_server(responses).await;
        let provider = AnthropicProvider::new(&api_key("k"), &base).unwrap();
        let calls = Calls::default();
        let body = "\n[[hooks]]\nname = \"typed\"\npoint = \"user-input\"\n\n[[hooks]]\nname = \"more\"\npoint = \"before-end\"\n";
        let script: kit::Script = Arc::new(|_, _, event| {
            Box::pin(async move {
                match event {
                    Event::UserInput(input) if input.text == "first" => {
                        Reply::Rewrite("FIRST, rewritten".to_owned())
                    }
                    Event::BeforeEnd(end) if end.turn == 1 && end.continues == 0 => {
                        Reply::Continue("Go on.".to_owned())
                    }
                    _ => Reply::None,
                }
            })
        });
        let make: crate::plugin::Make = kit::instantiate(script, calls.clone());
        let mut sources = crate::plugin::testing::turn_line_sources();
        sources.push((kit::manifest("aa", body), make));
        let (registry, problems) =
            crate::plugin::Registry::from_sources(sources, &crate::plugin::PluginConfig::default());
        assert!(problems.is_empty(), "{problems:?}");
        let started = registry
            .start(
                &std::collections::BTreeSet::new(),
                Arc::new(crate::plugin::KernelHost::new()),
            )
            .await;
        let mut agent = Agent::new(Arc::new(provider), crate::agent::default_session())
            .with_clock(crate::clock::TickingClock::new())
            .with_retry_policy(crate::agent::retry::quick())
            .with_hooks(started.hooks, started.host);
        for text in ["first", "second"] {
            let (outcome, _) = turn(&mut agent, text, &[]).await;
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
        }
        assert_eq!(calls.count("typed"), 2);
        assert_eq!(calls.count("more"), 3);

        let reqs = seen.lock().unwrap();
        assert_eq!(reqs.len(), 4);
        let bodies: Vec<&str> = reqs.iter().map(|r| http::body(r)).collect();
        assert_eq!(bodies[0], bodies[1]);
        for pair in bodies[1..].windows(2) {
            let (sys_a, msgs_a) = split_body(pair[0]);
            let (sys_b, msgs_b) = split_body(pair[1]);
            assert_eq!(sys_a, sys_b);
            assert!(
                msgs_b.starts_with(msgs_a.strip_suffix(']').unwrap()),
                "\n{msgs_a}\n{msgs_b}"
            );
        }
        assert!(!bodies[3].contains(r#""text":"first""#));
        let messages = request_json(&reqs[3])["messages"]
            .as_array()
            .unwrap()
            .clone();
        assert_eq!(messages.len(), 5);
        let texts: Vec<&Value> = messages
            .iter()
            .filter(|m| m["role"] == "user")
            .map(|m| m["content"].as_array().unwrap().last().unwrap())
            .collect();
        assert_eq!(
            texts,
            [
                &json!({"type": "text", "text": "FIRST, rewritten"}),
                &json!({"type": "text", "text": "Go on."}),
                &json!({"type": "text", "text": "second"}),
            ]
        );
    }

    const START: &str = "event: message_start\ndata: {\"type\":\"message_start\",\"message\":{\"id\":\"m1\",\"model\":\"claude-opus-5-5\",\"usage\":{\"input_tokens\":10,\"output_tokens\":1}}}\n\n";
    const END: &str = "event: message_delta\ndata: {\"type\":\"message_delta\",\"delta\":{\"stop_reason\":\"end_turn\"},\"usage\":{\"output_tokens\":5}}\n\nevent: message_stop\ndata: {\"type\":\"message_stop\"}\n\n";

    fn one_block(start: &str, deltas: &[&str]) -> String {
        let mut sse = format!(
            "{START}{}",
            http::event(
                "content_block_start",
                format!(r#"{{"type":"content_block_start","index":0,"content_block":{start}}}"#)
            )
        );
        for d in deltas {
            sse.push_str(&http::event(
                "content_block_delta",
                format!(r#"{{"type":"content_block_delta","index":0,"delta":{d}}}"#),
            ));
        }
        sse.push_str(&http::event(
            "content_block_stop",
            r#"{"type":"content_block_stop","index":0}"#,
        ));
        sse.push_str(END);
        sse
    }

    #[tokio::test]
    async fn promoted_blocks_do_not_poison_history() {
        let a = one_block(
            r#"{"type":"thinking","thinking":"","signature":""}"#,
            &[
                r#"{"type":"thinking_delta","thinking":"half a thought"}"#,
                r#"{"type":"future_delta","x":1}"#,
            ],
        );
        let b = one_block(
            r#"{"type":"text","text":""}"#,
            &[r#"{"type":"future_delta","x":2}"#],
        );
        let c = one_block(
            r#"{"type":"text","text":""}"#,
            &[
                r#"{"type":"text_delta","text":"hello"}"#,
                r#"{"type":"future_delta","x":3}"#,
            ],
        );
        let responses = vec![
            http::sse(&a),
            http::sse(&b),
            http::sse(&c),
            http::sse(THINKING_AND_TEXT),
        ];
        let (base, seen) = mock_server(responses).await;
        let provider = AnthropicProvider::new(&api_key("k"), &base).unwrap();
        let mut agent = Agent::new(Arc::new(provider), crate::agent::default_session())
            .with_clock(crate::clock::TickingClock::new());

        for words in ["one", "two"] {
            let (outcome, _) = turn(&mut agent, words, &[]).await;
            assert!(
                matches!(
                    outcome,
                    TurnOutcome::Completed {
                        committed: true,
                        ..
                    }
                ),
                "{words}: {outcome:?}"
            );
            assert_eq!(
                agent.history().last().unwrap().content,
                [ContentBlock::Text {
                    text: crate::provider::NO_TEXT.into()
                }]
            );
        }
        let (o3, _) = turn(&mut agent, "three", &[]).await;
        assert!(
            matches!(
                o3,
                TurnOutcome::Completed {
                    committed: true,
                    ..
                }
            ),
            "{o3:?}"
        );
        let ContentBlock::Opaque { deltas, .. } = &agent.history()[5].content[0] else {
            panic!("{:?}", agent.history());
        };
        assert_eq!(deltas, &[json!({"type": "future_delta", "x": 3})]);
        let _ = turn(&mut agent, "four", &[]).await;

        let reqs = seen.lock().unwrap();
        let fourth = request_json(&reqs[3]);
        assert_eq!(
            fourth["messages"],
            json!([
                {"role": "user", "content": [
                    {"type": "text", "text": "Sent: Wednesday 2026-10-21 23:58:00 UTC-04:00\n"},
                    {"type": "text", "text": "Context: 167 of 1M tokens (0%). Wisp compacts at 850k.\n"},
                    {"type": "text", "text": "one"},
                ]},
                {"role": "assistant", "content": [{"type": "text", "text": "[no text]"}]},
                {"role": "user", "content": [
                    {"type": "text", "text": "Sent: Thursday 2026-10-22 00:00:00 UTC-04:00\n"},
                    {"type": "text", "text": "Context: 31 of 1M tokens (0%). Wisp compacts at 850k.\n"},
                    {"type": "text", "text": "two"},
                ]},
                {"role": "assistant", "content": [{"type": "text", "text": "[no text]"}]},
                {"role": "user", "content": [
                    {"type": "text", "text": "Sent: Thursday 2026-10-22 00:02:00 UTC-04:00\n"},
                    {"type": "text", "text": "Context: 32 of 1M tokens (0%). Wisp compacts at 850k.\n"},
                    {"type": "text", "text": "three"},
                ]},
                {"role": "assistant", "content": [{"type": "text", "text": "hello"}]},
                {"role": "user", "content": [
                    {"type": "text", "text": "Sent: Thursday 2026-10-22 00:04:00 UTC-04:00\n"},
                    {"type": "text", "text": "Context: 32 of 1M tokens (0%). Wisp compacts at 850k.\n"},
                    {"type": "text", "text": "four"},
                ]},
            ])
        );
    }

    #[tokio::test]
    async fn a_gateway_504_is_one_possibly_billed_attempt() {
        let responses = vec![
            http::response("504 Gateway Timeout", &[], ""),
            http::sse(THINKING_AND_TEXT),
        ];
        let (base, seen) = mock_server(responses).await;
        let provider = AnthropicProvider::new(&api_key("k"), &base).unwrap();
        let s = session(None, CacheTtl::OneHour);
        let items: Vec<_> = provider.stream(&hi_request(&s)).collect().await;
        let [Ok(ProviderEvent::Sending), Err(err)] = items.as_slice() else {
            panic!("{items:?}")
        };
        assert_eq!(err.detail.status, Some(504));
        assert!(!err.message.contains("504"), "{}", err.message);
        assert_eq!(err.detail.retry, RetryClass::Transient);
        assert!(err.possibly_billed);
        assert_eq!(seen.lock().unwrap().len(), 1);
    }

    #[tokio::test]
    async fn every_server_status_is_one_attempt_the_kernel_may_retry() {
        let s = session(None, CacheTtl::OneHour);
        let envoy = "upstream connect error or disconnect/reset before headers";
        let api_error = r#"{"type":"error","error":{"type":"api_error","message":"Internal"}}"#;
        let org = "00000000-0000-0000-0000-000000000000";
        let limit = format!(
            r#"{{"type":"error","error":{{"type":"rate_limit_error","message":"This request would exceed the rate limit for your organization ({org}) of 50,000 input tokens per minute."}}}}"#
        );
        let typed = format!(r#"{{"type":"error","error":{{"type":"limit for {org}"}}}}"#);
        let straddles = format!("{} {org} tail", "x".repeat(480));
        let head = &org[..9];
        for (status, body) in [
            ("500 Internal Server Error", envoy),
            ("503 Service Unavailable", envoy),
            ("408 Request Timeout", envoy),
            ("502 Bad Gateway", api_error),
            ("429 Too Many Requests", "slow down"),
            ("429 Too Many Requests", &limit),
            ("503 Service Unavailable", &typed),
            ("503 Service Unavailable", &straddles),
        ] {
            let (base, seen) = mock_server(vec![http_error(status, body)]).await;
            let provider = AnthropicProvider::new(&api_key("k"), &base).unwrap();
            let items: Vec<_> = provider.stream(&hi_request(&s)).collect().await;
            let [Ok(ProviderEvent::Sending), Err(e)] = items.as_slice() else {
                panic!("{status}: {items:?}")
            };
            assert_ne!(e.detail.retry, RetryClass::Never, "{status}");
            assert!(!e.message.contains("not retried"), "{}", e.message);
            assert!(!e.message.contains(head), "{}", e.message);
            assert!(
                !format!("{:?}", e.detail.error_type).contains(head),
                "{status}"
            );
            if body == limit {
                assert_eq!(
                    e.message,
                    "This request would exceed the rate limit for your organization ([redacted]) of 50,000 input tokens per minute."
                );
            }
            assert_eq!(seen.lock().unwrap().len(), 1, "{status}");
        }
    }
}

mod review2 {
    use super::*;
    use crate::agent::Agent;
    use tokio::sync::mpsc;
    use wisp_protocol::{AgentEvent, ClientCommand, TurnOutcome};

    fn session_lower_bound(o: &TurnOutcome) -> bool {
        match o {
            TurnOutcome::Completed { cost: Some(c), .. } => c.session_lower_bound,
            other => panic!("{other:?}"),
        }
    }

    const START: &str = "event: message_start\ndata: {\"type\":\"message_start\",\"message\":{\"id\":\"m1\",\"model\":\"claude-opus-5-5\",\"usage\":{\"input_tokens\":10,\"output_tokens\":1}}}\n\n";
    const END: &str = "event: message_delta\ndata: {\"type\":\"message_delta\",\"delta\":{\"stop_reason\":\"end_turn\"},\"usage\":{\"output_tokens\":5}}\n\nevent: message_stop\ndata: {\"type\":\"message_stop\"}\n\n";

    #[tokio::test]
    async fn signed_thinking_with_unknown_delta_is_not_committed() {
        let mut sse = String::from(START);
        sse += &http::event(
            "content_block_start",
            r#"{"type":"content_block_start","index":0,"content_block":{"type":"thinking","thinking":"","signature":""}}"#,
        );
        sse += &http::event(
            "content_block_delta",
            r#"{"type":"content_block_delta","index":0,"delta":{"type":"thinking_delta","thinking":"part one "}}"#,
        );
        sse += &http::event(
            "content_block_delta",
            r#"{"type":"content_block_delta","index":0,"delta":{"type":"thinking_delta_v2","thinking":"part two"}}"#,
        );
        sse += &http::event(
            "content_block_delta",
            r#"{"type":"content_block_delta","index":0,"delta":{"type":"signature_delta","signature":"sig-over-both-parts"}}"#,
        );
        sse += &http::event(
            "content_block_stop",
            r#"{"type":"content_block_stop","index":0}"#,
        );
        sse += &http::event(
            "content_block_start",
            r#"{"type":"content_block_start","index":1,"content_block":{"type":"text","text":""}}"#,
        );
        sse += &http::event(
            "content_block_delta",
            r#"{"type":"content_block_delta","index":1,"delta":{"type":"text_delta","text":"hello"}}"#,
        );
        sse += &http::event(
            "content_block_stop",
            r#"{"type":"content_block_stop","index":1}"#,
        );
        sse += END;
        let (base, seen) = mock_server(vec![http::sse(&sse), http::sse(THINKING_AND_TEXT)]).await;
        let provider = AnthropicProvider::new(&api_key("k"), &base).unwrap();
        let mut agent = Agent::new(Arc::new(provider), crate::agent::default_session())
            .with_clock(crate::clock::TickingClock::new());
        let (o1, _) = turn(&mut agent, "one", &[]).await;
        assert!(
            matches!(
                o1,
                TurnOutcome::Completed {
                    committed: false,
                    ..
                }
            ),
            "{o1:?}"
        );
        let _ = turn(&mut agent, "two", &[]).await;
        let reqs = seen.lock().unwrap();
        let second = request_json(&reqs[1]);
        assert_eq!(
            second["messages"],
            json!([{"role": "user", "content": [
                {"type": "text", "text": "Sent: Thursday 2026-10-22 00:00:00 UTC-04:00\n"},
                {"type": "text", "text": "Context: 166 of 1M tokens (0%). Wisp compacts at 850k.\n"},
                {"type": "text", "text": "two"},
            ]}])
        );
    }

    #[tokio::test]
    async fn a_gateway_503_after_a_full_read_is_one_attempt() {
        let envoy = "upstream connect error or disconnect/reset before headers. reset reason: connection termination";
        let r503 = http::response(
            "503 Service Unavailable",
            &[("content-type", "text/plain")],
            envoy,
        );
        let (base, seen) = mock_server(vec![r503; 5]).await;
        let provider = AnthropicProvider::new(&api_key("k"), &base).unwrap();
        let s = session(None, CacheTtl::OneHour);
        let items: Vec<_> = provider.stream(&hi_request(&s)).collect().await;
        let last = items.last().unwrap().as_ref().unwrap_err();
        assert_eq!(seen.lock().unwrap().len(), 1);
        assert!(last.possibly_billed);
        assert_eq!(last.detail.retry, RetryClass::Transient);
    }

    #[tokio::test]
    async fn every_failure_after_a_200_is_possibly_billed_and_only_transient_ones_are_retried() {
        let error_event = http::event(
            "error",
            r#"{"type":"error","error":{"type":"overloaded_error","message":"Overloaded"}}"#,
        );
        let cut = START.to_owned() + "event: content_block_start\ndata: {\"type\":\"content";
        for body in [GATEWAY_ERROR.to_owned(), error_event, cut] {
            let (base, seen) = mock_server(vec![http::sse(&body)]).await;
            let provider = AnthropicProvider::new(&api_key("k"), &base).unwrap();
            let s = session(None, CacheTtl::OneHour);
            let items: Vec<_> = provider.stream(&hi_request(&s)).collect().await;
            let Some(Err(e)) = items.last() else {
                panic!("{items:?}")
            };
            assert!(e.possibly_billed, "{body}: {}", e.message);
            assert_eq!(seen.lock().unwrap().len(), 1, "{body}");

            let transient = e.detail.retry != RetryClass::Never;
            let (base, seen) = mock_server(vec![
                http::sse(&body),
                http::sse(THINKING_AND_TEXT),
                http::sse(THINKING_AND_TEXT),
            ])
            .await;
            let provider = AnthropicProvider::new(&api_key("k"), &base).unwrap();
            let mut agent = Agent::new(Arc::new(provider), crate::agent::default_session())
                .with_retry_policy(crate::agent::retry::quick());
            let (first, _) = turn(&mut agent, "a", &[]).await;
            if transient {
                assert!(session_lower_bound(&first), "{body}: {first:?}");
                assert_eq!(agent.last_turn_stats().unwrap().retries, 1, "{body}");
                assert_eq!(seen.lock().unwrap().len(), 2, "{body}");
            } else {
                assert!(matches!(first, TurnOutcome::Error { .. }), "{first:?}");
                let (next, _) = turn(&mut agent, "b", &[]).await;
                assert!(session_lower_bound(&next), "{body}");
            }
        }
    }

    #[tokio::test]
    async fn an_interrupted_stream_never_reports_the_request_url() {
        let cut = format!(
            "HTTP/1.1 200 OK\r\ncontent-type: text/event-stream\r\ncontent-length: 100000\r\nconnection: close\r\n\r\n{START}"
        );
        let (base, _) = mock_server(vec![cut]).await;
        let base = base + "/CANARYpath?CANARYquery";
        let provider = AnthropicProvider::new(&api_key("k"), &base).unwrap();
        let s = session(None, CacheTtl::OneHour);
        let items: Vec<_> = provider.stream(&hi_request(&s)).collect().await;
        let Some(Err(e)) = items.last() else {
            panic!("{items:?}")
        };
        assert!(e.message.contains("stream interrupted"), "{}", e.message);
        assert!(e.possibly_billed);
        assert!(!e.message.contains("CANARY"), "{}", e.message);
        assert!(!e.message.contains("127.0.0.1"), "{}", e.message);
    }

    #[tokio::test]
    async fn stream_interruption_text_drops_a_url_the_error_carries() {
        let (_socket, addr) = refusing_addr();
        let url = format!("http://{addr}/CANARYpath?CANARYquery");
        let e = reqwest::Client::new().get(&url).send().await.unwrap_err();
        assert!(e.to_string().contains("CANARY"), "{e}");
        let error = interrupted(e);
        assert!(error.possibly_billed);
        assert!(!error.message.contains("CANARY"), "{}", error.message);
    }

    #[tokio::test]
    async fn cancel_during_retry_backoff_keeps_session_exact() {
        let rl = r#"{"type":"error","error":{"type":"rate_limit_error","message":"slow down"}}"#;
        let r429 = json_reply("429 Too Many Requests", &[("retry-after", "30")], rl);
        let (base, seen) = mock_server(vec![r429, http::sse(THINKING_AND_TEXT)]).await;
        let provider = AnthropicProvider::new(&api_key("k"), &base).unwrap();
        let mut agent = Agent::new(Arc::new(provider), crate::agent::default_session());
        let (cmd_tx, mut cmd_rx) = mpsc::channel(4);
        let (ev_tx, mut ev_rx) = mpsc::channel(256);
        let watcher = tokio::spawn(async move {
            while let Some(ev) = ev_rx.recv().await {
                if matches!(ev, AgentEvent::Retrying { .. }) {
                    cmd_tx.send(ClientCommand::Cancel).await.unwrap();
                }
            }
        });
        let o1 = agent
            .run_turn("a".into(), &mut cmd_rx, &ev_tx)
            .await
            .unwrap();
        drop(ev_tx);
        watcher.await.unwrap();
        assert!(
            matches!(
                o1,
                TurnOutcome::Error {
                    usage: None,
                    gave_up: None,
                    offer: Some(_),
                    ..
                }
            ),
            "{o1:?}"
        );
        assert_eq!(seen.lock().unwrap().len(), 1, "only the 429 was sent");
        let (o2, _) = turn(&mut agent, "b", &[]).await;
        assert!(!session_lower_bound(&o2), "nothing in flight was cancelled");
    }

    #[tokio::test]
    async fn cancel_before_any_request_keeps_session_exact() {
        let (base, seen) = mock_server(vec![http::sse(THINKING_AND_TEXT)]).await;
        let provider = AnthropicProvider::new(&api_key("k"), &base).unwrap();
        let mut agent = Agent::new(Arc::new(provider), crate::agent::default_session());
        let (cmd_tx, mut cmd_rx) = mpsc::channel(4);
        let (ev_tx, _ev_rx) = mpsc::channel(256);
        cmd_tx.send(ClientCommand::Cancel).await.unwrap();
        let o1 = agent
            .run_turn("a".into(), &mut cmd_rx, &ev_tx)
            .await
            .unwrap();
        assert!(matches!(o1, TurnOutcome::Cancelled { usage: None, .. }));
        assert_eq!(seen.lock().unwrap().len(), 0, "nothing was sent");
        let (o2, _) = turn(&mut agent, "b", &[]).await;
        assert!(!session_lower_bound(&o2), "nothing was sent");
    }

    #[tokio::test]
    async fn builder_error_is_not_possibly_billed() {
        let provider = AnthropicProvider::new(&api_key("k"), "localhost:8080").unwrap();
        let s = session(None, CacheTtl::OneHour);
        let items: Vec<_> = provider.stream(&hi_request(&s)).collect().await;
        let Some(Err(e)) = items.last() else {
            panic!("{items:?}")
        };
        assert!(!e.possibly_billed, "{}", e.message);
        assert!(!e.message.contains("after sending"), "{}", e.message);
        assert!(e.message.contains("check the base URL"), "{}", e.message);
        assert_eq!(items.len(), 2, "not retried: {items:?}");
    }

    #[tokio::test]
    async fn redirects_are_not_followed() {
        let (base, seen) = mock_server(vec![String::new(), String::new()]).await;
        let redirect = http::response(
            "307 Temporary Redirect",
            &[("location", &format!("{base}/v1/messages"))],
            "",
        );
        let (first, first_seen) = mock_server(vec![redirect]).await;
        let provider = AnthropicProvider::new(&api_key("sk-secret"), &first).unwrap();
        let s = session(None, CacheTtl::OneHour);
        let items: Vec<_> = provider.stream(&hi_request(&s)).collect().await;
        let Some(Err(e)) = items.last() else {
            panic!("{items:?}")
        };
        assert_eq!(e.kind, ErrorKind::InvalidResponse);
        assert_eq!(e.detail.status, Some(307));
        assert!(e.message.contains("redirected"), "{}", e.message);
        assert_eq!(first_seen.lock().unwrap().len(), 1);
        assert!(seen.lock().unwrap().is_empty(), "the redirect was followed");
    }
}

mod session_log {
    use super::*;
    use crate::agent::Agent;
    use crate::session::{JsonlStore, SessionLog, SessionStore, new_session_id};
    use wisp_protocol::log::CredentialSource;
    use wisp_protocol::{AgentEvent, ClientCommand};

    const CANARY_KEY: &str = "sk-ant-api03-CANARYkeyDoNotLog0123456789";
    const CANARY_PASS: &str = "CANARYpassDoNotLog";
    const CANARY_OTHER: &str = "CANARYotherDoNotLog";

    #[tokio::test]
    async fn canary_secrets_never_reach_the_log() {
        let rejected =
            r#"{"type":"error","error":{"type":"invalid_request_error","message":"bad"}}"#;
        let (base, seen) = mock_server(vec![
            http::sse(THINKING_AND_TEXT),
            http::json("400 Bad Request", rejected),
        ])
        .await;
        let base_with_userinfo = base.replace("http://", &format!("http://user:{CANARY_PASS}@"));
        let file = EnvFile {
            path: "/cfg/wisp/.env".into(),
            vars: [
                ("ANTHROPIC_API_KEY", CANARY_KEY),
                ("ANTHROPIC_BASE_URL", base_with_userinfo.as_str()),
                ("OTHER_SECRET", CANARY_OTHER),
            ]
            .iter()
            .map(|(k, v)| ((*k).to_owned(), (*v).to_owned()))
            .collect(),
        };
        let settings = Settings::resolve(|_| None, Some(&file)).unwrap();
        let context = settings.log_context();
        assert_eq!(context.credential_source, CredentialSource::EnvFile);
        assert_eq!(context.endpoint, base);

        let dir = std::env::temp_dir().join(format!("wisp-canary-{}", new_session_id()));
        let store = JsonlStore::open(&dir).unwrap();
        let id = new_session_id();
        let path = store.path(&id);
        let provider = AnthropicProvider::from_settings(&settings).unwrap();
        let agent = Agent::new(Arc::new(provider), crate::agent::default_session())
            .with_log(SessionLog::new(Box::new(store), id.clone()), context);
        let (commands, mut events, _task) = crate::agent::spawn(agent);
        for text in ["one", "two"] {
            commands
                .send(ClientCommand::send(text, text))
                .await
                .unwrap();
            until_turn_end(&mut events, &mut Vec::new()).await;
        }
        drop(commands);
        while events.recv().await.is_some() {}

        let requests = seen.lock().unwrap();
        assert!(requests[0].contains(CANARY_KEY));
        let log = std::fs::read_to_string(&path).unwrap();
        for canary in [CANARY_KEY, CANARY_PASS, CANARY_OTHER, "x-api-key", "CANARY"] {
            assert!(!log.contains(canary), "{canary} leaked into the log");
        }
        let records = JsonlStore::open(&dir).unwrap().read(&id).unwrap();
        assert!(records.len() >= 7, "{records:?}");
        std::fs::remove_dir_all(&dir).unwrap();
    }

    fn files_under(dir: &std::path::Path) -> Vec<std::path::PathBuf> {
        let mut files = Vec::new();
        let mut dirs = vec![dir.to_path_buf()];
        while let Some(dir) = dirs.pop() {
            for entry in std::fs::read_dir(&dir).unwrap() {
                let path = entry.unwrap().path();
                if path.is_dir() {
                    dirs.push(path);
                } else {
                    files.push(path);
                }
            }
        }
        files.sort();
        files
    }

    #[tokio::test]
    async fn canary_secrets_never_reach_the_log_or_the_saved_images() {
        use crate::test_guard::RED_PIXEL as pixel;
        let (base, seen) = mock_server(vec![http::sse(THINKING_AND_TEXT)]).await;
        let base_with_userinfo = base.replace("http://", &format!("http://user:{CANARY_PASS}@"));
        let file = EnvFile {
            path: "/cfg/wisp/.env".into(),
            vars: [
                ("ANTHROPIC_API_KEY", CANARY_KEY),
                ("ANTHROPIC_BASE_URL", base_with_userinfo.as_str()),
                ("OTHER_SECRET", CANARY_OTHER),
            ]
            .iter()
            .map(|(k, v)| ((*k).to_owned(), (*v).to_owned()))
            .collect(),
        };
        let settings = Settings::resolve(|_| None, Some(&file)).unwrap();
        let context = settings.log_context();
        let dir = std::env::temp_dir().join(format!("wisp-canary-images-{}", new_session_id()));
        let store = JsonlStore::open(dir.join("sessions"))
            .unwrap()
            .with_attachments_dir(dir.join("attachments"));
        let id = new_session_id();
        let provider = AnthropicProvider::from_settings(&settings).unwrap();
        let agent = Agent::new(Arc::new(provider), crate::agent::default_session())
            .with_log(SessionLog::new(Box::new(store), id.clone()), context);
        let (commands, mut events, _task) = crate::agent::spawn(agent);
        commands
            .send(ClientCommand::Send {
                id: wisp_protocol::InputId("look".into()),
                text: "look".into(),
                images: vec![wisp_protocol::ImageData { data: pixel.into() }],
                mode: wisp_protocol::DeliveryMode::Steer,
                turn: None,
                replaces: None,
            })
            .await
            .unwrap();
        let ended = until_turn_end(&mut events, &mut Vec::new()).await;
        assert!(
            matches!(
                &ended,
                AgentEvent::TurnEnded {
                    outcome: wisp_protocol::TurnOutcome::Completed { .. },
                    ..
                }
            ),
            "{ended:?}"
        );
        drop(commands);
        while events.recv().await.is_some() {}

        let requests = seen.lock().unwrap();
        assert!(requests[0].contains(CANARY_KEY));
        assert!(requests[0].contains(r#""type":"image""#));
        let files = files_under(&dir);
        let images: Vec<_> = files
            .iter()
            .filter(|f| f.parent() == Some(dir.join("attachments").as_path()))
            .collect();
        assert_eq!(images.len(), 1, "{files:?}");
        assert!(images[0].to_string_lossy().ends_with(".png"));
        let spend = crate::spend::folder(&dir.join("sessions"));
        assert!(
            files
                .iter()
                .any(|path| path.parent() == Some(spend.as_path()))
        );
        assert_eq!(files.len(), 3, "{files:?}");
        for path in &files {
            let bytes = std::fs::read(path).unwrap();
            for canary in [CANARY_KEY, CANARY_PASS, CANARY_OTHER, "x-api-key", "CANARY"] {
                assert!(
                    !bytes.windows(canary.len()).any(|w| w == canary.as_bytes()),
                    "{canary} leaked into {}",
                    path.display()
                );
            }
        }
        std::fs::remove_dir_all(&dir).unwrap();
    }
}

mod log_redaction {
    use super::*;
    use crate::agent::Agent;
    use crate::session::{JsonlStore, SessionLog, new_session_id};
    use wisp_protocol::{AgentEvent, ClientCommand};

    const SECRETS: [&str; 4] = [
        "CANARYpathsecret",
        "CANARYquerysecret",
        "CANARYpass",
        "sk-ant-CANARYkey-0123456789",
    ];

    async fn run_logged(base_url: &str, cancel_on_retry: bool) -> (String, Vec<AgentEvent>) {
        let policy = if cancel_on_retry {
            crate::agent::retry::RetryPolicy::default()
        } else {
            crate::agent::retry::RetryPolicy {
                step_retries: Some(0),
                ..crate::agent::retry::RetryPolicy::default()
            }
        };
        let file = EnvFile {
            path: "/cfg/wisp/.env".into(),
            vars: [
                ("ANTHROPIC_API_KEY", SECRETS[3]),
                ("ANTHROPIC_BASE_URL", base_url),
            ]
            .iter()
            .map(|(k, v)| ((*k).to_owned(), (*v).to_owned()))
            .collect(),
        };
        let settings = Settings::resolve(|_| None, Some(&file)).unwrap();
        let context = settings.log_context();
        assert!(!format!("{context:?}").contains("CANARY"), "{context:?}");
        let dir = std::env::temp_dir().join(format!("wisp-log-redaction-{}", new_session_id()));
        let store = JsonlStore::open(&dir).unwrap();
        let id = new_session_id();
        let path = store.path(&id);
        let provider = AnthropicProvider::from_settings(&settings).unwrap();
        let agent = Agent::new(Arc::new(provider), crate::agent::default_session())
            .with_retry_policy(policy)
            .with_log(SessionLog::new(Box::new(store), id.clone()), context);
        let (commands, mut events, _task) = crate::agent::spawn(agent);
        commands
            .send(ClientCommand::send("one", "one"))
            .await
            .unwrap();
        let mut seen = Vec::new();
        while let Some(e) = events.recv().await {
            let retry = matches!(e, AgentEvent::Retrying { .. });
            let end = matches!(e, AgentEvent::TurnEnded { .. });
            seen.push(e);
            if retry && cancel_on_retry {
                commands.send(ClientCommand::Cancel).await.unwrap();
            }
            if end {
                break;
            }
        }
        drop(commands);
        while events.recv().await.is_some() {}
        let log = std::fs::read_to_string(&path).unwrap();
        std::fs::remove_dir_all(&dir).unwrap();
        (log, seen)
    }

    fn assert_clean(log: &str, events: &[AgentEvent]) {
        let shown = format!("{events:?}");
        for canary in SECRETS {
            assert!(!log.contains(canary), "{canary} in the log:\n{log}");
            assert!(!shown.contains(canary), "{canary} in an event: {shown}");
        }
    }

    #[tokio::test]
    async fn url_path_and_query_stay_out_of_error_records() {
        let (base, _seen) = mock_server(vec![String::new()]).await;
        let base = base.replace("http://", "http://user:CANARYpass@")
            + "/CANARYpathsecret/anthropic?key=CANARYquerysecret";
        let (log, events) = run_logged(&base, false).await;
        assert!(log.contains("\"turn_ended\""), "{log}");
        assert!(log.contains("connection failed after sending"), "{log}");
        assert_clean(&log, &events);
    }

    #[tokio::test]
    async fn an_error_object_in_a_200_is_logged_with_its_detail_and_without_the_canary() {
        let body = format!(
            r#"{{"error":{{"message":"key {} is not allowed","type":"server_error","status":502}},"request_id":"req_{}"}}"#,
            SECRETS[3], SECRETS[3]
        );
        let reply = http::json("200 OK", &body);
        let (base, _seen) = mock_server(vec![reply]).await;
        let (log, _events) = run_logged(&base, false).await;
        for canary in SECRETS {
            assert!(!log.contains(canary), "{canary} in the log:\n{log}");
        }
        let ended = log.lines().find(|l| l.contains("\"turn_ended\"")).unwrap();
        let record: Value = serde_json::from_str(ended).unwrap();
        let outcome = &record["outcome"];
        assert_eq!(outcome["kind"], "server", "{ended}");
        assert_eq!(outcome["detail"]["phase"], "response", "{ended}");
        assert_eq!(outcome["detail"]["source"], "endpoint", "{ended}");
        assert_eq!(outcome["detail"]["status"], 502, "{ended}");
        assert_eq!(outcome["detail"]["error_type"], "server_error", "{ended}");
        assert_eq!(outcome["detail"]["request_id"], "req_[redacted]", "{ended}");
        assert_eq!(
            record["schema_version"],
            wisp_protocol::log::LOG_SCHEMA_VERSION,
            "{ended}"
        );
    }

    #[tokio::test]
    async fn url_stays_out_of_retrying_records() {
        let (_socket, addr) = refusing_addr();
        let base = format!("http://{addr}/CANARYpathsecret?CANARYquerysecret");
        let (log, events) = run_logged(&base, true).await;
        let retry_line = log.lines().find(|l| l.contains("\"retrying\"")).unwrap();
        assert!(retry_line.contains("could not connect"), "{retry_line}");
        assert_clean(&log, &events);
    }

    #[test]
    fn log_context_redacts_the_credential_and_url_parts() {
        let file = EnvFile {
            path: "/x".into(),
            vars: [
                ("ANTHROPIC_AUTH_TOKEN", SECRETS[3]),
                (
                    "ANTHROPIC_BASE_URL",
                    "https://u:CANARYpass@gw.example/CANARYpathsecret?k=CANARYquerysecret",
                ),
            ]
            .iter()
            .map(|(k, v)| ((*k).to_owned(), (*v).to_owned()))
            .collect(),
        };
        let context = Settings::resolve(|_| None, Some(&file))
            .unwrap()
            .log_context();
        assert_eq!(context.endpoint, "https://gw.example");
        let scrubbed = context.redactions.scrub(&format!(
            "error for url (https://u:CANARYpass@gw.example/CANARYpathsecret?k=CANARYquerysecret) with {}",
            SECRETS[3]
        ));
        for canary in SECRETS {
            assert!(!scrubbed.contains(canary), "{scrubbed}");
        }
        let s = Settings::resolve(env_of(&[("ANTHROPIC_API_KEY", SECRETS[3])]), None).unwrap();
        let context = s.log_context();
        assert_eq!(
            context.redactions.scrub("https://api.anthropic.com"),
            "https://api.anthropic.com"
        );
    }
}

fn tool_specs() -> Vec<crate::tool::ToolSpec> {
    ["alpha", "beta"]
        .map(|name| crate::tool::ToolSpec {
            description: format!("The {name} tool."),
            input_schema: json!({"type": "object", "properties": {}}),
            name: name.into(),
        })
        .to_vec()
}

fn body_with_tools(system: Option<&str>) -> serde_json::Value {
    let session = session(system, CacheTtl::FiveMinutes);
    let tools = tool_specs();
    let messages = [Message::user_text("hi")];
    let request = Request {
        session: &session,
        tools: &tools,
        deferred: &[],
        by_value: &[],
        server_tools: &[],
        messages: &messages,
        previous_response_id: None,
        features: Features::default(),
        options: Default::default(),
        rendering: &crate::provider::UNRENDERED,
        compaction: None,
    };
    body_of(&request)
}

#[test]
fn without_a_system_prompt_the_last_tool_carries_the_1h_breakpoint() {
    let v = body_with_tools(None);
    assert!(v.get("system").is_none());
    let tools = v["tools"].as_array().unwrap();
    assert_eq!(tools.len(), 2);
    assert!(tools[0].get("cache_control").is_none());
    assert_eq!(
        tools[1]["cache_control"],
        json!({"type": "ephemeral", "ttl": "1h"})
    );
}

#[test]
fn with_a_system_prompt_the_breakpoint_stays_on_the_system() {
    let v = body_with_tools(Some("You are Wisp."));
    let tools = v["tools"].as_array().unwrap();
    assert!(tools.iter().all(|t| t.get("cache_control").is_none()));
    assert_eq!(
        v["system"][0]["cache_control"],
        json!({"type": "ephemeral", "ttl": "1h"})
    );
}

fn body_with_instructions(
    system: Option<&str>,
    instructions: Option<&str>,
    rendering: &crate::provider::Rendering,
) -> serde_json::Value {
    let mut session = session(system, CacheTtl::FiveMinutes);
    session.instructions = instructions.map(str::to_owned);
    let tools = tool_specs();
    let messages = [Message::user_text("hi")];
    let request = Request {
        session: &session,
        tools: &tools,
        deferred: &[],
        by_value: &[],
        server_tools: &[],
        messages: &messages,
        previous_response_id: None,
        features: Features::default(),
        options: Default::default(),
        rendering,
        compaction: None,
    };
    body_of(&request)
}

#[test]
fn the_project_block_follows_the_base_block_with_its_own_1h_breakpoint() {
    let hour = json!({"type": "ephemeral", "ttl": "1h"});
    let unrendered = &crate::provider::UNRENDERED;
    let v = body_with_instructions(Some("You are Wisp."), Some("<instructions>"), unrendered);
    assert_eq!(
        v["system"],
        json!([
            {"type": "text", "text": "You are Wisp.", "cache_control": hour},
            {"type": "text", "text": "<instructions>", "cache_control": hour}
        ])
    );
    assert!(
        v["tools"]
            .as_array()
            .unwrap()
            .iter()
            .all(|t| t.get("cache_control").is_none())
    );
    let alone = body_with_instructions(None, Some("<instructions>"), unrendered);
    assert_eq!(
        alone["system"],
        json!([{"type": "text", "text": "<instructions>", "cache_control": hour}])
    );
    assert!(
        alone["tools"]
            .as_array()
            .unwrap()
            .iter()
            .all(|t| t.get("cache_control").is_none()),
        "the project block, not the last tool, holds the breakpoint"
    );
    let neither = body_with_instructions(None, None, unrendered);
    assert!(neither.get("system").is_none());
    assert_eq!(neither["tools"][1]["cache_control"], hour);
    let dropped = crate::provider::Rendering {
        drop_base_marker: true,
        ..crate::provider::Rendering::default()
    };
    let without_base = body_with_instructions(Some("You are Wisp."), Some("<i>"), &dropped);
    assert!(without_base["system"][0].get("cache_control").is_none());
    assert_eq!(without_base["system"][1]["cache_control"], hour);
}

#[test]
fn a_session_without_instructions_sends_the_same_bytes_as_before() {
    let v = body_with_instructions(Some("You are Wisp."), None, &crate::provider::UNRENDERED);
    assert_eq!(v, body_with_tools(Some("You are Wisp.")));
    assert_eq!(v["system"].as_array().unwrap().len(), 1);
}

#[test]
fn the_gateway_host_keeps_only_the_host_and_a_non_default_port() {
    let at = |base_url: &str| Settings {
        credential: Credential::AuthToken("tok".into()),
        base_url: base_url.to_owned(),
        source: Source::Environment,
    };
    assert_eq!(
        at("https://user:secret@gw.example.com:8443/v1?key=x")
            .gateway_host()
            .as_deref(),
        Some("gw.example.com:8443")
    );
    assert_eq!(
        at("https://GW.Example.COM/anthropic#frag")
            .gateway_host()
            .as_deref(),
        Some("gw.example.com")
    );
    assert_eq!(
        at("https://gw.example.com:443/").gateway_host().as_deref(),
        Some("gw.example.com")
    );
    assert_eq!(
        at("http://localhost:8080/").gateway_host().as_deref(),
        Some("localhost:8080")
    );
    assert_eq!(
        at("http://[::1]:9000").gateway_host().as_deref(),
        Some("[::1]:9000")
    );
    assert_eq!(at(DEFAULT_BASE_URL).gateway_host(), None);
    assert_eq!(at("https://api.anthropic.com/").gateway_host(), None);
    assert_eq!(at("not a url with secret").gateway_host(), None);
    for url in [
        "https://user:secret@gw.example.com:8443/v1?key=x",
        "https://secret@gw.example.com/path-secret?q=secret",
    ] {
        let host = at(url).gateway_host().unwrap_or_default();
        assert!(!host.contains("secret") && !host.contains("user"), "{host}");
        assert!(
            !host.contains('/') && !host.contains('?') && !host.contains('@'),
            "{host}"
        );
    }
}

async fn probe_with(reply: http::Reply) -> (CheckOutcome, String) {
    let server = http::serve(vec![reply]).await;
    let provider = AnthropicProvider::new(
        &Credential::AuthToken("tok-CANARYPROBE".into()),
        &server.url,
    )
    .unwrap();
    let outcome = provider
        .count_tokens_probe("claude-sonnet-5-5", Duration::from_millis(300))
        .await;
    (outcome, server.requests.lock().unwrap().concat())
}

#[tokio::test]
async fn the_probe_sends_one_count_tokens_request_without_betas() {
    let (outcome, request) = probe_with(http::Reply::Send(http::json(
        "200 OK",
        r#"{"input_tokens": 3}"#,
    )))
    .await;
    assert_eq!(outcome, CheckOutcome::Ok);
    let lower = request.to_ascii_lowercase();
    assert!(
        lower.starts_with("post /v1/messages/count_tokens "),
        "{request}"
    );
    assert!(
        lower.contains("authorization: bearer tok-canaryprobe"),
        "{request}"
    );
    assert!(lower.contains("anthropic-version: 2023-06-01"), "{request}");
    assert!(
        lower.contains("content-type: application/json"),
        "{request}"
    );
    assert!(!lower.contains("anthropic-beta"), "{request}");
    assert!(!lower.contains("user-agent"), "{request}");
    let body: Value = serde_json::from_str(http::body(&request)).unwrap();
    assert_eq!(
        body,
        json!({"model": "claude-sonnet-5-5", "messages": [{"role": "user", "content": "ping"}]})
    );
}

#[tokio::test]
async fn probe_answers_map_to_wisp_written_outcomes() {
    let chunk =
        "data: {\"error\":{\"message\":\"CANARYSECRET quota\",\"type\":\"server_error\"}}\n\n";
    let cases = [
        (
            http::json("200 OK", r#"{"input_tokens": "3"}"#),
            "unexpected answer (HTTP 200): no input_tokens count in the body",
        ),
        (
            http::json("200 OK", "{}"),
            "unexpected answer (HTTP 200): no input_tokens count in the body",
        ),
        (
            http::json("200 OK", "<html>CANARYSECRET</html>"),
            "unexpected answer (HTTP 200): the body is not JSON",
        ),
        (
            http::json("200 OK", chunk),
            "unexpected answer (HTTP 200): the body reports an error",
        ),
        (
            http::json(
                "200 OK",
                r#"{"error":{"message":"CANARYSECRET"},"input_tokens":3}"#,
            ),
            "unexpected answer (HTTP 200): the body reports an error",
        ),
        (
            http::json("200 OK", r#"{"input_tokens":3,"type":"error"}"#),
            "unexpected answer (HTTP 200): the body reports an error",
        ),
        (
            http::json("200 OK", r#"{"type":"error"}"#),
            "unexpected answer (HTTP 200): the body reports an error",
        ),
        (
            http::json("200 OK", r#"{"input_tokens": -1}"#),
            "unexpected answer (HTTP 200): no input_tokens count in the body",
        ),
        (
            http::json("401 Unauthorized", r#"{"error":"CANARYSECRET"}"#),
            "key refused (401)",
        ),
        (
            http::json("403 Forbidden", "CANARYSECRET"),
            "forbidden (403)",
        ),
        (
            http::json("404 Not Found", "CANARYSECRET"),
            "count_tokens not offered here (404): couldn't check this account",
        ),
        (
            http::json("500 Internal Server Error", "CANARYSECRET"),
            "unexpected answer (HTTP 500)",
        ),
        (
            http::response(
                "302 Found",
                &[("location", "http://127.0.0.1:9/CANARYSECRET")],
                "",
            ),
            "unexpected answer (HTTP 302): a redirect, not followed",
        ),
    ];
    for (response, want) in cases {
        let (outcome, _) = probe_with(http::Reply::Send(response)).await;
        let shown = format!("{outcome} {outcome:?}");
        assert_eq!(outcome.to_string(), want);
        assert!(!shown.contains("CANARY"), "{shown}");
    }
}

#[tokio::test]
async fn a_hanging_or_absent_server_is_unreachable() {
    let (outcome, _) = probe_with(http::Reply::Hang).await;
    assert_eq!(outcome.to_string(), "unreachable: timed out");
    let (_socket, addr) = refusing_addr();
    let base = format!("http://{addr}");
    let provider = AnthropicProvider::new(&api_key("k"), &base).unwrap();
    let outcome = provider
        .count_tokens_probe("m", Duration::from_secs(5))
        .await;
    assert_eq!(outcome.to_string(), "unreachable: could not connect");
}

#[tokio::test]
async fn an_endless_body_stops_at_the_read_limit_long_before_the_timeout() {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base = format!("http://{}", listener.local_addr().unwrap());
    let server = tokio::spawn(async move {
        let (mut socket, _) = listener.accept().await.unwrap();
        let mut buf = [0u8; 4096];
        let _ = socket.read(&mut buf).await;
        let head = "HTTP/1.1 200 OK\r\ncontent-type: application/json\r\nconnection: close\r\n\r\n{\"input_tokens\": 3, \"pad\": \"";
        if socket.write_all(head.as_bytes()).await.is_err() {
            return;
        }
        let chunk = vec![b'x'; 8192];
        while socket.write_all(&chunk).await.is_ok() {}
    });
    let provider = AnthropicProvider::new(&api_key("k"), &base).unwrap();
    let start = std::time::Instant::now();
    let outcome = provider
        .count_tokens_probe("m", Duration::from_secs(10))
        .await;
    assert!(
        start.elapsed() < Duration::from_secs(3),
        "{:?}",
        start.elapsed()
    );
    assert_eq!(
        outcome.to_string(),
        "unexpected answer (HTTP 200): the body is too large"
    );
    server.abort();
}

#[tokio::test]
async fn a_status_answer_is_labelled_before_any_body_is_read() {
    for (status, want) in [
        ("401 Unauthorized", "key refused (401)"),
        ("403 Forbidden", "forbidden (403)"),
        (
            "404 Not Found",
            "count_tokens not offered here (404): couldn't check this account",
        ),
        ("500 Internal Server Error", "unexpected answer (HTTP 500)"),
    ] {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let base = format!("http://{}", listener.local_addr().unwrap());
        let head = format!(
            "HTTP/1.1 {status}\r\ncontent-type: application/json\r\nconnection: close\r\n\r\n"
        );
        let server = tokio::spawn(async move {
            let (mut socket, _) = listener.accept().await.unwrap();
            let mut buf = [0u8; 4096];
            let _ = socket.read(&mut buf).await;
            if socket.write_all(head.as_bytes()).await.is_err() {
                return;
            }
            let chunk = vec![b'x'; 8192];
            while socket.write_all(&chunk).await.is_ok() {}
        });
        let provider = AnthropicProvider::new(&api_key("k"), &base).unwrap();
        let outcome = provider
            .count_tokens_probe("m", Duration::from_secs(10))
            .await;
        assert_eq!(outcome.to_string(), want);
        server.abort();
    }
}

#[test]
fn binds_thinking_matches_only_the_documented_binding_sentence() {
    let cases = [
        (
            "messages.5.content.0: Invalid `signature` in `thinking` block. The block is bound to a different conversation. Remove the block, or set `thinking.block_binding.prefix_mismatch_behavior` to `drop_block` with the thinking-binding-controls-2026-08-01 beta header.",
            true,
        ),
        (
            "Invalid `signature` in `thinking` block. The block is bound to a different conversation.",
            true,
        ),
        (
            "messages.5.content.0: Invalid `signature` in `thinking` block",
            true,
        ),
        (
            "messages.5.content.0: Invalid `signature` in `thinking` block.",
            true,
        ),
        (
            "messages.1.content.0: Invalid `data` in `redacted_thinking` block",
            true,
        ),
        (
            "messages.1.content.0: xInvalid `data` in `redacted_thinking` block",
            false,
        ),
        (
            "messages.1.content.0: Invalid `data` in `thinking` block",
            false,
        ),
        (
            "messages.5.content.0: xInvalid `signature` in `thinking` block. The block is bound to a different conversation.",
            false,
        ),
        (
            "xInvalid `signature` in `thinking` block. The block is bound to a different conversation.",
            false,
        ),
        ("", false),
    ];
    for (message, expected) in cases {
        assert_eq!(binds_thinking(message), expected, "{message}");
    }
}

fn count_request<'a>(s: &'a SessionConfig, messages: &'a [Message]) -> Request<'a> {
    Request {
        session: s,
        tools: &[],
        deferred: &[],
        by_value: &[],
        server_tools: &[],
        messages,
        previous_response_id: None,
        features: Features {
            per_message_effort: Some(crate::provider::FeatureUse::Established),
            binding_controls: Some(crate::provider::FeatureUse::Established),
            native_compaction: None,
            documented: false,
            inline_tools: None,
            deferred_tools: None,
            thread: None,
        },
        options: Default::default(),
        rendering: &crate::provider::UNRENDERED,
        compaction: None,
    }
}

fn counted_messages() -> Vec<Message> {
    vec![
        Message::user_text("hi"),
        Message::effort(wisp_protocol::EffortMarker {
            level: Effort::Low,
            rendered: true,
        }),
    ]
}

#[test]
fn the_count_body_keeps_only_what_count_tokens_accepts() {
    let s = session(Some("be brief"), CacheTtl::OneHour);
    let messages = counted_messages();
    let request = count_request(&s, &messages);
    let full: Value = body_of(&request);
    assert!(full.get("max_tokens").is_some());
    let features = Features {
        thread: Some(crate::provider::Thread::Continue { held: 0 }),
        ..request.features
    };
    let continued = Request {
        features,
        previous_response_id: Some("msg_1"),
        ..request
    };
    assert_eq!(count_body(&continued), count_body(&request));
    let body = String::from_utf8(count_body(&request).unwrap()).unwrap() + "\n";
    let path = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures/requests/count_tokens.json");
    if std::env::var_os("WISP_BLESS").is_some() {
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(&path, &body).unwrap();
    }
    assert_eq!(std::fs::read_to_string(&path).unwrap(), body);
    let value: Value = serde_json::from_str(&body).unwrap();
    for key in [
        "max_tokens",
        "stream",
        "fallbacks",
        "diagnostics",
        "cache_control",
    ] {
        assert!(value.get(key).is_none(), "{key}: {body}");
    }
    assert!(!body.contains("cache_control"), "{body}");
    assert!(!body.contains("block_binding"), "{body}");
    assert!(!body.contains("\"system\",\"content\""), "{body}");
    let roles: Vec<&str> = value["messages"]
        .as_array()
        .unwrap()
        .iter()
        .map(|m| m["role"].as_str().unwrap())
        .collect();
    assert_eq!(roles, ["user"]);
    assert!(value.get("thinking").is_some());
    assert!(value.get("output_config").is_some());
}

async fn count_with(response: String) -> (TokenCount, String) {
    let (base, seen) = mock_server(vec![response]).await;
    let provider =
        AnthropicProvider::new(&Credential::AuthToken("tok-CANARYCOUNT".into()), &base).unwrap();
    let s = session(None, CacheTtl::OneHour);
    let messages = counted_messages();
    let count = provider.count_tokens(&count_request(&s, &messages)).await;
    (count, seen.lock().unwrap().concat())
}

#[tokio::test]
async fn the_count_posts_to_count_tokens_and_reads_input_tokens() {
    let (count, request) = count_with(http::json("200 OK", r#"{"input_tokens": 1234}"#)).await;
    assert_eq!(count, TokenCount::Counted { tokens: 1234 });
    let lower = request.to_ascii_lowercase();
    assert!(
        lower.starts_with("post /v1/messages/count_tokens "),
        "{request}"
    );
    assert!(!lower.contains("anthropic-beta"), "{request}");
    assert!(!lower.contains("\"stream\""), "{request}");
}

#[tokio::test]
async fn a_404_count_is_unsupported() {
    let (count, _) = count_with(http::json("404 Not Found", r#"{"error":"nope"}"#)).await;
    assert_eq!(count, TokenCount::Unsupported);
}

#[tokio::test]
async fn other_count_answers_fail_with_a_reason() {
    let (count, _) = count_with(http::json("500 Internal Server Error", "{}")).await;
    assert_eq!(
        count,
        TokenCount::Failed {
            reason: "HTTP 500".into()
        }
    );
    let (count, _) = count_with(http::json(
        "200 OK",
        r#"{"type":"error","error":{"type":"overloaded_error"}}"#,
    ))
    .await;
    assert_eq!(
        count,
        TokenCount::Failed {
            reason: "the server answered with an error".into()
        }
    );
}

fn started_with(list: &str) -> Vec<ProviderEvent> {
    let sse = http::event(
        "message_start",
        format!(
            r#"{{"type":"message_start","message":{{"id":"m","model":"claude-opus-5-5","usage":{{"input_tokens":1}},"input_transformations":{list}}}}}"#
        ),
    );
    let mut decoder = StreamDecoder::new("claude-opus-5-5", Features::default(), None);
    decoder
        .push(sse.as_bytes())
        .into_iter()
        .map(|item| item.unwrap())
        .collect()
}

#[test]
fn input_transformations_are_read_into_their_typed_form() {
    let events = started_with(
        r#"[{"type":"thinking_dropped","path":"messages.1.content.0","reason":"organization_binding_mismatch"},{"type":"later_kind","reason":"later_reason","extra":1}]"#,
    );
    let Some(ProviderEvent::InputTransformations {
        provider,
        transformations,
    }) = events.first()
    else {
        panic!("{events:?}")
    };
    assert_eq!(provider, "anthropic");
    assert_eq!(transformations.len(), 2);
    assert_eq!(transformations[0].kind, "thinking_dropped");
    assert_eq!(
        transformations[0].reason,
        Some(Some("organization_binding_mismatch".into()))
    );
    assert_eq!(
        transformations[0].path,
        Some(Some("messages.1.content.0".into()))
    );
    assert_eq!(transformations[1].kind, "later_kind");
    assert_eq!(transformations[1].path, None);
    assert_eq!(
        serde_json::to_value(transformations).unwrap(),
        json!([
            {"type": "thinking_dropped", "path": "messages.1.content.0", "reason": "organization_binding_mismatch"},
            {"type": "later_kind", "reason": "later_reason", "extra": 1}
        ])
    );
    assert!(matches!(
        started_with("[]").first(),
        Some(ProviderEvent::InputTransformations { transformations, .. }) if transformations.is_empty()
    ));
}

#[test]
fn input_transformations_of_an_unknown_shape_stay_unknown() {
    let events = started_with(r#"{"dropped":2}"#);
    assert_eq!(
        events.first(),
        Some(&ProviderEvent::Unknown {
            provider: "anthropic".into(),
            raw: json!({"type": "input_transformations", "input_transformations": {"dropped": 2}}),
        })
    );
}

#[test]
fn input_transformations_after_a_fallback_are_read_from_message_delta() {
    let list = r#"[{"type":"thinking_dropped","path":"messages.1.content.0","reason":"model_binding_mismatch"}]"#;
    for delta in [
        format!(
            r#"{{"type":"message_delta","delta":{{"stop_reason":"end_turn"}},"usage":{{"output_tokens":3}},"input_transformations":{list}}}"#
        ),
        format!(
            r#"{{"type":"message_delta","delta":{{"stop_reason":"end_turn","input_transformations":{list}}},"usage":{{"output_tokens":3}}}}"#
        ),
    ] {
        let sse = [
            http::event("message_start", r#"{"type":"message_start","message":{"id":"m","model":"claude-opus-5-5","usage":{"input_tokens":1}}}"#),
            http::event("message_delta", delta),
        ]
        .concat();
        let mut decoder = StreamDecoder::new("claude-opus-5-5", Features::default(), None);
        let events: Vec<ProviderEvent> = decoder
            .push(sse.as_bytes())
            .into_iter()
            .map(|item| item.unwrap())
            .collect();
        let lists: Vec<Value> = pick!(
            &events,
            ProviderEvent::InputTransformations {
                transformations, ..
            } => serde_json::to_value(transformations).unwrap()
        );
        assert_eq!(
            lists,
            [serde_json::from_str::<Value>(list).unwrap()],
            "{events:?}"
        );
        assert!(matches!(
            events.last(),
            Some(ProviderEvent::MessageDelta { .. })
        ));
    }
}

#[test]
fn input_transformations_keep_a_null_apart_from_an_absent_field() {
    let raw = json!([
        {"type": "thinking_dropped", "path": null, "reason": "model_binding_mismatch"},
        {"type": "thinking_dropped", "reason": null, "note": {"k": [1, null]}}
    ]);
    let events = started_with(&raw.to_string());
    let Some(ProviderEvent::InputTransformations {
        transformations, ..
    }) = events.first()
    else {
        panic!("{events:?}")
    };
    assert_eq!(transformations[0].path, Some(None));
    assert_eq!(transformations[1].path, None);
    assert_eq!(serde_json::to_value(transformations).unwrap(), raw);
}

#[test]
fn switched_openai_content_has_deterministic_text_without_native_secrets() {
    let content = vec![
        ContentBlock::opaque(
            "openai",
            serde_json::json!({"type":"wisp_native_item","projection_blocks":1,"identity":"private-account","item":{"type":"message","content":[{"type":"output_text","text":"Answer","annotations":[{"url":"https://example.org/source","title":"Source"}]}]}}),
        ),
        ContentBlock::Text {
            text: "Answer".into(),
        },
        ContentBlock::opaque(
            "openai",
            serde_json::json!({"type":"wisp_native_item","projection_blocks":0,"item":{"type":"web_search_call","action":{"sources":[{"url":"https://example.org/hit","title":"Hit"}]},"encrypted_content":"opaque-not-text"}}),
        ),
    ];
    let before = content.clone();
    let rendered = || {
        serde_json::to_string(&assistant_blocks(
            &content,
            0,
            &mut ServerCalls::default(),
            &Rendering::default(),
        ))
        .unwrap()
    };
    let text = rendered();
    for expected in ["Answer", "https://example.org/source"] {
        assert!(text.contains(expected));
    }
    assert!(!text.contains("https://example.org/hit"));
    assert!(!text.contains("private-account") && !text.contains("opaque-not-text"));
    assert_eq!(text, rendered());
    assert_eq!(content, before);
}

fn push_all(chunks: Vec<Vec<u8>>) -> (StreamDecoder, Vec<Result<ProviderEvent, ProviderError>>) {
    let mut decoder = StreamDecoder::new(
        "claude-opus-5-5",
        Features::default(),
        Some("req_cap".into()),
    );
    let mut items = Vec::new();
    for chunk in chunks {
        items.extend(decoder.push(&chunk));
    }
    (decoder, items)
}

fn assert_too_large(decoder: &StreamDecoder, items: &[Result<ProviderEvent, ProviderError>]) {
    let [earlier @ .., Err(error)] = items else {
        panic!("{items:?}")
    };
    assert!(earlier.iter().all(Result::is_ok), "{items:?}");
    let error = &after_200(error.clone(), decoder.request_id());
    assert_eq!(error.kind, ErrorKind::InvalidResponse);
    assert_eq!(error.message, "Anthropic event is too large");
    assert!(error.possibly_billed);
    assert_eq!(error.detail.phase, FailurePhase::Stream);
    assert_eq!(error.detail.retry, RetryClass::Never);
    assert_eq!(error.detail.status, Some(200));
    assert_eq!(error.detail.request_id.as_deref(), Some("req_cap"));
    assert!(!format!("{error:?}").contains("qqqq"));
    assert_eq!(billed_in_stream(error.clone()), *error);
    assert!(decoder.finish().is_none());
}

fn mebibytes(count: usize) -> Vec<Vec<u8>> {
    vec![vec![b'q'; 1024 * 1024]; count]
}

#[test]
fn bytes_without_a_newline_are_refused_past_the_event_cap() {
    let (decoder, items) = push_all(mebibytes(17));
    assert_eq!(items.len(), 1);
    assert_too_large(&decoder, &items);
}

#[test]
fn an_endless_sse_event_is_refused() {
    let start = "event: message_start\ndata: {\"type\":\"message_start\",\"message\":{\"id\":\"m\",\"model\":\"claude-opus-5-5\",\"usage\":{\"input_tokens\":1}}}\n\nevent: content_block_delta\ndata: {\"type\":\"content_block_delta\",\"index\":0,\"delta\":{\"type\":\"text_delta\",\"text\":\"";
    let mut chunks = vec![start.as_bytes().to_vec()];
    chunks.extend(mebibytes(20));
    let (decoder, items) = push_all(chunks);
    assert!(matches!(items[0], Ok(ProviderEvent::MessageStart { .. })));
    assert_too_large(&decoder, &items);
}

#[test]
fn data_lines_without_a_blank_line_are_refused_past_the_event_cap() {
    let line = format!("data: {}\n", "q".repeat(1024 * 1024 - 7)).into_bytes();
    let mut chunks = vec![b"event: content_block_delta\n".to_vec()];
    chunks.extend(vec![line; 17]);
    let (decoder, items) = push_all(chunks);
    assert_eq!(items.len(), 1);
    assert_too_large(&decoder, &items);
}

#[test]
fn an_anthropic_event_may_fill_the_cap_but_not_pass_it() {
    let mut decoder = StreamDecoder::new(
        "claude-opus-5-5",
        Features::default(),
        Some("req_cap".into()),
    );
    for chunk in mebibytes(16) {
        assert!(decoder.push(&chunk).is_empty());
    }
    let items = decoder.push(b"q");
    assert_eq!(items.len(), 1);
    assert_too_large(&decoder, &items);
}

fn tool(name: &str) -> ToolSpec {
    ToolSpec {
        description: format!("The {name} tool."),
        input_schema: json!({"type": "object", "properties": {}}),
        name: name.into(),
    }
}

#[test]
fn anthropic_encode_rejects_foreign_provider_models() {
    let session = SessionConfig {
        model: "gemini-3.1-pro-preview".into(),
        system: None,
        effort: Effort::Medium,
        cache_ttl: wisp_protocol::CacheTtl::OneHour,
        max_tokens: 8192,
        message_time: MessageTime::Off,
        instructions: None,
        context_note: false,
    };
    let messages = [Message::user_text("hi")];
    let rendering = Rendering::default();
    let req = Request {
        session: &session,
        tools: &[],
        deferred: &[],
        by_value: &[],
        server_tools: &[],
        messages: &messages,
        previous_response_id: None,
        features: Features::default(),
        options: RequestOptions::default(),
        rendering: &rendering,
        compaction: None,
    };
    let err = encode(&req, &layout(&req), marks_for(&req), Profile::Api, true).unwrap_err();
    assert_eq!(err.kind, ErrorKind::InvalidRequest);
    assert!(encode(&req, &layout(&req), marks_for(&req), Profile::Api, false).is_ok());
}
