use serde_json::{Value, json};
use wisp_protocol::{CacheTtl, ContentBlock, Effort, EffortMarker, Message};

use super::session;
use crate::provider::anthropic::tests::tool;
use crate::provider::anthropic::{build_body, build_marked_body, sent_prefix};
use crate::provider::{FeatureUse, Features, Rendering, Request, TailMarker, UNRENDERED};
use crate::test_guard::text;
use crate::test_guard::{assistant, user};
use crate::tool::ToolSpec;
use wisp_protocol::log::{BlockPosition, Cut};

const SYSTEM: &str = "You are Wisp.";

fn hour() -> Value {
    json!({"type": "ephemeral", "ttl": "1h"})
}

fn five() -> Value {
    json!({"type": "ephemeral"})
}

fn thinking(text: &str) -> ContentBlock {
    ContentBlock::Thinking {
        thinking: text.into(),
        signature: "sig==".into(),
    }
}

fn tool_result(id: &str, out: &str) -> ContentBlock {
    ContentBlock::ToolResult {
        tool_use_id: id.into(),
        content: vec![text(out)],
        is_error: false,
    }
}

fn tool_use(id: &str) -> ContentBlock {
    ContentBlock::ToolUse {
        id: id.into(),
        name: "bash".into(),
        input: json!({"command": "ls"}),
    }
}

fn history() -> Vec<Message> {
    vec![
        Message::user_text("list the files"),
        assistant(vec![thinking("look"), text("Listing."), tool_use("t1")]),
        user(vec![tool_result("t1", "a.rs")]),
        assistant(vec![thinking("done"), text("One file.")]),
    ]
}

fn features(per_message: bool) -> Features {
    Features {
        per_message_effort: per_message.then_some(FeatureUse::Established),
        binding_controls: None,
        native_compaction: None,
        documented: false,
        inline_tools: None,
        deferred_tools: None,
        thread: None,
    }
}

fn marked(messages: &[Message], tail: Option<TailMarker>, per_message: bool) -> Value {
    let session = session(Some(SYSTEM), CacheTtl::OneHour);
    let request = Request {
        session: &session,
        tools: &[],
        deferred: &[],
        by_value: &[],
        server_tools: &[],
        messages,
        previous_response_id: None,
        features: features(per_message),
        options: Default::default(),
        rendering: &UNRENDERED,
        compaction: None,
    };
    let body = build_marked_body(&request, tail).unwrap();
    serde_json::from_slice(&body).unwrap()
}

fn automatic(messages: &[Message], ttl: CacheTtl) -> Vec<u8> {
    let session = session(Some("You are Wisp."), ttl);
    let request = Request {
        session: &session,
        tools: &[],
        deferred: &[],
        by_value: &[],
        server_tools: &[],
        messages,
        previous_response_id: None,
        features: Features::default(),
        options: Default::default(),
        rendering: &UNRENDERED,
        compaction: None,
    };
    build_body(&request).unwrap()
}

fn marked_blocks(body: &Value) -> Vec<(usize, usize, Value)> {
    let mut found = Vec::new();
    for (m, message) in body["messages"].as_array().unwrap().iter().enumerate() {
        for (b, block) in message["content"].as_array().unwrap().iter().enumerate() {
            if let Some(marker) = block.get("cache_control") {
                found.push((m, b, marker.clone()));
            }
        }
    }
    found
}

fn every_marker(body: &Value) -> Vec<Value> {
    let mut out = Vec::new();
    for key in ["tools", "system"] {
        for item in body[key].as_array().into_iter().flatten() {
            out.extend(item.get("cache_control").cloned());
        }
    }
    out.extend(marked_blocks(body).into_iter().map(|(_, _, m)| m));
    out.extend(body.get("cache_control").cloned());
    out
}

fn sent_with(
    system: &str,
    messages: &[Message],
    per_message: bool,
    rendering: &Rendering,
    tools: &[ToolSpec],
) -> String {
    let session = session(Some(system), CacheTtl::OneHour);
    let request = Request {
        session: &session,
        tools,
        deferred: &[],
        by_value: &[],
        server_tools: &[],
        messages,
        previous_response_id: None,
        features: features(per_message),
        options: Default::default(),
        rendering,
        compaction: None,
    };
    sent_prefix(&request).unwrap_or_default()
}

fn tail_of(
    messages: &[Message],
    view_len: usize,
    ttl: CacheTtl,
    per_message: bool,
) -> Option<TailMarker> {
    let view = messages.get(..view_len).unwrap_or(messages);
    Some(TailMarker {
        view_len,
        ttl,
        prefix: sent_with(SYSTEM, view, per_message, &UNRENDERED, &[]),
        last_blocks: None,
    })
}

#[test]
fn the_tail_marker_sits_on_the_anchors_last_block() {
    let mut messages = history();
    messages.push(Message::user_text("Summarise."));
    let body = marked(
        &messages,
        tail_of(&messages, 3, CacheTtl::OneHour, false),
        false,
    );
    assert_eq!(marked_blocks(&body), [(2, 0, hour())]);
    assert!(body.get("cache_control").is_none());
    assert_eq!(body["system"][0]["cache_control"], hour());
}

#[test]
fn the_marker_skips_thinking_at_the_end_of_the_view() {
    let messages = vec![
        Message::user_text("hi"),
        assistant(vec![text("Hello."), thinking("then")]),
        Message::user_text("next"),
    ];
    let body = marked(
        &messages,
        tail_of(&messages, 2, CacheTtl::OneHour, false),
        false,
    );
    assert_eq!(marked_blocks(&body), [(1, 0, hour())]);
    assert_eq!(body["messages"][1]["content"][0]["text"], "Hello.");
}

#[test]
fn an_anchor_ending_in_an_effort_marker_marks_the_block_before_it() {
    let marker = EffortMarker {
        level: Effort::Low,
        rendered: true,
    };
    let mut messages = history();
    messages.push(Message::user_text("now quickly"));
    messages.push(Message::effort(marker));
    messages.push(assistant(vec![text("Done.")]));
    let body = marked(
        &messages,
        tail_of(&messages, 6, CacheTtl::OneHour, true),
        true,
    );
    assert_eq!(body["messages"][5]["role"], "system");
    assert!(
        body["messages"][5]["content"]
            .as_array()
            .unwrap()
            .is_empty()
    );
    assert_eq!(marked_blocks(&body), [(4, 0, hour())]);
}

#[test]
fn the_marker_carries_the_given_ttl_after_the_hour_markers() {
    let messages = history();
    let body = marked(
        &messages,
        tail_of(&messages, 3, CacheTtl::FiveMinutes, false),
        false,
    );
    assert_eq!(marked_blocks(&body), [(2, 0, five())]);
    let markers = every_marker(&body);
    assert!(markers.len() <= 4, "{markers:?}");
    let first_short = markers.iter().position(|m| *m == five()).unwrap();
    assert!(markers[first_short..].iter().all(|m| *m == five()));
}

#[test]
fn no_tail_marker_and_no_top_level_marker_without_an_anchor() {
    let messages = history();
    for tail in [
        None,
        tail_of(&messages, 0, CacheTtl::OneHour, false),
        tail_of(&messages, 9, CacheTtl::OneHour, false),
    ] {
        let body = marked(&messages, tail.clone(), false);
        assert!(marked_blocks(&body).is_empty(), "{tail:?}");
        assert!(body.get("cache_control").is_none());
        assert_eq!(every_marker(&body), [hour()]);
    }
}

#[test]
fn the_marked_block_keeps_its_bytes_and_gains_only_the_marker() {
    let messages = history();
    let plain = String::from_utf8(automatic(&messages, CacheTtl::OneHour)).unwrap();
    let session = session(Some("You are Wisp."), CacheTtl::OneHour);
    let request = Request {
        session: &session,
        tools: &[],
        deferred: &[],
        by_value: &[],
        server_tools: &[],
        messages: &messages,
        previous_response_id: None,
        features: Features::default(),
        options: Default::default(),
        rendering: &UNRENDERED,
        compaction: None,
    };
    let tail = tail_of(&messages, 3, CacheTtl::OneHour, false);
    let body = build_marked_body(&request, tail).unwrap();
    let body = String::from_utf8(body).unwrap();
    let block =
        r#"{"type":"tool_result","tool_use_id":"t1","content":[{"type":"text","text":"a.rs"}]"#;
    assert!(plain.contains(&format!("{block}}}")), "{plain}");
    let with_marker = format!(r#"{block},"cache_control":{{"type":"ephemeral","ttl":"1h"}}}}"#);
    assert!(body.contains(&with_marker), "{body}");
    let (head, rest) = body.split_once(&with_marker).unwrap();
    let (plain_head, plain_rest) = plain.split_once(&format!("{block}}}")).unwrap();
    assert!(plain_head.ends_with(head.split_once(r#""messages":"#).unwrap().1));
    assert_eq!(rest, plain_rest);
}

#[test]
fn the_default_mode_still_sends_the_top_level_marker_and_no_block_marker() {
    let messages = history();
    for (ttl, marker) in [
        (CacheTtl::OneHour, r#"{"type":"ephemeral","ttl":"1h"}"#),
        (CacheTtl::FiveMinutes, r#"{"type":"ephemeral"}"#),
    ] {
        let plain = String::from_utf8(automatic(&messages, ttl)).unwrap();
        let top = format!(r#""output_config":{{"effort":"high"}},"cache_control":{marker},"#);
        assert!(plain.contains(&top), "{plain}");
        let value: Value = serde_json::from_str(&plain).unwrap();
        assert!(marked_blocks(&value).is_empty());
        let mut without = value.clone();
        without.as_object_mut().unwrap().remove("cache_control");
        assert_eq!(marked(&messages, None, false), without);
    }
}

#[test]
fn a_prefix_that_differs_from_what_the_anchor_sent_gets_no_marker() {
    let mut messages = history();
    messages.push(Message::user_text("Summarise."));
    let view = &messages[..3];
    let cut = Rendering {
        cuts: vec![Cut {
            from: BlockPosition {
                message: 1,
                block: 0,
            },
            through: 1,
            tools: false,
        }],
        ..Rendering::default()
    };
    let sent = |system, rendering: &Rendering, tools: &[ToolSpec]| {
        Some(TailMarker {
            view_len: 3,
            ttl: CacheTtl::OneHour,
            prefix: sent_with(system, view, false, rendering, tools),
            last_blocks: None,
        })
    };
    let same = marked(&messages, sent(SYSTEM, &UNRENDERED, &[]), false);
    assert_eq!(marked_blocks(&same), [(2, 0, hour())]);
    let differs = [
        sent("You were Wisp.", &UNRENDERED, &[]),
        sent(SYSTEM, &UNRENDERED, &[tool("bash")]),
        sent(SYSTEM, &cut, &[]),
        sent(SYSTEM, &UNRENDERED, &[]).map(|t| TailMarker {
            prefix: String::new(),
            ..t
        }),
    ];
    for tail in differs {
        let body = marked(&messages, tail.clone(), false);
        assert!(marked_blocks(&body).is_empty(), "{tail:?}");
        assert_eq!(every_marker(&body), [hour()]);
    }
}
