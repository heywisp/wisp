use serde_json::{Value, json};
use wisp_protocol::log::{BlockPosition, Cut};
use wisp_protocol::{CacheTtl, ContentBlock, Message, Role};

use super::session;
use crate::agent::testing::{body_of, fallback_marker};
use crate::provider::anthropic::echoed_blocks;
use crate::provider::{Features, Rendering, Request, ServerCalls, ServerToolSpec};
use crate::test_guard::opaque;
use crate::test_guard::search_use;
use crate::test_guard::{assistant, thinking as typed_thinking};
use crate::tool::ToolSpec;

fn result(id: &str) -> Value {
    json!({"type": "web_search_tool_result", "tool_use_id": id, "content": [
        {"type": "web_search_result", "title": "Ripgrep", "url": "https://example.com/rg", "encrypted_content": "Eq1"},
        {"type": "web_search_result", "title": "Fd", "url": "https://example.com/fd", "encrypted_content": "Eq2"}
    ]})
}

fn thinking(text: &str) -> Value {
    json!({"type": "thinking", "thinking": text, "signature": format!("sig-{text}")})
}

fn request_body(
    messages: &[Message],
    tools: &[ToolSpec],
    server_tools: &[ServerToolSpec],
    rendering: &Rendering,
    system: Option<&str>,
) -> Value {
    let session = session(system, CacheTtl::OneHour);
    let request = Request {
        session: &session,
        tools,
        deferred: &[],
        by_value: &[],
        server_tools,
        messages,
        previous_response_id: None,
        features: Features::default(),
        options: Default::default(),
        rendering,
        compaction: None,
    };
    body_of(&request)
}

fn content(body: &Value, message: usize) -> Value {
    body["messages"][message]["content"].clone()
}

fn mixed_history() -> Vec<Message> {
    vec![
        Message::user_text("look it up"),
        assistant(vec![
            typed_thinking("before"),
            opaque(search_use("srvtoolu_1", "rust ripgrep")),
            typed_thinking("after"),
            ContentBlock::ToolUse {
                id: "toolu_1".into(),
                name: "bash".into(),
                input: json!({"command": "ls"}),
            },
        ]),
        Message {
            role: Role::User,
            content: vec![
                ContentBlock::ToolResult {
                    tool_use_id: "toolu_1".into(),
                    content: vec![ContentBlock::Text {
                        text: "a\nb".into(),
                    }],
                    is_error: false,
                },
                ContentBlock::Text {
                    text: "next turn".into(),
                },
            ],
        },
    ]
}

#[test]
fn a_closed_call_is_text_in_place_and_the_thinking_after_it_is_cut() {
    let history = mixed_history();
    let plain = request_body(&history, &[], &[], &Rendering::default(), Some("sys"));
    assert_eq!(
        content(&plain, 1)[1],
        search_use("srvtoolu_1", "rust ripgrep")
    );
    let rendering = Rendering {
        closed: ["srvtoolu_1".to_owned()].into(),
        cuts: vec![Cut {
            from: BlockPosition {
                message: 1,
                block: 1,
            },
            through: 2,
            tools: false,
        }],
        ..Rendering::default()
    };
    let rendered = request_body(&history, &[], &[], &rendering, Some("sys"));
    assert_eq!(
        content(&rendered, 1),
        json!([
            thinking("before"),
            {"type": "text", "text": "[web search for \"rust ripgrep\": the turn ended before its result arrived]"},
            {"type": "tool_use", "id": "toolu_1", "name": "bash", "input": {"command": "ls"}}
        ])
    );
    assert_eq!(content(&rendered, 0), content(&plain, 0));
    assert_eq!(content(&rendered, 2), content(&plain, 2));
    assert_eq!(content(&plain, 1)[0], thinking("before"));
    assert_eq!(content(&plain, 1)[2], thinking("after"));
    let again = request_body(&history, &[], &[], &rendering, Some("sys"));
    assert_eq!(
        serde_json::to_string(&again).unwrap(),
        serde_json::to_string(&rendered).unwrap()
    );
}

#[test]
fn a_cut_leaves_out_thinking_only_in_its_own_range() {
    let history = vec![
        Message::user_text("one"),
        assistant(vec![
            opaque(thinking("first")),
            ContentBlock::Text { text: "a".into() },
        ]),
        Message::user_text("two"),
        assistant(vec![
            ContentBlock::RedactedThinking {
                data: "Redacted==".into(),
            },
            typed_thinking("second"),
            ContentBlock::Text { text: "b".into() },
        ]),
        Message::user_text("three"),
        assistant(vec![
            typed_thinking("third"),
            ContentBlock::Text { text: "c".into() },
        ]),
        Message::user_text("four"),
    ];
    let rendering = Rendering {
        cuts: vec![Cut {
            from: BlockPosition {
                message: 1,
                block: 1,
            },
            through: 4,
            tools: false,
        }],
        ..Rendering::default()
    };
    let body = request_body(&history, &[], &[], &rendering, None);
    assert_eq!(content(&body, 1)[0], thinking("first"));
    assert_eq!(content(&body, 3), json!([{"type": "text", "text": "b"}]));
    assert_eq!(content(&body, 5)[0], thinking("third"));
}

#[test]
fn another_providers_thinking_is_never_sent_and_a_cut_drops_only_its_own_range() {
    let foreign = ContentBlock::opaque("openai", thinking("placeholder"));
    let history = vec![
        Message::user_text("one"),
        assistant(vec![
            foreign.clone(),
            opaque(thinking("cut")),
            ContentBlock::Text { text: "a".into() },
        ]),
        Message::user_text("two"),
        assistant(vec![
            foreign,
            opaque(thinking("kept")),
            ContentBlock::Text { text: "b".into() },
        ]),
        Message::user_text("three"),
    ];
    let rendering = Rendering {
        cuts: vec![Cut {
            from: BlockPosition {
                message: 1,
                block: 0,
            },
            through: 2,
            tools: false,
        }],
        ..Rendering::default()
    };
    let body = request_body(&history, &[], &[], &rendering, None);
    assert_eq!(content(&body, 1), json!([{"type": "text", "text": "a"}]));
    assert_eq!(
        content(&body, 3),
        json!([thinking("kept"), {"type": "text", "text": "b"}])
    );
}

#[test]
fn rule_r_renders_native_blocks_as_text_only_where_asked() {
    let cited = opaque(
        json!({"type": "text", "text": "ripgrep is fast.", "citations": [{
            "type": "web_search_result_location", "url": "https://example.com/rg", "title": "Ripgrep",
            "encrypted_index": "Ei", "cited_text": "fast"
        }]}),
    );
    let failed = opaque(
        json!({"type": "web_search_tool_result", "tool_use_id": "srvtoolu_2",
        "content": {"type": "web_search_tool_result_error", "error_code": "too_many_requests"}}),
    );
    let empty = opaque(
        json!({"type": "web_search_tool_result", "tool_use_id": "srvtoolu_3", "content": []}),
    );
    let history = vec![
        Message::user_text("look"),
        assistant(vec![
            opaque(search_use("srvtoolu_1", "rg")),
            opaque(result("srvtoolu_1")),
            cited.clone(),
            opaque(search_use("srvtoolu_2", "fd")),
            failed,
            opaque(search_use("srvtoolu_3", "none")),
            empty,
        ]),
        Message::user_text("again"),
        assistant(vec![
            opaque(search_use("srvtoolu_4", "kept")),
            opaque(result("srvtoolu_4")),
        ]),
        Message::user_text("more"),
    ];
    let rendering = Rendering {
        native_as_text: vec![false, true, false, false, false],
        ..Rendering::default()
    };
    let body = request_body(&history, &[], &[], &rendering, None);
    assert_eq!(
        content(&body, 1),
        json!([
            {"type": "text", "text": "[web search: \"rg\"]"},
            {"type": "text", "text": "1. Ripgrep \u{2014} https://example.com/rg\n2. Fd \u{2014} https://example.com/fd"},
            {"type": "text", "text": "ripgrep is fast."},
            {"type": "text", "text": "[web search: \"fd\"]"},
            {"type": "text", "text": "failed: too_many_requests"},
            {"type": "text", "text": "[web search: \"none\"]"},
            {"type": "text", "text": "no results"}
        ])
    );
    assert_eq!(content(&body, 3)[0], search_use("srvtoolu_4", "kept"));
    assert_eq!(content(&body, 3)[1], result("srvtoolu_4"));
}

#[test]
fn server_tools_join_the_client_tools_in_name_order() {
    let client = |name: &str| ToolSpec {
        name: name.into(),
        description: format!("{name} tool"),
        input_schema: json!({"type": "object"}),
    };
    let tools = [client("ask_user"), client("bash")];
    let server = [ServerToolSpec {
        name: "web_search".into(),
        kind: "web_search_20250305".into(),
    }];
    let history = [Message::user_text("hi")];
    let body = request_body(&history, &tools, &server, &Rendering::default(), None);
    let names: Vec<&str> = body["tools"]
        .as_array()
        .unwrap()
        .iter()
        .map(|t| t["name"].as_str().unwrap())
        .collect();
    assert_eq!(names, ["ask_user", "bash", "web_search"]);
    assert_eq!(
        body["tools"][2],
        json!({"cache_control": {"type": "ephemeral", "ttl": "1h"}, "name": "web_search", "type": "web_search_20250305"})
    );
    assert!(body["tools"][1].get("cache_control").is_none());
    let none = request_body(&history, &tools, &[], &Rendering::default(), None);
    assert_eq!(none["tools"][1]["cache_control"]["ttl"], "1h");
    assert_eq!(none["tools"].as_array().unwrap().len(), 2);
}

#[test]
fn dropping_the_base_marker_keeps_the_tail_marker() {
    let history = [Message::user_text("hi")];
    let kept = request_body(&history, &[], &[], &Rendering::default(), Some("sys"));
    assert_eq!(kept["system"][0]["cache_control"]["ttl"], "1h");
    let rendering = Rendering {
        drop_base_marker: true,
        ..Rendering::default()
    };
    let dropped = request_body(&history, &[], &[], &rendering, Some("sys"));
    assert!(dropped["system"][0].get("cache_control").is_none());
    assert_eq!(dropped["cache_control"], kept["cache_control"]);
}

#[test]
fn a_result_for_an_earlier_messages_call_survives_a_refusal_fallback() {
    let earlier: ServerCalls = ["srvtoolu_1".to_owned()].into();
    let reply = vec![
        opaque(result("srvtoolu_1")),
        typed_thinking("dropped"),
        opaque(search_use("srvtoolu_2", "unrun")),
        opaque(fallback_marker("a", "b")),
        ContentBlock::Text {
            text: "after".into(),
        },
    ];
    let kept: Vec<Option<&str>> = echoed_blocks(&reply, &earlier)
        .iter()
        .map(|b| match b {
            ContentBlock::Opaque { raw, .. } => raw["type"].as_str(),
            ContentBlock::Text { .. } => Some("text"),
            _ => None,
        })
        .collect();
    assert_eq!(
        kept,
        [
            Some("web_search_tool_result"),
            Some("fallback"),
            Some("text")
        ]
    );
    assert_eq!(echoed_blocks(&reply, &ServerCalls::new()).len(), 2);

    let history = vec![
        Message::user_text("look"),
        assistant(vec![
            opaque(search_use("srvtoolu_1", "rg")),
            ContentBlock::ToolUse {
                id: "toolu_1".into(),
                name: "bash".into(),
                input: json!({}),
            },
        ]),
        Message {
            role: Role::User,
            content: vec![ContentBlock::ToolResult {
                tool_use_id: "toolu_1".into(),
                content: vec![ContentBlock::Text { text: "ok".into() }],
                is_error: false,
            }],
        },
        assistant(reply),
        Message::user_text("next"),
    ];
    let body = request_body(&history, &[], &[], &Rendering::default(), None);
    assert_eq!(content(&body, 1)[0], search_use("srvtoolu_1", "rg"));
    assert_eq!(content(&body, 3)[0], result("srvtoolu_1"));
    let mut longer = history.clone();
    longer.push(assistant(vec![ContentBlock::Text {
        text: "more".into(),
    }]));
    longer.push(Message::user_text("last"));
    let later = request_body(&longer, &[], &[], &Rendering::default(), None);
    let first = body["messages"].as_array().unwrap();
    assert_eq!(
        later["messages"].as_array().unwrap()[..first.len()],
        first[..]
    );
}
