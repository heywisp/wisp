use futures::StreamExt;
use serde_json::{Value, json};
use wisp_protocol::{CacheTtl, ContentBlock, ErrorKind, Message, Role, StopReason};

use super::{api_key, assistant, http, mock_server, run_fixture, session, split_body};
use crate::agent::testing::{body_of, text_of};
use crate::provider::anthropic::{
    AnthropicProvider, COMPACTION_BETA, ErrorForm, beta_header, rejected_features, sent_prefix,
};
use crate::provider::{
    CompactionAsk, Feature, FeatureUse, Features, Provider, Refused, Request, RequestOptions,
    TailMarker, UNRENDERED,
};

const SYSTEM: &str = "You are Wisp.";
const INSTRUCTIONS: &str = "Write a summary of the conversation so far.";

fn history() -> Vec<Message> {
    vec![
        Message::user_text("list the files"),
        assistant(vec![ContentBlock::Text {
            text: "Two files.".into(),
        }]),
        Message::user_text("and the tests?"),
        assistant(vec![ContentBlock::Text {
            text: "One test file.".into(),
        }]),
    ]
}

fn trial() -> Features {
    Features {
        native_compaction: Some(FeatureUse::Trial),
        ..Features::default()
    }
}

fn request<'a>(
    session: &'a wisp_protocol::SessionConfig,
    messages: &'a [Message],
    features: Features,
    ask: Option<&'a CompactionAsk>,
) -> Request<'a> {
    Request {
        session,
        tools: &[],
        deferred: &[],
        by_value: &[],
        server_tools: &[],
        messages,
        previous_response_id: None,
        features,
        options: RequestOptions {
            debug_cache: false,
            refusal_fallback: true,
            message_threads: true,
        },
        rendering: &UNRENDERED,
        compaction: ask,
    }
}

fn tail(messages: &[Message], view_len: usize) -> TailMarker {
    let session = session(Some(SYSTEM), CacheTtl::OneHour);
    let mut anchor = request(&session, &messages[..view_len], trial(), None);
    anchor.options = RequestOptions::default();
    TailMarker {
        view_len,
        ttl: CacheTtl::OneHour,
        prefix: sent_prefix(&anchor).unwrap_or_default(),
        last_blocks: None,
    }
}

fn markers(body: &Value) -> Vec<(usize, usize)> {
    let mut found = Vec::new();
    for (m, message) in body["messages"].as_array().unwrap().iter().enumerate() {
        for (b, block) in message["content"].as_array().unwrap().iter().enumerate() {
            if block.get("cache_control").is_some() {
                found.push((m, b));
            }
        }
    }
    found
}

#[test]
fn a_compaction_request_asks_for_a_summary_with_the_tail_marked() {
    let session = session(Some(SYSTEM), CacheTtl::OneHour);
    let messages = history();
    let ask = CompactionAsk {
        native_instructions: Some(INSTRUCTIONS.into()),
        tail: Some(tail(&messages, 2)),
    };
    let sent = request(&session, &messages, trial(), Some(&ask));
    let body = body_of(&sent);
    assert_eq!(
        body["compaction"],
        json!({"type": "summarize", "instructions": INSTRUCTIONS})
    );
    for absent in [
        "stop_sequences",
        "tool_choice",
        "fallbacks",
        "cache_control",
    ] {
        assert!(body.get(absent).is_none(), "{absent}: {body}");
    }
    assert_eq!(markers(&body), [(1, 0)]);
    assert_eq!(
        body["system"][0]["cache_control"],
        json!({"type": "ephemeral", "ttl": "1h"})
    );
    assert!(beta_header(&sent).contains(COMPACTION_BETA));
}

#[test]
fn without_a_tail_the_compaction_request_marks_no_message() {
    let session = session(Some(SYSTEM), CacheTtl::OneHour);
    let messages = history();
    let ask = CompactionAsk {
        native_instructions: Some(INSTRUCTIONS.into()),
        tail: None,
    };
    let body = body_of(&request(&session, &messages, trial(), Some(&ask)));
    assert!(markers(&body).is_empty(), "{body}");
    assert!(body.get("cache_control").is_none());
}

#[test]
fn the_beta_rides_only_on_requests_using_the_feature() {
    let session = session(Some(SYSTEM), CacheTtl::OneHour);
    let messages = history();
    let plain = request(&session, &messages, Features::default(), None);
    assert!(!beta_header(&plain).contains(COMPACTION_BETA));
    let body = body_of(&plain);
    assert!(body.get("compaction").is_none());
    assert_eq!(body["fallbacks"], "default");
    let carrying = request(&session, &messages, trial(), None);
    assert!(beta_header(&carrying).contains(COMPACTION_BETA));
    assert!(body_of(&carrying).get("compaction").is_none());
}

#[test]
fn a_head_block_is_sent_back_as_it_came() {
    let session = session(Some(SYSTEM), CacheTtl::OneHour);
    let raw = json!({"type": "compaction", "content": "So far: two files.", "signature": "c2ln"});
    let messages = vec![Message {
        role: Role::User,
        content: vec![
            ContentBlock::opaque("anthropic", raw.clone()),
            ContentBlock::Text {
                text:
                    "<compacted_context epoch=\"1\" turns=\"1\u{2013}2\">\n</compacted_context>\n"
                        .into(),
            },
            ContentBlock::Text {
                text: "next".into(),
            },
        ],
    }];
    let body = body_of(&request(&session, &messages, trial(), None));
    assert_eq!(body["messages"][0]["content"][0], raw);
    assert_eq!(body["messages"][0]["role"], "user");
}

#[test]
fn turns_after_a_head_keep_one_prefix_byte_for_byte() {
    let session = session(Some(SYSTEM), CacheTtl::OneHour);
    let raw = json!({"type": "compaction", "content": "So far: two files.", "signature": "c2ln"});
    let mut view = vec![Message {
        role: Role::User,
        content: vec![
            ContentBlock::opaque("anthropic", raw.clone()),
            ContentBlock::Text {
                text:
                    "<compacted_context epoch=\"1\" turns=\"1\u{2013}2\">\n</compacted_context>\n"
                        .into(),
            },
            ContentBlock::Text {
                text: "and the tests?".into(),
            },
        ],
    }];
    let mut bodies = Vec::new();
    for (reply, next) in [("One test file.", "run them"), ("They pass.", "thanks")] {
        let sent = request(&session, &view, trial(), None);
        assert!(beta_header(&sent).contains(COMPACTION_BETA));
        bodies.push(text_of(&sent));
        view.push(assistant(vec![ContentBlock::Text { text: reply.into() }]));
        view.push(Message::user_text(next));
    }
    bodies.push(text_of(&request(&session, &view, trial(), None)));
    for pair in bodies.windows(2) {
        let (system, messages) = split_body(&pair[0]);
        let (next_system, next_messages) = split_body(&pair[1]);
        assert_eq!(system, next_system);
        let open = messages.strip_suffix(']').unwrap();
        assert!(
            next_messages.starts_with(open),
            "\n{messages}\n{next_messages}"
        );
    }
    let last: Value = serde_json::from_str(&bodies[2]).unwrap();
    assert_eq!(last["messages"][0]["content"][0], raw);
    assert_eq!(
        last["messages"]
            .as_array()
            .unwrap()
            .iter()
            .flat_map(|m| m["content"].as_array().unwrap())
            .filter(|b| b["type"] == "compaction")
            .count(),
        1
    );
}

#[test]
fn a_rejected_compaction_beta_is_named_in_every_form() {
    let trials = [Feature::NativeCompaction];
    for message in [
        "compaction: Extra inputs are not permitted",
        "messages.0.content.0: unexpected content block type 'compaction'; expected one of text, image",
        "anthropic-beta: Unexpected value(s) `compact-2026-09-04` for the `anthropic-beta` header.",
    ] {
        for form in [ErrorForm::Http400, ErrorForm::Json] {
            assert_eq!(
                rejected_features(message, form, Features::trying(&trials)),
                vec![Feature::NativeCompaction],
                "{message} {form:?}"
            );
        }
    }
    assert!(
        rejected_features(
            "compaction: Extra inputs are not permitted",
            ErrorForm::Http400,
            Features::trying(&[Feature::PerMessageEffort])
        )
        .is_empty()
    );
    assert!(
        rejected_features(
            "messages.0.content.0: bad block",
            ErrorForm::Http400,
            Features::trying(&trials)
        )
        .is_empty()
    );
}

async fn error_for(response: String, features: Features) -> crate::provider::ProviderError {
    let (base, _) = mock_server(vec![response]).await;
    let provider = AnthropicProvider::new(&api_key("k"), &base).unwrap();
    let session = session(None, CacheTtl::OneHour);
    let messages = history();
    let sent = request(&session, &messages, features, None);
    let items: Vec<_> = provider.stream(&sent).collect().await;
    match items.last() {
        Some(Err(e)) => e.clone(),
        other => panic!("{other:?}"),
    }
}

#[tokio::test]
async fn a_named_block_error_refuses_the_block_and_not_the_beta() {
    for code in [
        "compaction_signature_invalid",
        "compaction_content_mismatch",
    ] {
        let body = json!({"type": "error", "error": {
            "type": "invalid_request_error",
            "message": "compaction: Extra inputs are not permitted",
            "details": {"error_code": code},
        }})
        .to_string();
        let e = error_for(http::json("400 Bad Request", &body), trial()).await;
        assert_eq!(e.kind, ErrorKind::InvalidRequest, "{code}");
        assert_eq!(e.refused, Some(Refused::CompactionBlock), "{code}");
        assert!(e.rejected.is_empty(), "{code}");
    }
    let other = json!({"type": "error", "error": {
        "type": "invalid_request_error",
        "message": "messages.0: bad block",
        "details": {"error_code": "something_else"},
    }})
    .to_string();
    let e = error_for(http::json("400 Bad Request", &other), trial()).await;
    assert_ne!(e.refused, Some(Refused::CompactionBlock));
}

#[tokio::test]
async fn a_gateway_block_refusal_without_details_refuses_the_block() {
    let body = |message: &str| {
        json!({"type": "error", "error": {
            "type": "invalid_request_error",
            "message": message,
        }})
        .to_string()
    };
    let coded = |message: &str| {
        json!({"type": "error", "error": {
            "type": "invalid_request_error",
            "code": "unsupported_parameter",
            "message": message,
        }})
        .to_string()
    };
    let refusal = "messages.0.content.0: The compaction block's signature is invalid.";
    for features in [trial(), Features::default()] {
        for body in [body(refusal), coded(refusal)] {
            let e = error_for(http::json("400 Bad Request", &body), features).await;
            assert_eq!(e.kind, ErrorKind::InvalidRequest);
            assert_eq!(
                e.refused,
                Some(Refused::CompactionBlock),
                "{features:?} {body}"
            );
            assert!(e.rejected.is_empty());
        }
    }
    let e = error_for(
        http::json("400 Bad Request", &body("messages.0: bad block")),
        Features::default(),
    )
    .await;
    assert_eq!(e.refused, None);
    let e = error_for(
        http::json(
            "400 Bad Request",
            &body("compaction: Extra inputs are not permitted"),
        ),
        trial(),
    )
    .await;
    assert_eq!(e.refused, None);
    assert_eq!(e.rejected, [Feature::NativeCompaction]);
}

const COMPACTED: &str = r#"event: message_start
data: {"type":"message_start","message":{"id":"msg_c1","model":"claude-opus-5-5","usage":{"input_tokens":9000,"output_tokens":1}}}

event: content_block_start
data: {"type":"content_block_start","index":0,"content_block":{"type":"compaction","content":"So far: two files."}}

event: content_block_stop
data: {"type":"content_block_stop","index":0}

event: message_delta
data: {"type":"message_delta","delta":{"stop_reason":"compaction"},"usage":{"output_tokens":300}}

event: message_stop
data: {"type":"message_stop"}

"#;

#[test]
fn a_whole_compaction_block_is_kept_opaque_with_its_stop() {
    let (_, message) = run_fixture(COMPACTED);
    let message = message.unwrap();
    assert_eq!(message.stop_reason, StopReason::Compaction);
    assert_eq!(
        message.content,
        [ContentBlock::opaque(
            "anthropic",
            json!({"type": "compaction", "content": "So far: two files."})
        )]
    );
}

#[test]
fn compaction_deltas_are_kept_beside_the_block_without_identity() {
    let streamed = COMPACTED.replace(
        "event: content_block_stop",
        &format!("{}event: content_block_stop", http::event("content_block_delta", r#"{"type":"content_block_delta","index":0,"delta":{"type":"compaction_delta","content":" More.","user_id":"user_PLACEHOLDER","email":"someone@example.com"}}"#)),
    );
    let (_, message) = run_fixture(&streamed);
    let message = message.unwrap();
    let [ContentBlock::Opaque { deltas, .. }] = message.content.as_slice() else {
        panic!("{:?}", message.content);
    };
    assert_eq!(
        deltas,
        &[json!({"type": "compaction_delta", "content": " More."})]
    );
}

#[tokio::test]
async fn compaction_unavailable_names_itself_on_an_overload_and_nowhere_else() {
    let unavailable = |kind: &str| {
        json!({"type": "error", "error": {
            "type": kind,
            "message": "Overloaded",
            "details": {"error_code": "compaction_unavailable"},
        }})
        .to_string()
    };
    let e = error_for(
        http::json("529 Overloaded", &unavailable("overloaded_error")),
        trial(),
    )
    .await;
    assert_eq!(e.kind, ErrorKind::Overloaded);
    assert_eq!(e.refused, Some(Refused::CompactionUnavailable));
    let start = http::event(
        "message_start",
        r#"{"type":"message_start","message":{"id":"m","model":"claude-opus-5-5","usage":{"input_tokens":1,"output_tokens":0}}}"#,
    );
    let streamed = http::sse(&format!(
        "{start}{}",
        http::event("error", unavailable("overloaded_error"))
    ));
    let e = error_for(streamed, trial()).await;
    assert_eq!(e.kind, ErrorKind::Overloaded);
    assert_eq!(e.refused, Some(Refused::CompactionUnavailable));
    let plain =
        json!({"type": "error", "error": {"type": "overloaded_error", "message": "Overloaded"}})
            .to_string();
    let e = error_for(http::json("529 Overloaded", &plain), trial()).await;
    assert_eq!(e.refused, None);
    let wrong_kind = error_for(
        http::json("400 Bad Request", &unavailable("invalid_request_error")),
        trial(),
    )
    .await;
    assert_ne!(wrong_kind.refused, Some(Refused::CompactionUnavailable));
}

#[test]
fn a_fork_request_sends_no_compaction_field_and_the_same_markers() {
    let session = session(Some(SYSTEM), CacheTtl::OneHour);
    let mut messages = history();
    messages.push(Message::user_text(INSTRUCTIONS));
    let ask = CompactionAsk {
        native_instructions: None,
        tail: Some(tail(&messages, 3)),
    };
    let fork = request(&session, &messages, Features::default(), Some(&ask));
    let body = body_of(&fork);
    for absent in [
        "compaction",
        "stop_sequences",
        "tool_choice",
        "fallbacks",
        "cache_control",
    ] {
        assert!(body.get(absent).is_none(), "{absent}: {body}");
    }
    assert_eq!(markers(&body), [(2, 0)]);
    assert!(!beta_header(&fork).contains(COMPACTION_BETA));
    let plain = body_of(&request(&session, &messages, Features::default(), None));
    let strip = |mut body: Value| {
        for key in ["fallbacks", "cache_control", "compaction"] {
            body.as_object_mut().unwrap().remove(key);
        }
        for message in body["messages"].as_array_mut().unwrap() {
            for block in message["content"].as_array_mut().unwrap() {
                block.as_object_mut().unwrap().remove("cache_control");
            }
        }
        body
    };
    assert_eq!(strip(body), strip(plain));
}
