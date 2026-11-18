use serde_json::{Value, json};
use wisp_protocol::{
    CacheTtl, ContentBlock, Effort, EffortMarker, Message, Role, ToolDefinition, ToolRef,
};

use super::{conversation, session, stamped};
use crate::agent::testing::{body_of, text_of};
use crate::provider::anthropic::{INLINE_TOOLS_BETA, beta_header, sent_prefix};
use crate::provider::{
    CompactionAsk, Feature, FeatureUse, Features, Request, RequestOptions, TailMarker,
};
use crate::test_guard::{assistant, text, user};
use crate::tool::ToolSpec;

fn array() -> Vec<ToolSpec> {
    ["alpha", "beta"]
        .map(|name| ToolSpec {
            description: format!("The {name} tool."),
            input_schema: json!({"type": "object", "properties": {}}),
            name: name.into(),
        })
        .to_vec()
}

fn by_name(name: &str) -> ToolRef {
    ToolRef::ToolReference { name: name.into() }
}

pub(super) fn gamma() -> ContentBlock {
    ContentBlock::ToolAddition {
        tool: ToolRef::ToolDefinition {
            definition: ToolDefinition {
                description: "The gamma tool.".into(),
                input_schema: json!({"type": "object", "properties": {}}),
                name: "gamma".into(),
            },
        },
    }
}

fn switched(blocks: &[ContentBlock]) -> Message {
    Message::turn_system(None, blocks).unwrap()
}

fn history() -> Vec<Message> {
    let mut history = conversation();
    history[0] = stamped(56, "Write a haiku about autumn.");
    history[2] = stamped(57, "Another, about winter.");
    history.push(stamped(58, "And spring?"));
    history.push(switched(&[
        ContentBlock::ToolRemoval {
            tool: by_name("alpha"),
        },
        gamma(),
        ContentBlock::Text {
            text: "Use gamma for g.".into(),
        },
    ]));
    history.push(Message {
        role: Role::Assistant,
        content: vec![ContentBlock::Text {
            text: "Blossom.".into(),
        }],
    });
    history.push(stamped(59, "And summer?"));
    history.push(switched(&[ContentBlock::ToolAddition {
        tool: by_name("alpha"),
    }]));
    history
}

fn sent(features: Features, messages: &[Message]) -> (Value, String) {
    let session = session(Some("You are Wisp."), CacheTtl::OneHour);
    let tools = array();
    let request = Request {
        session: &session,
        tools: &tools,
        deferred: &[],
        by_value: &[],
        server_tools: &[],
        messages,
        previous_response_id: None,
        features,
        options: RequestOptions::default(),
        rendering: &crate::provider::UNRENDERED,
        compaction: None,
    };
    let body = text_of(&request);
    (serde_json::from_str(&body).unwrap(), beta_header(&request))
}

fn established() -> Features {
    Features {
        documented: true,
        ..Features::using(&[Feature::InlineTools], FeatureUse::Established)
    }
}

fn prefix_of(earlier: &Value, later: &Value) -> bool {
    let (a, b) = (
        earlier["messages"].to_string(),
        later["messages"].to_string(),
    );
    b.starts_with(a.strip_suffix(']').unwrap())
}

fn roles(body: &Value) -> Vec<&str> {
    body["messages"]
        .as_array()
        .unwrap()
        .iter()
        .map(|m| m["role"].as_str().unwrap())
        .collect()
}

#[test]
fn tool_changes_keep_the_tools_array_and_the_prefix_on_a_capable_pair() {
    let history = history();
    let cuts = [5, 6, 8, 9];
    let bodies: Vec<(Value, String)> = cuts
        .iter()
        .map(|n| sent(established(), &history[..*n]))
        .collect();
    for (body, betas) in &bodies {
        assert_eq!(body["tools"], bodies[0].0["tools"]);
        assert_eq!(body["system"], bodies[0].0["system"]);
        assert!(betas.contains(INLINE_TOOLS_BETA), "{betas}");
    }
    for pair in bodies.windows(2) {
        assert!(prefix_of(&pair[0].0, &pair[1].0));
    }
    let last = &bodies[3].0["messages"];
    assert_eq!(
        roles(&bodies[3].0),
        [
            "user",
            "assistant",
            "user",
            "assistant",
            "user",
            "system",
            "assistant",
            "user",
            "system"
        ]
    );
    assert_eq!(
        last[5]["content"],
        json!([
            {"type": "tool_removal", "tool": {"type": "tool_reference", "name": "alpha"}},
            {"type": "tool_addition", "tool": {"type": "tool_definition", "definition": {
                "description": "The gamma tool.",
                "input_schema": {"type": "object", "properties": {}},
                "name": "gamma"
            }}},
            {"type": "text", "text": "Use gamma for g."}
        ])
    );
    assert_eq!(
        last[8]["content"],
        json!([{"type": "tool_addition", "tool": {"type": "tool_reference", "name": "alpha"}}])
    );
}

#[test]
fn without_the_capability_tool_blocks_never_reach_the_wire_and_the_prefix_holds() {
    let history = history();
    let plain: Vec<(Value, String)> = [5, 6, 8, 9]
        .iter()
        .map(|n| sent(Features::default(), &history[..*n]))
        .collect();
    for (body, betas) in &plain {
        assert!(!betas.contains(INLINE_TOOLS_BETA));
        assert!(!body.to_string().contains("tool_removal"));
        assert!(!body.to_string().contains("tool_addition"));
        assert_eq!(body["tools"], plain[0].0["tools"]);
    }
    for pair in plain.windows(2) {
        assert!(prefix_of(&pair[0].0, &pair[1].0));
    }
    let last = &plain[3].0["messages"];
    assert!(
        last.as_array()
            .unwrap()
            .iter()
            .all(|m| m["role"] != "system"),
        "{last}"
    );
    assert!(!last.to_string().contains("Use gamma for g."));
}

#[test]
fn a_trial_pair_sends_the_beta_only_with_tool_changes_in_view() {
    let trial = Features::using(&[Feature::InlineTools], FeatureUse::Trial);
    let history = history();
    assert!(!sent(trial, &history[..5]).1.contains(INLINE_TOOLS_BETA));
    assert!(sent(trial, &history[..6]).1.contains(INLINE_TOOLS_BETA));
    assert!(
        sent(established(), &history[..5])
            .1
            .contains(INLINE_TOOLS_BETA)
    );
}

#[test]
fn a_by_name_block_for_a_tool_never_declared_stays_off_the_wire() {
    let mut history = history()[..5].to_vec();
    history.push(switched(&[
        ContentBlock::ToolRemoval {
            tool: by_name("nowhere"),
        },
        ContentBlock::ToolAddition {
            tool: by_name("gamma"),
        },
    ]));
    let (body, _) = sent(established(), &history);
    let text = body.to_string();
    assert!(!text.contains("nowhere"), "{text}");
    assert!(!text.contains("gamma"), "{text}");
}

#[test]
fn the_miss_check_leaves_the_inline_beta_out_of_its_beta_digest() {
    let session = session(Some("You are Wisp."), CacheTtl::OneHour);
    let tools = array();
    let history = history();
    let print = |features: Features, messages: &[Message]| {
        let request = Request {
            session: &session,
            tools: &tools,
            deferred: &[],
            by_value: &[],
            server_tools: &[],
            messages,
            previous_response_id: None,
            features,
            options: RequestOptions::default(),
            rendering: &crate::provider::UNRENDERED,
            compaction: None,
        };
        (
            crate::provider::anthropic::wire_print(&request)
                .unwrap()
                .betas,
            beta_header(&request),
        )
    };
    let trial = Features::using(&[Feature::InlineTools], FeatureUse::Trial);
    let (before, plain) = print(trial, &history[..5]);
    let (after, with) = print(trial, &history[..6]);
    assert!(!plain.contains(INLINE_TOOLS_BETA) && with.contains(INLINE_TOOLS_BETA));
    assert_eq!(before, after);
}

#[test]
fn an_explicit_tail_over_a_removal_marks_the_removal_block() {
    let session = session(Some("You are Wisp."), CacheTtl::OneHour);
    let tools = array();
    let mut messages = conversation();
    messages.push(stamped(58, "And spring?"));
    messages.push(switched(&[ContentBlock::ToolRemoval {
        tool: by_name("alpha"),
    }]));
    let request = |ask| Request {
        session: &session,
        tools: &tools,
        deferred: &[],
        by_value: &[],
        server_tools: &[],
        messages: &messages,
        previous_response_id: None,
        features: established(),
        options: RequestOptions::default(),
        rendering: &crate::provider::UNRENDERED,
        compaction: ask,
    };
    let ask = CompactionAsk {
        native_instructions: None,
        tail: Some(TailMarker {
            view_len: messages.len(),
            ttl: CacheTtl::FiveMinutes,
            prefix: sent_prefix(&request(None)).unwrap(),
            last_blocks: None,
        }),
    };
    let body: Value = body_of(&request(Some(&ask)));
    let last = body["messages"].as_array().unwrap().last().unwrap();
    assert_eq!(last["role"], "system");
    assert_eq!(last["content"][0]["type"], "tool_removal");
    assert_eq!(
        last["content"][0]["cache_control"],
        json!({"type": "ephemeral"})
    );
}

fn sent_voiding(features: Features, messages: &[Message], point: usize) -> (Value, String) {
    let session = session(Some("You are Wisp."), CacheTtl::OneHour);
    let mut tools = array();
    tools.push(ToolSpec {
        description: "The gamma tool.".into(),
        input_schema: json!({"type": "object", "properties": {}}),
        name: "gamma".into(),
    });
    let rendering = crate::provider::Rendering {
        tools_point: point,
        array: tools.iter().map(|t| t.name.clone()).collect(),
        ..crate::provider::Rendering::default()
    };
    let request = Request {
        session: &session,
        tools: &tools,
        deferred: &[],
        by_value: &[],
        server_tools: &[],
        messages,
        previous_response_id: None,
        features,
        options: RequestOptions::default(),
        rendering: &rendering,
        compaction: None,
    };
    let body = text_of(&request);
    let carries = crate::provider::carries_tool_changes(
        messages,
        &rendering,
        crate::provider::anthropic::compaction_tool_changes,
    );
    if features.inline_tools == Some(FeatureUse::Trial) {
        assert_eq!(carries, beta_header(&request).contains(INLINE_TOOLS_BETA));
    }
    (serde_json::from_str(&body).unwrap(), beta_header(&request))
}

fn system_contents(body: &Value) -> Vec<Vec<String>> {
    body["messages"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|m| m["role"] == "system")
        .map(|m| {
            m["content"]
                .as_array()
                .unwrap()
                .iter()
                .map(|b| {
                    let name = b["tool"]["name"]
                        .as_str()
                        .or_else(|| b["tool"]["definition"]["name"].as_str())
                        .unwrap_or("");
                    format!("{} {name}", b["type"].as_str().unwrap())
                })
                .collect()
        })
        .collect()
}

#[test]
fn a_voided_addition_leaves_the_wire_and_its_message_keeps_the_other_blocks() {
    let history = history();
    let (kept, _) = sent_voiding(established(), &history, 0);
    assert_eq!(
        system_contents(&kept),
        [
            vec!["tool_removal alpha", "tool_addition gamma", "text "],
            vec!["tool_addition alpha"]
        ]
    );
    let (voided, _) = sent_voiding(established(), &history, 9);
    assert_eq!(
        system_contents(&voided),
        [
            vec!["tool_removal alpha", "text "],
            vec!["tool_addition alpha"]
        ]
    );
}

#[test]
fn a_message_of_voided_additions_only_is_not_sent_and_a_trial_takes_no_beta_for_it() {
    let mut history = conversation();
    history.push(stamped(58, "And spring?"));
    history.push(switched(&[gamma()]));
    let trial = Features::using(&[Feature::InlineTools], FeatureUse::Trial);
    let (sent_whole, betas) = sent_voiding(trial, &history, 0);
    assert_eq!(system_contents(&sent_whole), [vec!["tool_addition gamma"]]);
    assert!(betas.contains(INLINE_TOOLS_BETA), "{betas}");
    let (voided, betas) = sent_voiding(trial, &history, history.len());
    assert!(system_contents(&voided).is_empty(), "{voided:#}");
    assert!(!betas.contains(INLINE_TOOLS_BETA), "{betas}");
}

#[test]
fn an_addition_after_a_result_batch_is_a_separate_system_message_and_strictly_appends() {
    let mut history = vec![
        Message::user_text("Find gamma."),
        Message {
            role: Role::Assistant,
            content: vec![ContentBlock::ToolUse {
                id: "found".into(),
                name: "alpha".into(),
                input: json!({}),
            }],
        },
        Message {
            role: Role::User,
            content: vec![ContentBlock::ToolResult {
                tool_use_id: "found".into(),
                content: vec![],
                is_error: false,
            }],
        },
    ];
    let before = sent(established(), &history).0;
    history.push(switched(&[gamma()]));
    let after = sent(established(), &history).0;
    assert!(prefix_of(&before, &after));
    let messages = after["messages"].as_array().unwrap();
    assert_eq!(
        messages[messages.len() - 2]["content"][0]["type"],
        "tool_result"
    );
    assert_eq!(messages.last().unwrap()["role"], "system");
    assert_eq!(
        messages.last().unwrap()["content"][0]["type"],
        "tool_addition"
    );
}

fn restated() -> Message {
    switched(&[gamma(), text("Use gamma for g.")])
}

fn called_by_openai() -> [Message; 2] {
    let call =
        json!({"type": "function_call", "call_id": "call_1", "name": "alpha", "arguments": "{}"});
    [
        assistant(vec![
            crate::provider::envelope("openai", &call, 1, "openai"),
            ContentBlock::ToolUse {
                id: "call_1".into(),
                name: "alpha".into(),
                input: json!({}),
            },
        ]),
        user(vec![ContentBlock::ToolResult {
            tool_use_id: "call_1".into(),
            content: vec![text("done")],
            is_error: false,
        }]),
    ]
}

#[test]
fn a_system_message_with_content_goes_behind_the_user_messages_that_follow_it() {
    let after_compaction = [
        Message::user_text("The summary, then the note to continue."),
        restated(),
        Message::user_text("A steer that waited."),
    ];
    let (first, _) = sent(established(), &after_compaction);
    assert_eq!(roles(&first), ["user", "user", "system"]);
    assert_eq!(
        first["messages"][2],
        sent(established(), &after_compaction[..2]).0["messages"][1]
    );
    let mut committed = after_compaction.to_vec();
    committed.extend(called_by_openai());
    committed.push(assistant(vec![text("Done.")]));
    committed.push(stamped(59, "Mail that opens the next turn."));
    let (later, _) = sent(established(), &committed);
    assert_eq!(
        roles(&later),
        [
            "user",
            "user",
            "system",
            "assistant",
            "user",
            "assistant",
            "user"
        ]
    );
    assert!(prefix_of(&first, &later));
}

#[test]
fn only_a_system_message_with_content_before_a_user_message_moves() {
    let features = Features {
        per_message_effort: Some(FeatureUse::Established),
        ..established()
    };
    let marker = EffortMarker {
        level: Effort::Low,
        rendered: true,
    };
    let ask = || Message::user_text("Say ok.");
    let reply = || assistant(vec![text("Ok.")]);
    let both = Message::turn_system(Some(marker), &[gamma()]).unwrap();
    let cases: [(Vec<Message>, &[&str]); 6] = [
        (
            vec![ask(), restated(), reply(), ask()],
            &["user", "system", "assistant", "user"],
        ),
        (
            vec![ask(), restated(), Message::effort(marker), reply(), ask()],
            &["user", "system", "system", "assistant", "user"],
        ),
        (
            vec![ask(), Message::effort(marker), ask()],
            &["user", "system", "user"],
        ),
        (
            vec![
                ask(),
                restated(),
                ask(),
                switched(&[text("Then this.")]),
                ask(),
            ],
            &["user", "user", "user", "system", "system"],
        ),
        (vec![ask(), both, ask()], &["user", "user", "system"]),
        (
            vec![ask(), restated(), Message::effort(marker), ask()],
            &["user", "system", "user", "system"],
        ),
    ];
    for (history, wanted) in &cases {
        let (body, _) = sent(features, history);
        assert_eq!(roles(&body), *wanted, "{}", body["messages"]);
    }
    let moved = sent(features, &cases[3].0).0;
    assert_eq!(moved["messages"][3]["content"][0]["type"], "tool_addition");
    assert_eq!(moved["messages"][4]["content"][0]["text"], "Then this.");
    let marked = sent(features, &cases[4].0).0;
    assert_eq!(marked["messages"][2]["output_config"]["effort"], "low");
    assert_eq!(marked["messages"][2]["content"][0]["type"], "tool_addition");
    let stays = sent(features, &cases[5].0).0;
    assert_eq!(stays["messages"][1]["content"], json!([]));
    let plain = sent(Features::default(), &cases[3].0).0;
    assert_eq!(roles(&plain), ["user", "user", "user"]);
}
