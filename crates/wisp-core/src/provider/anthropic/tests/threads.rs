use serde_json::{Value, json};
use wisp_protocol::{
    CacheTtl, ContentBlock, Effort, EffortMarker, ErrorKind, Message, SessionConfig,
};

use super::classification::last_error_for;
use super::inline::gamma;
use super::subscription::{credential, local_provider};
use super::{api_key, conversation, hi_request, session};
use crate::agent::testing::body_of;
use crate::provider::anthropic::{
    AnthropicProvider, DEFAULT_BASE_URL, MESSAGE_THREADS_BETA, beta_header, build_body,
    reply_print, wire_print,
};
use crate::provider::credential::Credential;
use crate::provider::{
    CompactionAsk, Feature, FeatureUse, Features, Provider, ProviderError, Refused, Rendering,
    Request, RequestOptions, Thread, UNRENDERED,
};
use crate::test_guard::http;
use crate::tool::ToolSpec;

fn wisp_session() -> SessionConfig {
    SessionConfig {
        instructions: Some("Project rules.".into()),
        ..session(Some("You are Wisp."), CacheTtl::OneHour)
    }
}

fn tools() -> Vec<ToolSpec> {
    vec![ToolSpec {
        description: "The alpha tool.".into(),
        input_schema: json!({"type": "object", "properties": {}}),
        name: "alpha".into(),
    }]
}

fn history() -> Vec<Message> {
    let marker = EffortMarker {
        level: Effort::Low,
        rendered: true,
    };
    let mut history = conversation();
    history.push(Message::user_text("And spring?"));
    history.push(Message::turn_system(Some(marker), &[gamma()]).unwrap());
    history
}

fn threaded<'a>(
    session: &'a SessionConfig,
    tools: &'a [ToolSpec],
    messages: &'a [Message],
    thread: Option<Thread>,
) -> Request<'a> {
    Request {
        session,
        tools,
        deferred: &[],
        by_value: &[],
        server_tools: &[],
        messages,
        previous_response_id: Some("msg_held"),
        features: Features {
            thread,
            documented: true,
            ..Features::using(
                &[Feature::PerMessageEffort, Feature::InlineTools],
                FeatureUse::Established,
            )
        },
        options: RequestOptions::default(),
        rendering: &UNRENDERED,
        compaction: None,
    }
}

fn markers(body: &Value) -> usize {
    body.to_string().matches("\"cache_control\"").count()
}

#[test]
fn a_create_is_the_logical_body_with_the_thread_and_its_beta() {
    let (s, tools, history) = (wisp_session(), tools(), history());
    let plain = threaded(&s, &tools, &history, None);
    let create = threaded(&s, &tools, &history, Some(Thread::Create));
    let mut body = body_of(&create);
    assert_eq!(body["thread"], json!({"type": "create"}));
    assert!(markers(&body) <= 4, "{body}");
    body.as_object_mut().unwrap().remove("thread");
    assert_eq!(body, body_of(&plain));
    assert!(!beta_header(&plain).contains(MESSAGE_THREADS_BETA));
    assert_eq!(
        beta_header(&create),
        format!("{},{MESSAGE_THREADS_BETA}", beta_header(&plain))
    );
}

#[test]
fn a_continue_sends_only_the_new_messages_and_never_an_unheld_one() {
    let (s, tools, history) = (wisp_session(), tools(), history());
    let full = body_of(&threaded(&s, &tools, &history, Some(Thread::Create)));
    let held = 4;
    let request = threaded(&s, &tools, &history, Some(Thread::Continue { held }));
    let body = body_of(&request);
    assert_eq!(
        body["thread"],
        json!({"type": "continue", "previous_message_id": "msg_held"})
    );
    let messages = body["messages"].as_array().unwrap();
    assert_eq!(messages[..], full["messages"].as_array().unwrap()[held..]);
    let new = body["messages"].to_string();
    assert!(
        new.contains("\"output_config\":{\"effort\":\"low\"}"),
        "{new}"
    );
    assert!(new.contains("\"tool_addition\""), "{new}");
    let full_system = full["system"].to_string().matches("cache_control").count();
    assert_eq!(full_system + markers(&body), 3);
    let (mut rest, mut full_rest) = (body.clone(), full.clone());
    for key in ["messages", "thread", "system", "tools"] {
        rest.as_object_mut().unwrap().remove(key);
        full_rest.as_object_mut().unwrap().remove(key);
    }
    assert_eq!(rest, full_rest);
    assert!(body.get("system").is_none() && body.get("tools").is_none());
    let wire = wire_print(&request).unwrap().messages.len();
    for (held, previous) in [(wire, Some("msg_held")), (held, None)] {
        let request = Request {
            previous_response_id: previous,
            ..threaded(&s, &tools, &history, Some(Thread::Continue { held }))
        };
        let refused = build_body(&request).unwrap_err();
        assert_eq!(
            refused.refused,
            Some(Refused::Thread),
            "{held} {previous:?}"
        );
    }
}

#[test]
fn the_pinned_digest_moves_with_every_fixed_field_only() {
    let (s, tools, history) = (wisp_session(), tools(), history());
    let pinned = |request: &Request<'_>| wire_print(request).unwrap().pinned;
    let base = threaded(&s, &tools, &history[..5], None);
    let at_base = pinned(&base);
    for thread in [Thread::Create, Thread::Continue { held: 4 }] {
        let request = threaded(&s, &tools, &history[..5], Some(thread));
        assert_eq!(pinned(&request), at_base, "{thread:?}");
    }
    let marked = threaded(&s, &tools, &history, None);
    let longer = SessionConfig {
        max_tokens: 1,
        ..s.clone()
    };
    let stamped = Request {
        previous_response_id: Some("msg_other"),
        ..threaded(&longer, &tools, &history[..5], None)
    };
    assert_eq!(pinned(&marked), at_base);
    assert_eq!(pinned(&stamped), at_base);
    let effort = SessionConfig {
        effort: Effort::Low,
        ..s.clone()
    };
    let ttl = SessionConfig {
        cache_ttl: CacheTtl::FiveMinutes,
        ..s.clone()
    };
    let system = SessionConfig {
        system: Some("You are someone else.".into()),
        ..s.clone()
    };
    let trial = Features {
        inline_tools: Some(FeatureUse::Trial),
        ..base.features
    };
    let moved = [
        ("effort", threaded(&effort, &tools, &history[..5], None)),
        ("ttl", threaded(&ttl, &tools, &history[..5], None)),
        ("system", threaded(&system, &tools, &history[..5], None)),
        ("tools", threaded(&s, &[], &history[..5], None)),
        (
            "fallbacks",
            Request {
                options: RequestOptions {
                    refusal_fallback: false,
                    ..base.options
                },
                ..base
            },
        ),
        (
            "binding",
            Request {
                features: Features {
                    binding_controls: Some(FeatureUse::Established),
                    ..base.features
                },
                ..base
            },
        ),
    ];
    for (field, request) in moved {
        assert_ne!(pinned(&request), at_base, "{field}");
    }
    let without = Request {
        features: trial,
        ..base
    };
    let joining = Request {
        features: trial,
        ..marked
    };
    assert!(!beta_header(&without).contains("inline-tools"));
    assert!(beta_header(&joining).contains("inline-tools"));
    assert_ne!(pinned(&joining), pinned(&without));
}

#[test]
fn threads_need_the_real_origin_a_listed_model_and_a_turn_request() {
    let (s, tools, history) = (wisp_session(), tools(), history());
    let turn = threaded(&s, &tools, &history, None);
    let official = |credential: &Credential| AnthropicProvider::new(credential, DEFAULT_BASE_URL);
    for provider in [
        official(&api_key("k")),
        official(&credential("subscription-test-token")),
    ] {
        assert!(provider.unwrap().threads(&turn));
    }
    let gateway = AnthropicProvider::new(&api_key("k"), "https://gateway.example").unwrap();
    assert!(!gateway.threads(&turn));
    let mock = AnthropicProvider::new(&api_key("k"), "http://127.0.0.1:9").unwrap();
    assert!(!mock.on_official_origin().threads(&turn));
    let provider = official(&api_key("k")).unwrap();
    let alias = SessionConfig {
        model: "claude-opus-4-5".into(),
        ..s.clone()
    };
    let ask = CompactionAsk {
        native_instructions: None,
        tail: None,
    };
    let native = Features {
        native_compaction: Some(FeatureUse::Established),
        ..turn.features
    };
    for request in [
        threaded(&alias, &tools, &history, None),
        Request {
            compaction: Some(&ask),
            ..turn
        },
        Request {
            features: native,
            ..turn
        },
    ] {
        assert!(!provider.threads(&request));
    }
}

fn error_body(kind: &str, message: &str, code: Option<&str>) -> String {
    let details = code.map_or(Value::Null, |code| json!({"error_code": code}));
    json!({"type": "error", "error": {"type": kind, "message": message, "details": details}})
        .to_string()
}

async fn refused(thread: Option<Thread>, provisional: bool, response: String) -> ProviderError {
    let s = session(None, CacheTtl::OneHour);
    let messages = [Message::user_text("hi")];
    let rendering = Rendering {
        provisional,
        ..Rendering::default()
    };
    let request = Request {
        messages: &messages,
        previous_response_id: Some("msg_held"),
        rendering: &rendering,
        features: Features {
            thread,
            ..Features::default()
        },
        ..hi_request(&s)
    };
    last_error_for(
        |base| AnthropicProvider::new(&api_key("k"), base).unwrap(),
        &request,
        response,
    )
    .await
}

const CONTINUE: Option<Thread> = Some(Thread::Continue { held: 0 });
const CREATE: Option<Thread> = Some(Thread::Create);
const BAD: &str = "400 Bad Request";
const NOT_FOUND: &str = "404 Not Found";
const INVALID: &str = "invalid_request_error";

#[tokio::test]
async fn thread_errors_over_http_are_thread_refusals_and_others_are_not() {
    let unexpected =
        format!("Unexpected value(s) `{MESSAGE_THREADS_BETA}` for the `anthropic-beta` header");
    let mut cases = vec![
        (
            CREATE,
            BAD,
            error_body(INVALID, "thread: no system", None),
            None,
        ),
        (CREATE, BAD, error_body(INVALID, &unexpected, None), None),
        (
            CONTINUE,
            BAD,
            error_body(INVALID, "messages: bad", None),
            None,
        ),
        (
            CONTINUE,
            NOT_FOUND,
            error_body("not_found_error", "gone", Some("thread_not_found")),
            Some("thread_not_found"),
        ),
    ];
    for code in super::super::THREAD_CODES {
        cases.push((
            CREATE,
            BAD,
            error_body(INVALID, "m", Some(code)),
            Some(code),
        ));
    }
    for (thread, status, body, name) in cases {
        let error = refused(thread, true, http::json(status, &body)).await;
        assert_eq!(error.refused, Some(Refused::Thread), "{body}");
        assert!(error.rejected.is_empty(), "{body}");
        assert_eq!(error.detail.error_type.as_deref(), name.or(Some(INVALID)));
    }
    let not_found = error_body("not_found_error", "model: claude-x", None);
    let coded = error_body(INVALID, "m", Some("thread_not_found"));
    for (thread, status, body, kind) in [
        (CONTINUE, NOT_FOUND, not_found, ErrorKind::InvalidRequest),
        (
            CREATE,
            BAD,
            error_body(INVALID, "messages.0: bad", None),
            ErrorKind::InvalidRequest,
        ),
        (None, BAD, coded, ErrorKind::InvalidRequest),
        (
            CONTINUE,
            "429 Too Many Requests",
            error_body("rate_limit_error", "slow", None),
            ErrorKind::RateLimit,
        ),
        (
            CONTINUE,
            "529 Overloaded",
            error_body("overloaded_error", "busy", None),
            ErrorKind::Overloaded,
        ),
    ] {
        let error = refused(thread, false, http::json(status, &body)).await;
        assert_eq!(error.refused, None, "{body}");
        assert_eq!(error.kind, kind, "{body}");
    }
}

#[tokio::test]
async fn thread_errors_in_a_200_body_or_a_first_event_are_thread_refusals() {
    let not_found = error_body("not_found_error", "gone", Some("thread_not_found"));
    let uncoded = error_body(INVALID, "thread: no system", None);
    for body in [&not_found, &uncoded] {
        let event = http::event("error", body);
        for response in [http::json("200 OK", body), http::sse(&event)] {
            let error = refused(CONTINUE, false, response).await;
            assert_eq!(error.refused, Some(Refused::Thread), "{body}");
        }
        let unthreaded = refused(None, false, http::json("200 OK", body)).await;
        assert_eq!(unthreaded.refused, None, "{body}");
    }
    let mismatch = error_body(
        INVALID,
        "thread: these fields differ from the stored thread: `anthropic-beta`",
        Some("thread_fingerprint_mismatch"),
    );
    let s = session(None, CacheTtl::OneHour);
    let messages = [Message::user_text("hi")];
    let code = Some("thread_fingerprint_mismatch");
    for (thread, refused, rejected, name) in [
        (CONTINUE, Some(Refused::Thread), false, code),
        (None, None, true, Some(INVALID)),
    ] {
        let request = Request {
            messages: &messages,
            previous_response_id: Some("msg_held"),
            features: Features {
                thread,
                ..Features::trying(&[Feature::PerMessageEffort])
            },
            ..hi_request(&s)
        };
        let error = last_error_for(local_provider, &request, http::json(BAD, &mismatch)).await;
        assert_eq!(error.refused, refused);
        assert_eq!(error.rejected.is_empty(), !rejected);
        assert_eq!(error.detail.error_type.as_deref(), name);
    }
}

#[test]
fn the_reply_print_is_the_next_request_s_digest_of_that_reply() {
    let s = wisp_session();
    let history = conversation();
    let next = wire_print(&threaded(&s, &[], &history, None)).unwrap();
    assert_eq!(
        reply_print(&history[1].content),
        Some(next.messages[1].clone())
    );
    assert_eq!(
        AnthropicProvider::new(&api_key("k"), DEFAULT_BASE_URL)
            .unwrap()
            .reply_print(&history[3].content),
        Some(next.messages[3].clone())
    );
    let mut spaced = history.clone();
    spaced[1]
        .content
        .push(ContentBlock::Text { text: " ".into() });
    let next = wire_print(&threaded(&s, &[], &spaced, None)).unwrap();
    assert_ne!(
        reply_print(&spaced[1].content),
        Some(next.messages[1].clone())
    );
}
