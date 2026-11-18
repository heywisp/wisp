use serde_json::{Value, json};
use wisp_protocol::{CacheTtl, ContentBlock, Message, Role, ToolDefinition, ToolRef};

use super::{conversation, session, stamped};
use crate::agent::testing::body_of;
use crate::provider::anthropic::tests::tool;
use crate::provider::anthropic::{ErrorForm, prefix_bytes, rejected_features};
use crate::provider::{Feature, FeatureUse, Features, Request, RequestOptions};
use crate::tool::ToolSpec;

fn deferring() -> Features {
    Features {
        documented: true,
        ..Features::using(
            &[Feature::InlineTools, Feature::DeferredTools],
            FeatureUse::Established,
        )
    }
}

fn request_body(
    system: Option<&str>,
    deferred: &[ToolSpec],
    messages: &[Message],
    features: Features,
) -> Value {
    let session = session(system, CacheTtl::OneHour);
    let tools = vec![tool("alpha"), tool("delta")];
    let request = Request {
        session: &session,
        tools: &tools,
        deferred,
        by_value: &[],
        server_tools: &[],
        messages,
        previous_response_id: None,
        features,
        options: RequestOptions::default(),
        rendering: &crate::provider::UNRENDERED,
        compaction: None,
    };
    body_of(&request)
}

fn surfaced(blocks: Vec<ContentBlock>) -> Message {
    Message::turn_system(None, &blocks).unwrap()
}

fn by_value(name: &str) -> ContentBlock {
    let spec = tool(name);
    ContentBlock::ToolAddition {
        tool: ToolRef::ToolDefinition {
            definition: ToolDefinition {
                description: spec.description,
                input_schema: spec.input_schema,
                name: spec.name,
            },
        },
    }
}

fn by_name(name: &str) -> ContentBlock {
    ContentBlock::ToolAddition {
        tool: ToolRef::ToolReference { name: name.into() },
    }
}

#[test]
fn deferred_entries_are_flagged_sorted_with_the_rest_and_never_carry_the_breakpoint() {
    let messages = [Message::user_text("hi")];
    let deferred = [tool("beta"), tool("zeta")];
    let body = request_body(None, &deferred, &messages, deferring());
    assert_eq!(
        body["tools"],
        json!([
            {"description": "The alpha tool.", "input_schema": {"type": "object", "properties": {}}, "name": "alpha"},
            {"defer_loading": true, "description": "The beta tool.", "input_schema": {"type": "object", "properties": {}}, "name": "beta"},
            {"cache_control": {"type": "ephemeral", "ttl": "1h"}, "description": "The delta tool.", "input_schema": {"type": "object", "properties": {}}, "name": "delta"},
            {"defer_loading": true, "description": "The zeta tool.", "input_schema": {"type": "object", "properties": {}}, "name": "zeta"}
        ])
    );
    let raw = String::from_utf8(serde_json::to_vec(&body["tools"][1]).unwrap()).unwrap();
    assert!(
        raw.starts_with(r#"{"defer_loading":true,"description""#),
        "{raw}"
    );
}

#[test]
fn with_nothing_deferred_the_bytes_are_todays() {
    let messages = [Message::user_text("hi")];
    let plain = request_body(Some("You are Wisp."), &[], &messages, Features::default());
    let flagged = request_body(Some("You are Wisp."), &[], &messages, deferring());
    assert_eq!(plain["tools"], flagged["tools"]);
    assert!(!plain["tools"].to_string().contains("defer_loading"));
}

#[test]
fn a_by_value_addition_of_a_deferred_tool_goes_by_name_and_elsewhere_by_value() {
    let mut history = conversation();
    history.push(stamped(58, "And spring?"));
    history.push(surfaced(vec![
        by_value("zeta"),
        by_value("gamma"),
        ContentBlock::Text {
            text: "Use zeta for z.".into(),
        },
    ]));
    let deferred = [tool("zeta")];
    let body = request_body(Some("You are Wisp."), &deferred, &history, deferring());
    let system = body["messages"]
        .as_array()
        .unwrap()
        .iter()
        .find(|m| m["role"] == "system")
        .unwrap();
    assert_eq!(
        system["content"],
        json!([
            {"type": "tool_addition", "tool": {"type": "tool_reference", "name": "zeta"}},
            {"type": "tool_addition", "tool": {"type": "tool_definition", "definition": {
                "description": "The gamma tool.",
                "input_schema": {"type": "object", "properties": {}},
                "name": "gamma"
            }}},
            {"type": "text", "text": "Use zeta for z."}
        ])
    );
    let elsewhere = request_body(Some("You are Wisp."), &[], &history, deferring());
    let system = elsewhere["messages"]
        .as_array()
        .unwrap()
        .iter()
        .find(|m| m["role"] == "system")
        .unwrap();
    assert_eq!(
        system["content"][0]["tool"]["type"], "tool_definition",
        "a pair that doesn't defer declares it by value"
    );
}

#[test]
fn a_reference_to_a_deferred_tool_is_sent_only_where_it_is_declared() {
    let mut history = conversation();
    history.push(stamped(58, "And spring?"));
    history.push(surfaced(vec![by_name("zeta")]));
    let deferred = [tool("zeta")];
    let here = request_body(Some("You are Wisp."), &deferred, &history, deferring());
    let roles: Vec<&str> = here["messages"]
        .as_array()
        .unwrap()
        .iter()
        .map(|m| m["role"].as_str().unwrap())
        .collect();
    assert!(roles.contains(&"system"), "{roles:?}");
    let there = request_body(Some("You are Wisp."), &[], &history, deferring());
    assert!(
        there["messages"]
            .as_array()
            .unwrap()
            .iter()
            .all(|m| m["role"] != "system"),
        "an undeclared name stays off the wire"
    );
}

#[test]
fn a_surfacing_keeps_the_tools_and_the_prefix_across_turns() {
    let deferred = [tool("zeta")];
    let session = session(Some("You are Wisp."), CacheTtl::OneHour);
    let tools = vec![tool("alpha"), tool("delta")];
    let mut history = conversation();
    history.push(stamped(58, "And spring?"));
    let before = history.clone();
    history.push(surfaced(vec![by_name("zeta")]));
    history.push(Message {
        role: Role::Assistant,
        content: vec![ContentBlock::Text {
            text: "Blossom.".into(),
        }],
    });
    history.push(stamped(59, "And summer?"));
    let prefixes: Vec<_> = [&before[..], &history[..]]
        .iter()
        .map(|messages| {
            prefix_bytes(&Request {
                session: &session,
                tools: &tools,
                deferred: &deferred,
                by_value: &[],
                server_tools: &[],
                messages,
                previous_response_id: None,
                features: deferring(),
                options: RequestOptions::default(),
                rendering: &crate::provider::UNRENDERED,
                compaction: None,
            })
            .unwrap()
        })
        .collect();
    assert_eq!(prefixes[0], prefixes[1]);
    let bodies = [
        request_body(Some("You are Wisp."), &deferred, &before, deferring()),
        request_body(Some("You are Wisp."), &deferred, &history, deferring()),
    ];
    assert_eq!(bodies[0]["tools"], bodies[1]["tools"]);
    assert_eq!(bodies[0]["system"], bodies[1]["system"]);
    let earlier = bodies[0]["messages"].to_string();
    assert!(
        bodies[1]["messages"]
            .to_string()
            .starts_with(earlier.strip_suffix(']').unwrap())
    );
}

#[test]
fn a_refusal_naming_defer_loading_rejects_deferral_alone() {
    let message = "tools.1.custom.defer_loading: Extra inputs are not permitted";
    for form in [ErrorForm::Http400, ErrorForm::Json] {
        assert_eq!(
            rejected_features(message, form, deferring()),
            [Feature::DeferredTools],
            "{form:?}"
        );
    }
    let without = Features {
        documented: true,
        ..Features::using(&[Feature::InlineTools], FeatureUse::Established)
    };
    assert!(rejected_features(message, ErrorForm::Http400, without).is_empty());
    let other = "messages.3.content.0.tool_removal: Extra inputs are not permitted";
    assert!(
        !rejected_features(other, ErrorForm::Http400, deferring())
            .contains(&Feature::DeferredTools)
    );
}

#[test]
fn a_compaction_block_declaring_a_deferred_tool_by_value_names_it_by_reference() {
    let definition = |name: &str| serde_json::to_value(by_value(name)).unwrap();
    let raw = json!({
        "type": "compaction",
        "content": "So far: two files.",
        "tool_changes": [definition("zeta"), definition("gamma")]
    });
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
    let features = Features {
        documented: true,
        ..Features::using(
            &[
                Feature::InlineTools,
                Feature::DeferredTools,
                Feature::NativeCompaction,
            ],
            FeatureUse::Established,
        )
    };
    let deferred = [tool("zeta")];
    let here = request_body(Some("You are Wisp."), &deferred, &messages, features);
    let sent = &here["messages"][0]["content"][0];
    assert_eq!(sent["content"], raw["content"]);
    assert_eq!(
        sent["tool_changes"],
        json!([
            {"type": "tool_addition", "tool": {"type": "tool_reference", "name": "zeta"}},
            definition("gamma")
        ])
    );
    let there = request_body(Some("You are Wisp."), &[], &messages, features);
    assert_eq!(
        there["messages"][0]["content"][0], raw,
        "elsewhere it goes as it came"
    );
}

#[test]
fn a_compaction_block_naming_a_tool_sent_by_value_declares_it_by_value() {
    let raw = json!({
        "type": "compaction",
        "content": "So far: two files.",
        "tool_changes": [
            serde_json::to_value(by_name("zeta")).unwrap(),
            serde_json::to_value(by_name("gamma")).unwrap()
        ]
    });
    let messages = vec![Message {
        role: Role::User,
        content: vec![
            ContentBlock::opaque("anthropic", raw.clone()),
            ContentBlock::Text {
                text: "next".into(),
            },
        ],
    }];
    let session = session(Some("You are Wisp."), CacheTtl::OneHour);
    let tools = vec![tool("alpha"), tool("delta")];
    let by_value_tools = [tool("zeta")];
    let body = |by_value: &[ToolSpec]| -> Value {
        let request = Request {
            session: &session,
            tools: &tools,
            deferred: &[],
            by_value,
            server_tools: &[],
            messages: &messages,
            previous_response_id: None,
            features: Features {
                documented: true,
                ..Features::using(
                    &[Feature::InlineTools, Feature::NativeCompaction],
                    FeatureUse::Established,
                )
            },
            options: RequestOptions::default(),
            rendering: &crate::provider::UNRENDERED,
            compaction: None,
        };
        body_of(&request)
    };
    let here = body(&by_value_tools);
    let sent = &here["messages"][0]["content"][0];
    assert_eq!(sent["content"], raw["content"]);
    assert_eq!(
        sent["tool_changes"],
        json!([
            serde_json::to_value(by_value("zeta")).unwrap(),
            serde_json::to_value(by_name("gamma")).unwrap()
        ])
    );
    let there = body(&[]);
    assert_eq!(
        there["messages"][0]["content"][0], raw,
        "with nothing sent by value it goes as it came"
    );
}

#[test]
fn a_by_name_addition_of_a_tool_sent_by_value_goes_as_its_definition() {
    let mut history = conversation();
    history.push(stamped(58, "And spring?"));
    history.push(surfaced(vec![
        by_name("zeta"),
        ContentBlock::Text {
            text: "Use zeta for z.".into(),
        },
    ]));
    let session = session(Some("You are Wisp."), CacheTtl::OneHour);
    let tools = vec![tool("alpha"), tool("delta")];
    let by_value_tools = [tool("zeta")];
    let request = Request {
        session: &session,
        tools: &tools,
        deferred: &[],
        by_value: &by_value_tools,
        server_tools: &[],
        messages: &history,
        previous_response_id: None,
        features: Features {
            documented: true,
            ..Features::using(&[Feature::InlineTools], FeatureUse::Established)
        },
        options: RequestOptions::default(),
        rendering: &crate::provider::UNRENDERED,
        compaction: None,
    };
    let body: Value = body_of(&request);
    assert!(!body["tools"].to_string().contains("defer_loading"));
    let system = body["messages"]
        .as_array()
        .unwrap()
        .iter()
        .find(|m| m["role"] == "system")
        .unwrap();
    assert_eq!(
        system["content"],
        json!([
            {"type": "tool_addition", "tool": {"type": "tool_definition", "definition": {
                "description": "The zeta tool.",
                "input_schema": {"type": "object", "properties": {}},
                "name": "zeta"
            }}},
            {"type": "text", "text": "Use zeta for z."}
        ])
    );
}

#[test]
fn a_deferred_hit_after_tool_results_is_sent_by_name_without_changing_the_array() {
    let mut messages = vec![
        Message::user_text("Find zeta."),
        Message {
            role: Role::Assistant,
            content: vec![ContentBlock::ToolUse {
                id: "find".into(),
                name: "alpha".into(),
                input: json!({}),
            }],
        },
        Message {
            role: Role::User,
            content: vec![ContentBlock::ToolResult {
                tool_use_id: "find".into(),
                content: vec![],
                is_error: false,
            }],
        },
    ];
    let deferred = [tool("zeta")];
    let before = request_body(None, &deferred, &messages, deferring());
    messages.push(surfaced(vec![by_name("zeta")]));
    let after = request_body(None, &deferred, &messages, deferring());
    assert_eq!(before["tools"], after["tools"]);
    assert_eq!(
        after["messages"][3]["content"][0]["tool"],
        json!({"type":"tool_reference","name":"zeta"})
    );
}
