use futures::StreamExt;
use serde_json::{Value, json};
use wisp_protocol::{CacheTtl, ContentBlock, ErrorKind, ImageSource, Message, RenderingForm};

use super::{api_key, http, mock_server, session};
use crate::agent::testing::body_of;
use crate::provider::anthropic::AnthropicProvider;
use crate::provider::{
    Feature, FeatureUse, Features, Provider, ProviderError, Refused, Rendering, Request,
};
use crate::test_guard::opaque;
use crate::test_guard::text;
use crate::test_guard::{assistant, thinking, user};

fn search(id: &str, query: &str) -> ContentBlock {
    opaque(
        json!({"type": "server_tool_use", "id": id, "name": "web_search", "input": {"query": query}}),
    )
}

fn body(messages: &[Message], rendering: &Rendering) -> Value {
    let session = session(None, CacheTtl::OneHour);
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
        rendering,
        compaction: None,
    };
    body_of(&request)
}

fn closed(form: RenderingForm) -> Rendering {
    Rendering {
        closed: ["s1".to_owned()].into(),
        form,
        ..Rendering::default()
    }
}

fn history() -> Vec<Message> {
    vec![
        Message::user_text("go"),
        assistant(vec![thinking("kept"), text("first answer")]),
        Message::user_text("search"),
        assistant(vec![
            thinking("before"),
            search("s1", "rust"),
            ContentBlock::ToolUse {
                id: "toolu_1".into(),
                name: "bash".into(),
                input: json!({"command": "ls", "b": 1}),
            },
            ContentBlock::ToolUse {
                id: "toolu_2".into(),
                name: "bash".into(),
                input: json!({"command": "false"}),
            },
        ]),
        user(vec![
            ContentBlock::ToolResult {
                tool_use_id: "toolu_1".into(),
                content: vec![
                    text("a.txt"),
                    ContentBlock::Image {
                        source: ImageSource::Base64 {
                            media_type: wisp_protocol::ImageType::Png,
                            data: "iVBORw0KGgo=".into(),
                        },
                    },
                ],
                is_error: false,
            },
            ContentBlock::ToolResult {
                tool_use_id: "toolu_2".into(),
                content: vec![text("exit 1")],
                is_error: true,
            },
            text("next turn"),
        ]),
    ]
}

fn closed_line() -> Value {
    json!({"type": "text", "text": "[web search for \"rust\": the turn ended before its result arrived]"})
}

#[test]
fn form_2_leaves_out_only_the_rendered_messages_thinking() {
    let mut messages = history();
    messages.truncate(3);
    messages.push(assistant(vec![thinking("before"), search("s1", "rust")]));
    messages.push(Message::user_text("next turn"));
    let first = body(&messages, &closed(RenderingForm::Text));
    let second = body(&messages, &closed(RenderingForm::WithoutThinking));
    assert_eq!(first["messages"][3]["content"][0]["type"], "thinking");
    assert_eq!(second["messages"][3]["content"], json!([closed_line()]));
    assert_eq!(second["messages"][1], first["messages"][1]);
    assert_eq!(second["messages"][4], first["messages"][4]);
    assert_eq!(
        body(&messages, &Rendering::default())["messages"][3]["content"][0]["type"],
        "thinking"
    );
}

#[test]
fn form_2_sends_a_rendered_tool_turn_with_thinking_in_the_flattened_form() {
    let messages = history();
    let second = body(&messages, &closed(RenderingForm::WithoutThinking));
    let flat = body(&messages, &closed(RenderingForm::Flattened));
    let first = body(&messages, &closed(RenderingForm::Text));
    assert_eq!(second["messages"][1], first["messages"][1]);
    assert_eq!(second["messages"][3], flat["messages"][3]);
    assert_eq!(second["messages"][4], flat["messages"][4]);
    assert_eq!(second["messages"].as_array().unwrap().len(), 5);
    let wire = second["messages"].to_string();
    assert!(!wire.contains("\"tool_use\""), "{wire}");
    assert!(!wire.contains("\"tool_result\""), "{wire}");
    let mut cut = closed(RenderingForm::WithoutThinking);
    cut.cuts = vec![wisp_protocol::log::Cut {
        from: wisp_protocol::log::BlockPosition {
            message: 3,
            block: 0,
        },
        through: 3,
        tools: false,
    }];
    let kept = body(&messages, &cut);
    assert_eq!(kept["messages"][3]["content"][1]["type"], "tool_use");
    assert_eq!(kept["messages"][4]["content"][0]["type"], "tool_result");
}

#[test]
fn form_3_turns_the_exchange_into_plain_alternating_text() {
    let messages = history();
    let flat = body(&messages, &closed(RenderingForm::Flattened));
    let first = body(&messages, &closed(RenderingForm::Text));
    assert_eq!(flat["messages"][0], first["messages"][0]);
    assert_eq!(flat["messages"][1], first["messages"][1]);
    assert_eq!(flat["messages"][2], first["messages"][2]);
    assert_eq!(
        flat["messages"][3],
        json!({"role": "assistant", "content": [{"type": "text", "text":
            "[web search for \"rust\": the turn ended before its result arrived]\n[called bash: {\"b\":1,\"command\":\"ls\"}]\n[called bash: {\"command\":\"false\"}]"
        }]})
    );
    assert_eq!(
        flat["messages"][4],
        json!({"role": "user", "content": [
            {"type": "text", "text": "[result of bash: a.txt\n[image]]"},
            {"type": "text", "text": "[error from bash: exit 1]"},
            {"type": "text", "text": "next turn"}
        ]})
    );
    assert_eq!(flat["messages"].as_array().unwrap().len(), 5);
}

#[test]
fn form_3_renders_other_server_tools_and_cited_text_as_text() {
    let messages = vec![
        Message::user_text("go"),
        assistant(vec![
            opaque(
                json!({"type": "server_tool_use", "id": "x1", "name": "code_execution", "input": {"code": "1"}}),
            ),
            opaque(
                json!({"type": "code_execution_tool_result", "tool_use_id": "x1", "content": {"stdout": "1"}}),
            ),
            search("s1", "rust"),
        ]),
        Message::user_text("next"),
        assistant(vec![opaque(json!({
            "type": "text",
            "text": "cited answer",
            "citations": [{"type": "web_search_result_location", "url": "u"}]
        }))]),
        Message::user_text("again"),
    ];
    let mut rendering = closed(RenderingForm::Flattened);
    rendering.native_as_text = vec![false, false, false, true, false];
    let flat = body(&messages, &rendering);
    assert_eq!(
        flat["messages"][1]["content"][0]["text"],
        "[called code_execution: {\"code\":\"1\"}]\n[result of code_execution: {\"stdout\":\"1\"}]\n[web search for \"rust\": the turn ended before its result arrived]"
    );
    assert_eq!(
        flat["messages"][3],
        json!({"role": "assistant", "content": [{"type": "text", "text": "cited answer"}]})
    );
    assert_eq!(flat["messages"][4]["content"][0]["text"], "again");
}

fn rendered_request<'a>(
    session: &'a wisp_protocol::SessionConfig,
    messages: &'a [Message],
    rendering: &'a Rendering,
    features: Features,
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
        options: Default::default(),
        rendering,
        compaction: None,
    }
}

async fn error_for(response: String, rendered: bool, features: Features) -> ProviderError {
    let (base, _) = mock_server(vec![response]).await;
    let provider = AnthropicProvider::new(&api_key("k"), &base).unwrap();
    let s = session(None, CacheTtl::OneHour);
    let messages = vec![
        Message::user_text("go"),
        assistant(vec![search("s1", "rust")]),
        Message::user_text("next"),
    ];
    let rendering = if rendered {
        closed(RenderingForm::Text)
    } else {
        Rendering::default()
    };
    let unanswered = vec![Message::user_text("go")];
    let messages = if rendered { messages } else { unanswered };
    let request = rendered_request(&s, &messages, &rendering, features);
    let items: Vec<_> = provider.stream(&request).collect().await;
    match items.last() {
        Some(Err(e)) => e.clone(),
        other => panic!("{other:?}"),
    }
}

pub(super) fn invalid(message: &str) -> String {
    json!({"type": "error", "error": {"type": "invalid_request_error", "message": message}})
        .to_string()
}

const BINDING: &str =
    "Invalid `signature` in `thinking` block. The block is bound to a different conversation";
const SPEND: &str = "You have reached your specified workspace API usage limits.";
const START: &str = "event: message_start\ndata: {\"type\":\"message_start\",\"message\":{\"id\":\"m1\",\"model\":\"claude-opus-5-5\",\"usage\":{\"input_tokens\":10,\"output_tokens\":1}}}\n\n";

#[tokio::test]
async fn a_pre_event_invalid_request_on_a_rendered_request_is_a_refused_rendering() {
    let none = Features::default();
    let coded = json!({"type": "error", "error": {"type": "invalid_request_error", "code": "unsupported_parameter", "message": "messages.1: bad block"}});
    let cases = [
        (
            http::json("400 Bad Request", &invalid("messages.1: bad block")),
            ErrorKind::InvalidRequest,
        ),
        (
            http::json("400 Bad Request", &invalid(SPEND)),
            ErrorKind::InvalidRequest,
        ),
        (
            http::json("400 Bad Request", &invalid(BINDING)),
            ErrorKind::ThinkingBinding,
        ),
        (
            http::json("200 OK", &invalid("messages.1: bad block")),
            ErrorKind::InvalidRequest,
        ),
        (
            http::sse(&http::event("error", invalid("messages.1: bad block"))),
            ErrorKind::InvalidRequest,
        ),
        (
            http::json("400 Bad Request", &coded.to_string()),
            ErrorKind::InvalidRequest,
        ),
        (
            http::json("200 OK", &coded.to_string()),
            ErrorKind::InvalidRequest,
        ),
        (
            http::sse(&http::event("error", coded)),
            ErrorKind::InvalidRequest,
        ),
        (
            http::json(
                "400 Bad Request",
                &json!({"error": {"code": "invalid_request_error", "message": "messages.1: bad block"}}).to_string(),
            ),
            ErrorKind::InvalidRequest,
        ),
    ];
    for (response, kind) in cases {
        let e = error_for(response.clone(), true, none).await;
        assert_eq!(e.kind, kind, "{response}");
        assert_eq!(e.refused, Some(Refused::Rendering), "{response}");
        let e = error_for(response.clone(), false, none).await;
        assert_eq!(e.kind, kind, "{response}");
        assert_eq!(e.refused, None, "no rendered block: {response}");
    }
}

#[tokio::test]
async fn other_errors_on_a_rendered_request_are_not_refused_renderings() {
    let none = Features::default();
    let trial = Features {
        binding_controls: Some(FeatureUse::Trial),
        ..Features::default()
    };
    let overflow = r#"{"error":{"message":"too long","type":"invalid_request_error","code":"context_length_exceeded"}}"#;
    let mid_stream = format!(
        "{START}{}",
        http::event("error", invalid("messages.1: bad block"))
    );
    let cases = [
        (
            http::json("400 Bad Request", overflow),
            none,
            ErrorKind::ContextOverflow,
        ),
        (
            http::json(
                "529 Overloaded",
                r#"{"type":"error","error":{"type":"overloaded_error","message":"busy"}}"#,
            ),
            none,
            ErrorKind::Overloaded,
        ),
        (http::sse(&mid_stream), none, ErrorKind::InvalidRequest),
        (
            http::json(
                "400 Bad Request",
                &invalid("thinking.block_binding: Extra inputs are not permitted"),
            ),
            trial,
            ErrorKind::InvalidRequest,
        ),
    ];
    for (response, features, kind) in cases {
        let e = error_for(response.clone(), true, features).await;
        assert_eq!(e.kind, kind, "{response}");
        assert_eq!(e.refused, None, "{response}");
    }
    let rejected = error_for(
        http::json(
            "400 Bad Request",
            &invalid("thinking.block_binding: Extra inputs are not permitted"),
        ),
        true,
        trial,
    )
    .await;
    assert_eq!(rejected.rejected, vec![Feature::BindingControls]);
}

#[tokio::test]
async fn anthropics_prompt_too_long_is_an_overflow_that_never_escalates_in_any_form() {
    let none = Features::default();
    let too_long = invalid("prompt is too long: 210000 tokens > 200000 maximum");
    for response in [
        http::json("400 Bad Request", &too_long),
        http::json("200 OK", &too_long),
        http::sse(&http::event("error", &too_long)),
    ] {
        let e = error_for(response.clone(), true, none).await;
        assert_eq!(e.kind, ErrorKind::ContextOverflow, "{response}");
        assert_eq!(e.refused, None, "{response}");
        assert!(e.rejected.is_empty(), "{response}");
    }
}

#[test]
fn form_3_names_a_file_attachment_without_its_bytes() {
    let mut messages = history();
    let ContentBlock::ToolResult { content, .. } = &mut messages[4].content[0] else {
        panic!("{:?}", messages[4])
    };
    let pdf = "application/pdf".parse().unwrap();
    content[1] = ContentBlock::media(pdf, "JVBERi0xLjQK");
    content.push(ContentBlock::media_sliced(
        "application/pdf".parse().unwrap(),
        "YQ==",
        Some(wisp_protocol::FileSlice::pages(14, 16).unwrap()),
    ));
    let flat = body(&messages, &closed(RenderingForm::Flattened));
    assert_eq!(
        flat["messages"][4]["content"][0],
        json!({"type": "text", "text": "[result of bash: a.txt\n[file]\n[application/pdf file (pages 14-16) not sent: this model can't read the selected part]]"})
    );
    assert!(!flat.to_string().contains("JVBERi0xLjQK"));
    assert!(!flat.to_string().contains("YQ=="));
}
