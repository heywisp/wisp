#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    reason = "offline Responses fixtures fail on invalid fixture data"
)]

use super::*;
use crate::agent::testing::{search_tool, turn};
use crate::endpoints::{EndpointName, PricingMode, UrlSource};
use crate::provider::credential::Source;
use crate::provider::{Assembler, Features, Rendering, RequestOptions};
use crate::test_guard::{
    holding_server, http, log_context, refusing_addr, until_event, until_turn_end, within,
};
use crate::tool::ToolSpec;
use wisp_protocol::RetryClass;
use wisp_protocol::{CacheTtl, Message, MessageTime, SessionConfig};

#[cfg(unix)]
mod media;
mod tools;
mod voice;

const USAGE_HEADERS: [(&str, &str); 3] = [
    ("x-codex-primary-used-percent", "55"),
    ("x-codex-primary-window-minutes", "300"),
    ("x-codex-credits-balance", "HEADER_CANARY"),
];

fn session() -> SessionConfig {
    SessionConfig {
        model: "gpt-6.1-sol".into(),
        system: Some("Stable developer instructions".into()),
        effort: Effort::Medium,
        cache_ttl: CacheTtl::OneHour,
        max_tokens: 8192,
        message_time: MessageTime::Off,
        instructions: None,
        context_note: false,
    }
}

pub(crate) fn endpoint(credential: Credential, base_url: &str) -> ResolvedEndpoint {
    ResolvedEndpoint {
        client_version: None,
        name: EndpointName::parse("openai-test").unwrap(),
        provider: ProviderKind::OpenAi,
        credential,
        base_url: base_url.into(),
        source: Source::Environment,
        key_env: None,
        url_source: UrlSource::Official,
        pricing: PricingMode::List,
    }
}

fn provider() -> OpenAiProvider {
    OpenAiProvider::from_endpoint(&endpoint(
        Credential::ApiKey("synthetic-openai-key".into()),
        DEFAULT_BASE_URL,
    ))
    .unwrap()
}

fn request<'a>(
    session: &'a SessionConfig,
    messages: &'a [Message],
    tools: &'a [ToolSpec],
    rendering: &'a Rendering,
) -> Request<'a> {
    Request {
        session,
        messages,
        tools,
        deferred: &[],
        by_value: &[],
        server_tools: &[],
        previous_response_id: Some("ignored-server-state"),
        features: Features::default(),
        options: RequestOptions::default(),
        rendering,
        compaction: None,
    }
}

pub(crate) fn created() -> Value {
    json!({"type":"response.created","response":{"id":"resp_fixture","model":"gpt-6.1-sol"}})
}
pub(crate) fn completed(items: &[Value]) -> Value {
    json!({"type":"response.completed","response":{"id":"resp_fixture","model":"gpt-6.1-sol","status":"completed","output":items,
    "usage":{"input_tokens":100,"input_tokens_details":{"cached_tokens":30},"output_tokens":20,"output_tokens_details":{"reasoning_tokens":7},"future_usage":19}}})
}
pub(crate) fn message() -> Value {
    json!({"type":"message","id":"msg_fixture","status":"completed","role":"assistant","phase":"final_answer","future":{"kept":true},"content":[{"type":"output_text","text":"Hello","annotations":[],"logprobs":[]}]})
}

#[test]
fn final_items_round_trip_exactly_without_duplicate_projections() {
    let provider = provider();
    let mut machine = Machine::new(
        "gpt-6.1-sol".into(),
        provider.identity.clone(),
        BTreeSet::from(["lookup".into()]),
        false,
    );
    let items = vec![
        json!({"type":"reasoning","id":"rs_fixture","encrypted_content":"synthetic-encrypted-reasoning","summary":[{"type":"summary_text","text":"Checking"}],"future":42}),
        message(),
        json!({"type":"function_call","id":"fc_item","call_id":"call_fixture","name":"lookup","namespace":"wisp","arguments":"{ \"key\": 7 }","status":"completed","future":17}),
        json!({"type":"future_item","id":"future","data":[1,2,3]}),
    ];
    let mut assembler = Assembler::default();
    for event in machine
        .apply(created())
        .unwrap()
        .into_iter()
        .chain(machine.apply(completed(&items)).unwrap())
    {
        assembler.apply(event).unwrap();
    }
    let assembled = assembler.finish().unwrap();
    assert_eq!(assembled.usage.input_tokens, 70);
    assert_eq!(assembled.usage.reasoning_tokens, 7);
    let persisted = serde_json::to_vec(&assembled.clone().into_message()).unwrap();
    let message: Message = serde_json::from_slice(&persisted).unwrap();
    assert_eq!(
        message
            .content
            .iter()
            .filter(|b| matches!(b, ContentBlock::ToolUse { .. }))
            .count(),
        1
    );
    let session = session();
    let messages = vec![Message::user_text("Question"), message];
    let rendering = Rendering::default();
    let body = provider
        .body(&request(&session, &messages, &[], &rendering))
        .unwrap();
    let replay = body["input"].as_array().unwrap();
    assert_eq!(&replay[1..], items.as_slice());
    assert!(body.get("previous_response_id").is_none());
    assert_eq!(body["store"], false);
}

#[test]
fn complete_streamed_text_is_displayed_once_and_must_match_final_item() {
    let mut machine = Machine::new(
        "gpt-6.1-sol".into(),
        "identity".into(),
        BTreeSet::new(),
        false,
    );
    let mut assembler = Assembler::default();
    let mut deltas = String::new();
    for raw in [
        created(),
        json!({"type":"response.output_text.delta","output_index":0,"content_index":0,"delta":"Hello"}),
        done(0, &message()),
        completed(&[message()]),
    ] {
        for event in machine.apply(raw).unwrap() {
            if let ProviderEvent::BlockDelta {
                delta: Delta::Text(text),
                ..
            } = &event
            {
                deltas.push_str(text);
            }
            assembler.apply(event).unwrap();
        }
    }
    assert_eq!(deltas, "Hello");
    assert!(assembler.finish().is_ok());
}

#[test]
fn incomplete_failed_malformed_and_duplicate_tools_never_complete() {
    for raw in [
        json!({"type":"response.failed","response":{"error":{"code":"server_error"}}}),
        json!({"type":"response.incomplete","response":{"incomplete_details":{"reason":"max_output_tokens"}}}),
    ] {
        let mut machine = Machine::new("gpt-6.1-sol".into(), "id".into(), BTreeSet::new(), false);
        machine.apply(created()).unwrap();
        let events = machine.apply(raw).unwrap();
        assert!(
            !events
                .iter()
                .any(|event| matches!(event, ProviderEvent::MessageStop))
        );
        assert!(machine.failure.is_some());
    }
    let mut machine = Machine::new(
        "gpt-6.1-sol".into(),
        "id".into(),
        BTreeSet::from(["lookup".into()]),
        false,
    );
    machine.apply(created()).unwrap();
    let call =
        json!({"type":"function_call","call_id":"call_fixture","name":"lookup","arguments":"{}"});
    machine.finalize(0, call.clone()).unwrap();
    assert!(machine.finalize(1, call).is_err());
    let mut machine = Machine::new(
        "gpt-6.1-sol".into(),
        "id".into(),
        BTreeSet::from(["lookup".into()]),
        false,
    );
    machine.apply(created()).unwrap();
    assert!(
        machine
            .finalize(
                0,
                json!({"type":"function_call","call_id":"bad","name":"lookup","arguments":"{"})
            )
            .is_err()
    );
    let failed = json!({"type":"function_call","call_id":"call_failed","name":"lookup","arguments":"{}","status":"failed"});
    assert!(machine.finalize(1, failed).is_err());
    let typeless = machine.apply(json!({"foo":1})).unwrap_err();
    assert_eq!(typeless.message, "OpenAI event has no type");
}

#[test]
fn subscription_request_uses_reference_profile_without_api_billing_controls() {
    let provider = subscription();
    let session = session();
    let messages = vec![Message::user_text("Hello")];
    let rendering = Rendering::default();
    let body = provider
        .body(&request(&session, &messages, &[], &rendering))
        .unwrap();
    assert_eq!(
        provider.url,
        "https://chatgpt.com/backend-api/codex/responses"
    );
    for field in [
        "max_output_tokens",
        "temperature",
        "previous_response_id",
        "prompt_cache_retention",
        "user",
    ] {
        assert!(body.get(field).is_none(), "unexpected {field}");
    }
    assert_eq!(body["include"], json!(["reasoning.encrypted_content"]));
    assert!(!body.to_string().contains("synthetic-oauth"));
    assert!(
        OpenAiProvider::from_endpoint(&endpoint(
            provider.credential.clone(),
            "https://example.invalid"
        ))
        .is_err()
    );
}

#[test]
fn tools_are_canonical_and_refresh_identity_does_not_change_prompt_prefix() {
    let provider = provider();
    let session = session();
    let messages = vec![Message::user_text("Hello")];
    let rendering = Rendering::default();
    let a = ToolSpec {
        name: "a".into(),
        description: "first".into(),
        input_schema: json!({"type":"object","properties":{}}),
    };
    let z = ToolSpec {
        name: "z".into(),
        description: "last".into(),
        input_schema: json!({"type":"object","properties":{}}),
    };
    let first = [z.clone(), a.clone()];
    let second = [a, z];
    assert_eq!(
        provider.prefix_bytes(&request(&session, &messages, &first, &rendering)),
        provider.prefix_bytes(&request(&session, &messages, &second, &rendering))
    );
    let mut searching = request(&session, &messages, &first, &rendering);
    searching.server_tools = search_tool("web_search");
    let body = provider.body(&searching).unwrap();
    let kinds: Vec<_> = body["tools"]
        .as_array()
        .unwrap()
        .iter()
        .map(|t| t.get("name").unwrap_or(&t["type"]).clone())
        .collect();
    assert_eq!(kinds, [json!("a"), json!("web_search"), json!("z")]);
    assert_eq!(body["tools"][1], json!({"type":"web_search"}));
    assert_eq!(
        body["include"],
        json!([
            "reasoning.encrypted_content",
            "web_search_call.action.sources"
        ])
    );
    let mut machine = Machine::new(session.model.clone(), "id".into(), BTreeSet::new(), true);
    machine.apply(created()).unwrap();
    let events = machine.apply(completed(&[message()])).unwrap();
    assert!(events.iter().any(|e| matches!(e,ProviderEvent::MessageDelta {usage,..} if usage.billing==wisp_protocol::Billing::ChatGptPlan)));
}

#[test]
fn thinking_cuts_retain_user_messages_native_text_and_matching_tool_pairs() {
    let provider = provider();
    let mut machine = Machine::new(
        "gpt-6.1-sol".into(),
        provider.identity.clone(),
        BTreeSet::from(["lookup".into()]),
        false,
    );
    let mut assembler = Assembler::default();
    let items = vec![
        json!({"type":"reasoning","id":"rs_cut","encrypted_content":"ciphertext","summary":[{"type":"summary_text","text":"Thinking"}]}),
        message(),
        json!({"type":"function_call","id":"fc_cut","call_id":"call_cut","name":"lookup","arguments":"{}"}),
    ];
    for event in machine
        .apply(created())
        .unwrap()
        .into_iter()
        .chain(machine.apply(completed(&items)).unwrap())
    {
        assembler.apply(event).unwrap();
    }
    let messages = vec![
        Message::user_text("Keep this question"),
        assembler.finish().unwrap().into_message(),
        Message {
            role: Role::User,
            content: vec![ContentBlock::ToolResult {
                tool_use_id: "call_cut".into(),
                content: vec![ContentBlock::Text {
                    text: "Keep this result".into(),
                }],
                is_error: false,
            }],
        },
    ];
    let rendering = Rendering {
        cuts: vec![wisp_protocol::log::Cut {
            from: wisp_protocol::log::BlockPosition {
                message: 0,
                block: 0,
            },
            through: 2,
            tools: true,
        }],
        ..Rendering::default()
    };
    let session = session();
    let body = provider
        .body(&request(&session, &messages, &[], &rendering))
        .unwrap();
    let input = body["input"].as_array().unwrap();
    assert!(
        input
            .iter()
            .any(|item| item["content"][0]["text"] == "Keep this question")
    );
    assert!(input.contains(&items[1]));
    assert!(input.contains(&items[2]));
    assert!(!input.contains(&items[0]));
    assert!(
        input
            .iter()
            .any(|item| item["output"] == "Keep this result")
    );
}

#[test]
fn tool_result_images_are_preserved_and_foreign_nested_blocks_become_text() {
    let provider = provider();
    let session = session();
    let rendering = Rendering::default();
    let image = ContentBlock::image(wisp_protocol::ImageType::Png, "synthetic-image");
    let result = |content| Message {
        role: Role::User,
        content: vec![ContentBlock::ToolResult {
            tool_use_id: "call_image".into(),
            content,
            is_error: false,
        }],
    };
    let input = |messages: &[Message]| {
        provider
            .body(&request(&session, messages, &[], &rendering))
            .unwrap()["input"]
            .clone()
    };
    let screenshot = ContentBlock::Text {
        text: "Screenshot".into(),
    };
    let messages = vec![
        Message::user_text("Look"),
        Message {
            role: Role::Assistant,
            content: vec![ContentBlock::Text {
                text: "Looking".into(),
            }],
        },
        result(vec![screenshot.clone(), image.clone()]),
    ];
    assert_eq!(
        input(&messages).to_string(),
        r#"[{"content":[{"text":"Look","type":"input_text"}],"role":"user"},{"content":[{"text":"Looking","type":"output_text"}],"role":"assistant"},{"call_id":"call_image","output":[{"text":"Screenshot","type":"input_text"},{"image_url":"data:image/png;base64,synthetic-image","type":"input_image"}],"type":"function_call_output"}]"#
    );
    let png = ContentBlock::media("image/png".parse().unwrap(), "synthetic-image");
    assert_eq!(
        input(&[result(vec![image.clone()])]),
        input(&[result(vec![png.clone()])])
    );
    assert_eq!(
        input(&[Message {
            role: Role::User,
            content: vec![image]
        }]),
        input(&[Message {
            role: Role::User,
            content: vec![png.clone()]
        }])
    );
    let notes = Rendering {
        attachments_as_notes: true,
        ..Rendering::default()
    };
    let pdf = ContentBlock::media("application/pdf".parse().unwrap(), "synthetic-pdf");
    let noted = provider
        .body(&request(
            &session,
            &[
                Message {
                    role: Role::User,
                    content: vec![png, pdf],
                },
                result(vec![
                    screenshot.clone(),
                    ContentBlock::image(wisp_protocol::ImageType::Png, "synthetic-image"),
                ]),
            ],
            &[],
            &notes,
        ))
        .unwrap()["input"]
        .to_string();
    let note = |t: &str| {
        format!(r#"{{"text":"[{t} file not sent: this model can't take it]","type":"input_text"}}"#)
    };
    assert!(!noted.contains("synthetic-"), "{noted}");
    assert_eq!(noted.matches(&note("image/png")).count(), 2, "{noted}");
    assert_eq!(
        noted.matches(&note("application/pdf")).count(),
        1,
        "{noted}"
    );
    assert!(MEDIA_TYPES.is_sorted());
    assert_eq!(
        FILES.map(|(media_type, filename)| {
            assert!(MEDIA_TYPES.contains(&media_type), "{media_type}");
            filename
        }),
        [
            "file.pdf",
            "file.odt",
            "file.pptx",
            "file.xlsx",
            "file.docx"
        ]
    );
    assert!(MEDIA_TYPES.contains(&"image/webp"));
    for subscription in [false, true] {
        let mut provider = provider.clone();
        provider.subscription = subscription;
        let carried = provider.media_types();
        let input = |messages: &[Message]| {
            provider
                .body(&request(&session, messages, &[], &rendering))
                .unwrap()["input"]
                .clone()
        };
        let all = crate::provider::anthropic::media::MEDIA_TYPES.iter();
        for media_type in all.chain(MEDIA_TYPES.iter()) {
            let block = ContentBlock::media(media_type.parse().unwrap(), "synthetic-data");
            let url = format!("data:{media_type};base64,synthetic-data");
            let file = FILES.iter().find(|(listed, ..)| listed == media_type);
            let expected = MEDIA_TYPES.contains(media_type);
            assert_eq!(carried.contains(media_type), expected, "{media_type}");
            let part = match file {
                _ if !expected => {
                    json!({"type":"input_text","text":format!("[{media_type} file not sent: this server can't carry it]")})
                }
                Some((_, filename)) => {
                    json!({"type":"input_file","filename":filename,"file_data":url})
                }
                None => json!({"type":"input_image","image_url":url}),
            };
            assert_eq!(
                input(&[result(vec![block.clone()])])[0]["output"],
                json!([part])
            );
            assert_eq!(
                input(&[Message {
                    role: Role::User,
                    content: vec![block]
                }])[0]["content"],
                json!([part])
            );
        }
        use wisp_protocol::FileSlice;
        for (kind, slice) in [
            ("application/pdf", FileSlice::pages(14, 16).unwrap()),
            (
                "application/vnd.openxmlformats-officedocument.wordprocessingml.document",
                FileSlice::pages(2, 2).unwrap(),
            ),
            (
                "application/vnd.openxmlformats-officedocument.spreadsheetml.sheet",
                FileSlice::sheet(2).unwrap(),
            ),
            ("audio/mpeg", FileSlice::seconds(30, 90).unwrap()),
        ] {
            let block =
                ContentBlock::media_sliced(kind.parse().unwrap(), "synthetic-data", Some(slice));
            let part = json!({"type":"input_text","text":format!("[{kind} file ({slice}) not sent: this model can't read the selected part]")});
            assert_eq!(
                input(&[result(vec![block.clone()])])[0]["output"],
                json!([part])
            );
            assert_eq!(
                input(&[Message {
                    role: Role::User,
                    content: vec![block]
                }])[0]["content"],
                json!([part])
            );
        }
    }
    let foreign = ContentBlock::opaque("other", json!({"type":"unsupported_media"}));
    let input = input(&[result(vec![screenshot, foreign])]);
    assert!(
        input[0]["output"][1]["text"]
            .as_str()
            .unwrap()
            .contains("unsupported_media")
    );
}

#[tokio::test]
async fn real_agent_can_summarize_and_continue_after_client_compaction() {
    use std::sync::Arc;
    use wisp_protocol::{AgentEvent, ClientCommand, RequestId};
    let replies = (0..3)
        .map(|n| {
            let mut item = message();
            item["content"][0]["text"]=if n==1 {"## Goal and standing instructions\nSummary of previous conversation\n## Decisions\nNone\n## State of the work\nReady\n## Files, branches and artefacts\nNone\n## Commands, errors and fixes\nNone\n## Open questions\nNone"} else {"Hello"}.into();
            if n == 0 {
                item["content"][0]["text"] = "Earlier assistant context. ".repeat(2000).into();
            }
            let mut values = [created(), completed(&[item])];
            values[1]["response"]["usage"]["input_tokens"] =
                if n < 2 { 10_000 } else { 100 }.into();
            stream_reply("200 OK", &[], &data(&values))
        })
        .collect();
    let server = http::serve(replies).await;
    let provider = OpenAiProvider::from_endpoint(&endpoint(
        Credential::ApiKey("synthetic-key".into()),
        &server.url,
    ))
    .unwrap();
    let agent = crate::agent::Agent::new(Arc::new(provider), session());
    let (commands, mut events, _agent) = crate::agent::spawn(agent);
    commands
        .send(ClientCommand::send("first", "Remember this fact"))
        .await
        .unwrap();
    until_turn_end(&mut events, &mut Vec::new()).await;
    commands
        .send(ClientCommand::Compact {
            focus: None,
            request: RequestId(1),
        })
        .await
        .unwrap();
    let compacted = until_event(
        &mut events,
        &mut Vec::new(),
        "the compaction's answer",
        |e| {
            matches!(
                e,
                AgentEvent::Compacted { .. }
                    | AgentEvent::CompactionFailed { .. }
                    | AgentEvent::Rejected { .. }
            )
        },
    )
    .await;
    assert!(
        matches!(compacted, AgentEvent::Compacted { .. }),
        "{compacted:?}"
    );
    commands
        .send(ClientCommand::send("second", "Continue"))
        .await
        .unwrap();
    until_turn_end(&mut events, &mut Vec::new()).await;
    let requests = bodies(&server);
    assert_eq!(requests.len(), 3);
    assert!(
        requests[2]
            .to_string()
            .contains("Summary of previous conversation")
    );
    assert!(
        requests
            .iter()
            .all(|request| request.get("previous_response_id").is_none())
    );
}

async fn single_server(values: Option<Vec<Value>>) -> (OpenAiProvider, http::Server) {
    scripted_server(vec![values]).await
}

async fn scripted_server(responses: Vec<Option<Vec<Value>>>) -> (OpenAiProvider, http::Server) {
    let replies = responses
        .into_iter()
        .map(|values| match values {
            Some(values) => stream_reply("200 OK", &[], &data(&values)),
            None => http::Reply::Hang,
        })
        .collect();
    serving(replies).await
}

async fn serving(replies: Vec<http::Reply>) -> (OpenAiProvider, http::Server) {
    let server = http::serve(replies).await;
    let provider = OpenAiProvider::from_endpoint(&endpoint(
        Credential::ApiKey("synthetic-key".into()),
        &server.url,
    ))
    .unwrap();
    (provider, server)
}

pub(crate) fn data(values: &[Value]) -> String {
    values
        .iter()
        .map(|value| format!("data: {value}\n\n"))
        .collect()
}

fn stream_reply(status: &str, headers: &[(&str, &str)], body: &str) -> http::Reply {
    let mut headers = headers.to_vec();
    headers.push(("content-type", "text/event-stream"));
    http::Reply::Send(http::response(status, &headers, body))
}

fn asked(server: &http::Server) -> usize {
    server.requests.lock().unwrap().len()
}

fn bodies(server: &http::Server) -> Vec<Value> {
    server
        .requests
        .lock()
        .unwrap()
        .iter()
        .map(|request| serde_json::from_str(http::body(request)).unwrap())
        .collect()
}

fn assert_resent(server: &http::Server) {
    let requests = server.requests.lock().unwrap();
    assert_eq!(requests.len(), 2);
    assert_eq!(http::body(&requests[0]), http::body(&requests[1]));
}

async fn refreshing_subscription(
    replies: Vec<http::Reply>,
) -> (tempfile::TempDir, PathBuf, OpenAiProvider, http::Server) {
    let (folder, path) = crate::oauth::valid_chatgpt_profile();
    let (provider, server) = serving(replies).await;
    let mut provider = provider.on_subscription();
    provider.credential = Credential::Subscription {
        kind: SubscriptionKind::ChatGpt,
        secret: String::new(),
        profile: Some(path.clone()),
    };
    (folder, path, provider, server)
}

fn refresh_at(
    path: PathBuf,
    url: String,
) -> impl FnOnce(Secret) -> BoxFuture<'static, Result<String, ProviderError>> {
    move |refused| {
        async move { crate::oauth::refresh_refused_at(&path, &url, refused).await }.boxed()
    }
}

#[tokio::test]
async fn a_subscription_401_refreshes_once_and_replays_identical_bytes_without_duplicate_events() {
    let (_folder, path, provider, server) = refreshing_subscription(vec![
        stream_reply("401 Unauthorized", &USAGE_HEADERS, "{}"),
        stream_reply("200 OK", &[], &data(&[created(), completed(&[message()])])),
    ])
    .await;
    let token_server = http::serve(vec![http::Reply::Send(http::json(
        "200 OK",
        r#"{"access_token":"synthetic-new-access","refresh_token":"synthetic-new-refresh","expires_in":3600}"#,
    ))])
    .await;
    let session = session();
    let messages = [Message::user_text("Hello")];
    let rendering = Rendering::default();
    let request = request(&session, &messages, &[], &rendering);
    let events = within(
        "the refreshed subscription response",
        provider
            .stream_refreshing(&request, refresh_at(path, token_server.url.clone()))
            .collect::<Vec<_>>(),
    )
    .await
    .into_iter()
    .collect::<Result<Vec<_>, _>>()
    .unwrap();
    assert_resent(&server);
    assert_eq!(asked(&token_server), 1);
    let requests = server.requests.lock().unwrap();
    let mut first = http::header_lines(&requests[0]);
    let mut second = http::header_lines(&requests[1]);
    assert!(first.contains(&("authorization".into(), "Bearer synthetic-access-0".into())));
    assert!(second.contains(&("authorization".into(), "Bearer synthetic-new-access".into())));
    first.retain(|(name, _)| name != "authorization");
    second.retain(|(name, _)| name != "authorization");
    assert_eq!(first, second);
    assert_eq!(
        events
            .iter()
            .filter(|e| matches!(e, ProviderEvent::Sending))
            .count(),
        1
    );
    assert_eq!(
        events
            .iter()
            .filter(|e| matches!(e, ProviderEvent::MessageStart { .. }))
            .count(),
        1
    );
    assert_eq!(
        events
            .iter()
            .filter(|e| matches!(e, ProviderEvent::MessageDelta { .. }))
            .count(),
        1
    );
    assert_eq!(
        events
            .iter()
            .filter(|e| matches!(e, ProviderEvent::MessageStop))
            .count(),
        1
    );
    assert!(
        !events
            .iter()
            .any(|e| matches!(e, ProviderEvent::PlanUsage(_)))
    );
    let mut assembler = Assembler::default();
    for event in events {
        assembler.apply(event).unwrap();
    }
    let assembled = assembler.finish().unwrap();
    assert_eq!(assembled.usage.input_tokens, 70);
    assert_eq!(assembled.usage.output_tokens, 20);
    assert_eq!(assembled.usage.billing, wisp_protocol::Billing::ChatGptPlan);
}

#[tokio::test]
async fn a_second_subscription_401_says_to_sign_in_again_without_a_third_request() {
    let (_folder, path, provider, server) = refreshing_subscription(vec![
        stream_reply("401 Unauthorized", &[], "{}"),
        stream_reply("401 Unauthorized", &[], "{}"),
    ])
    .await;
    let token_server = http::serve(vec![http::Reply::Send(http::json(
        "200 OK",
        r#"{"access_token":"synthetic-new-access","refresh_token":"synthetic-new-refresh","expires_in":3600}"#,
    ))])
    .await;
    let session = session();
    let messages = [Message::user_text("Hello")];
    let rendering = Rendering::default();
    let events = within(
        "the second subscription refusal",
        provider
            .stream_refreshing(
                &request(&session, &messages, &[], &rendering),
                refresh_at(path, token_server.url.clone()),
            )
            .collect::<Vec<_>>(),
    )
    .await;
    let error = events.into_iter().find_map(Result::err).unwrap();
    assert_eq!(error.kind, ErrorKind::Auth);
    assert_eq!(error.detail.status, Some(401));
    assert_eq!(error.detail.retry, RetryClass::Never);
    assert_eq!(
        error.message,
        "ChatGPT subscription authentication failed; sign in again with /account"
    );
    assert_resent(&server);
    assert_eq!(asked(&token_server), 1);
}

#[tokio::test]
async fn a_refused_subscription_refresh_says_to_sign_in_again_and_keeps_the_refusal_mark() {
    for status in ["400 Bad Request", "401 Unauthorized"] {
        let (_folder, path, provider, server) = refreshing_subscription(vec![
            stream_reply("401 Unauthorized", &USAGE_HEADERS, "{}"),
            stream_reply("401 Unauthorized", &USAGE_HEADERS, "{}"),
        ])
        .await;
        let before = std::fs::read(&path).unwrap();
        let token_server = http::serve(vec![http::Reply::Send(http::json(
            status,
            r#"{"error":"REFRESH_PRIVATE_CANARY","access_token":"REFRESH_PRIVATE_CANARY"}"#,
        ))])
        .await;
        let session = session();
        let messages = [Message::user_text("Hello")];
        let rendering = Rendering::default();
        let request = request(&session, &messages, &[], &rendering);
        let (first, second) = within("the concurrent refused subscription refresh", async {
            tokio::join!(
                provider
                    .stream_refreshing(&request, refresh_at(path.clone(), token_server.url.clone()))
                    .collect::<Vec<_>>(),
                provider
                    .stream_refreshing(&request, refresh_at(path.clone(), token_server.url.clone()))
                    .collect::<Vec<_>>()
            )
        })
        .await;
        for events in [first, second] {
            assert!(
                !events
                    .iter()
                    .any(|e| matches!(e, Ok(ProviderEvent::PlanUsage(_))))
            );
            let error = events.into_iter().find_map(Result::err).unwrap();
            assert_eq!(error.kind, ErrorKind::Auth);
            assert_eq!(
                error.message,
                "ChatGPT subscription authentication failed; sign in again with /account"
            );
            assert!(!format!("{error:?}").contains("CANARY"));
        }
        assert_eq!(asked(&server), 2);
        assert_eq!(asked(&token_server), 1);
        assert!(crate::oauth::refused(&path));
        assert_eq!(std::fs::read(&path).unwrap(), before);
    }
}

#[tokio::test]
async fn an_api_key_401_and_a_subscription_403_are_never_refreshed_or_replayed() {
    for (subscription, status) in [(false, "401 Unauthorized"), (true, "403 Forbidden")] {
        let (mut provider, server) = serving(vec![stream_reply(status, &[], "{}")]).await;
        provider.subscription = subscription;
        let session = session();
        let messages = [Message::user_text("Hello")];
        let rendering = Rendering::default();
        let events = provider
            .stream_refreshing(&request(&session, &messages, &[], &rendering), |_| async {
                panic!("this refusal must not refresh the token")
            })
            .collect::<Vec<_>>()
            .await;
        let error = events.into_iter().find_map(Result::err).unwrap();
        assert_eq!(error.kind, ErrorKind::Auth);
        assert_eq!(
            error.detail.status,
            Some(if subscription { 403 } else { 401 })
        );
        assert_eq!(
            error.message,
            if subscription {
                "ChatGPT subscription authentication failed; sign in again with /account"
            } else {
                "OpenAI request refused (HTTP 401)"
            }
        );
        assert_eq!(asked(&server), 1);
    }
}

#[tokio::test]
async fn a_subscription_without_a_profile_reports_sign_in_again_without_replaying() {
    let (provider, server) = serving(vec![stream_reply("401 Unauthorized", &[], "{}")]).await;
    let mut provider = provider.on_subscription();
    provider.credential = Credential::Subscription {
        kind: SubscriptionKind::ChatGpt,
        secret: "synthetic-unmanaged-token".into(),
        profile: None,
    };
    let session = session();
    let messages = [Message::user_text("Hello")];
    let rendering = Rendering::default();
    let events = provider
        .stream(&request(&session, &messages, &[], &rendering))
        .collect::<Vec<_>>()
        .await;
    let error = events.into_iter().find_map(Result::err).unwrap();
    assert_eq!(error.kind, ErrorKind::Auth);
    assert_eq!(error.detail.retry, RetryClass::Never);
    assert_eq!(
        error.message,
        "ChatGPT subscription authentication failed; sign in again with /account"
    );
    assert_eq!(asked(&server), 1);
}

#[tokio::test]
async fn incomplete_agent_step_keeps_reported_usage_without_committing_a_reply() {
    use std::sync::Arc;
    use wisp_protocol::{AgentEvent, TurnOutcome};
    let terminal = json!({"type":"response.incomplete","response":{"status":"incomplete","incomplete_details":{"reason":"max_output_tokens"},"usage":{"input_tokens":90,"input_tokens_details":{"cached_tokens":20},"output_tokens":18,"output_tokens_details":{"reasoning_tokens":8}}}});
    let (provider, server) = single_server(Some(vec![created(), terminal])).await;
    let mut agent = crate::agent::Agent::new(Arc::new(provider), session());
    let (outcome, events) = turn(&mut agent, "Hello", &[]).await;
    let TurnOutcome::Error {
        message,
        committed_steps,
        ..
    } = outcome
    else {
        panic!("unexpected outcome: {outcome:?}");
    };
    assert_eq!(committed_steps, 0);
    assert_eq!(
        message,
        "OpenAI did not complete the response: max_output_tokens"
    );
    assert_eq!(asked(&server), 1);
    let mut usage = None;
    for event in events {
        if let AgentEvent::StepEnded {
            usage: reported, ..
        } = event
        {
            usage = reported;
        }
        assert!(!matches!(event, AgentEvent::ToolCallFinished { .. }));
    }
    let usage = usage.unwrap();
    assert_eq!(usage.input_tokens, 70);
    assert_eq!(usage.output_tokens, 18);
    assert_eq!(usage.reasoning_tokens, 8);
}

#[tokio::test]
async fn cancellation_after_request_before_headers_is_possibly_billed() {
    use std::sync::Arc;
    use wisp_protocol::{AgentEvent, ClientCommand, TurnOutcome};
    let (provider, mut server) = single_server(None).await;
    let (commands, mut events, _agent) =
        crate::agent::spawn(crate::agent::Agent::new(Arc::new(provider), session()));
    commands
        .send(ClientCommand::send("first", "Hello"))
        .await
        .unwrap();
    server.next().await;
    commands.send(ClientCommand::Cancel).await.unwrap();
    let mut seen = Vec::new();
    let ended = until_turn_end(&mut events, &mut seen).await;
    assert!(
        matches!(
            ended,
            AgentEvent::TurnEnded {
                outcome: TurnOutcome::Cancelled { .. },
                ..
            }
        ),
        "{ended:?}"
    );
    assert!(seen.iter().any(|e| matches!(
        e,
        AgentEvent::StepEnded {
            lower_bound: true,
            ..
        }
    )));
}

#[test]
fn completed_response_requires_authoritative_complete_output_coverage() {
    let mut machine = Machine::new(
        "gpt-6.1-sol".into(),
        "id".into(),
        BTreeSet::from(["lookup".into()]),
        false,
    );
    machine.apply(created()).unwrap();
    assert!(
        machine
            .apply(json!({"type":"response.completed","response":{"status":"completed"}}))
            .is_err()
    );
    let mut machine = Machine::new(
        "gpt-6.1-sol".into(),
        "id".into(),
        BTreeSet::from(["lookup".into()]),
        false,
    );
    machine.apply(created()).unwrap();
    let call = json!({"type":"function_call","id":"fc","call_id":"call","name":"lookup","arguments":"{}","status":"completed"});
    machine.apply(done(0, &call)).unwrap();
    assert!(machine.apply(completed(&[])).is_err());
    assert!(!machine.terminal);
    let empty = |stream: &[Value]| {
        let mut machine = Machine::new("gpt-6.1-sol".into(), "id".into(), BTreeSet::new(), true);
        machine.apply(created()).unwrap();
        for raw in stream {
            machine.apply(raw.clone()).unwrap();
        }
        (machine.apply(completed(&[])).is_ok(), machine.terminal)
    };
    let item = message();
    assert_eq!(empty(&[added(0, &item), done(0, &item)]), (true, true));
    assert_eq!(
        empty(&[added(0, &item), added(1, &item), done(0, &item)]),
        (false, false)
    );
}

#[test]
fn cache_write_ttls_and_reasoning_usage_are_counted_once() {
    let usage = usage(
        &json!({"input_tokens":12000,"input_tokens_details":{"cached_tokens":4000,"cache_write_tokens":7000,"cache_write_tokens_details":{"ttl_30m_tokens":5000,"ttl_12h_tokens":2000}},"output_tokens":200,"output_tokens_details":{"reasoning_tokens":40}}),
    );
    assert_eq!(usage.input_tokens, 1000);
    assert_eq!(usage.prompt_tokens(), 12000);
    assert_eq!(usage.output_tokens, 200);
    assert_eq!(usage.reasoning_tokens, 40);
    let info = crate::pricing::model_info("gpt-6.1-sol").unwrap();
    let cost = crate::pricing::estimate_usd(info, &usage, CacheTtl::OneHour);
    assert!((cost - 0.0269).abs() < 1e-10, "{cost}");
}

#[tokio::test]
async fn stalled_or_oversized_http_error_bodies_keep_the_http_classification() {
    let unauthorized =
        "HTTP/1.1 401 Unauthorized\r\nTransfer-Encoding: chunked\r\nConnection: close\r\n\r\n";
    let oversized = format!(
        "{unauthorized}{:x}\r\n{}\r\n0\r\n\r\n",
        1024 * 1024 + 1,
        "x".repeat(1024 * 1024 + 1)
    );
    for head in [unauthorized.to_owned(), oversized] {
        let (address, server) = holding_server(head).await;
        let client = Client::new();
        let response = client
            .get(format!("http://{address}/"))
            .send()
            .await
            .unwrap();
        let error = tokio::time::timeout(
            Duration::from_secs(7),
            http_error(response, false, 1_800_000_000, Carries::default()),
        )
        .await
        .unwrap();
        assert_eq!(error.kind, ErrorKind::Auth);
        assert_eq!(error.detail.status, Some(401));
        assert_eq!(error.detail.retry, RetryClass::Never);
        server.abort();
    }
}

#[tokio::test]
async fn an_openai_stream_that_stalls_after_its_first_event_times_out() {
    let (address, server) = holding_server(format!(
        "HTTP/1.1 200 OK\r\ncontent-type: text/event-stream\r\nx-request-id: req_stall\r\n\r\ndata: {}\n\n",
        created()
    ))
    .await;
    let mut provider = OpenAiProvider::from_endpoint(&endpoint(
        Credential::ApiKey("synthetic-key".into()),
        &format!("http://{address}"),
    ))
    .unwrap();
    provider.waits.idle = Duration::from_millis(50);
    let session = session();
    let rendering = Rendering::default();
    let stream = provider.stream(&request(&session, &[], &[], &rendering));
    let events: Vec<_> = within("the stream's end", stream.collect()).await;
    server.abort();
    let [
        Ok(ProviderEvent::Sending),
        Ok(ProviderEvent::MessageStart { .. }),
        ..,
        Err(error),
    ] = events.as_slice()
    else {
        panic!("{events:?}")
    };
    assert_eq!(error.kind, ErrorKind::Timeout);
    assert_eq!(error.detail.status, Some(200));
    assert_eq!(error.detail.request_id.as_deref(), Some("req_stall"));
}

#[tokio::test]
async fn an_openai_connection_that_never_opens_is_one_unbilled_attempt() {
    let (_socket, address) = refusing_addr();
    let provider = OpenAiProvider::from_endpoint(&endpoint(
        Credential::ApiKey("synthetic-key".into()),
        &format!("http://{address}/CANARYpath"),
    ))
    .unwrap();
    let session = session();
    let rendering = Rendering::default();
    let events: Vec<_> = provider
        .stream(&request(&session, &[], &[], &rendering))
        .collect()
        .await;
    let [Ok(ProviderEvent::Sending), Err(error)] = events.as_slice() else {
        panic!("{events:?}")
    };
    assert_eq!(
        (error.kind, error.detail.phase, error.detail.retry),
        (
            ErrorKind::Network,
            FailurePhase::Connect,
            RetryClass::Transient
        )
    );
    assert!(!error.possibly_billed);
    assert!(!error.message.contains("CANARY"), "{}", error.message);
    assert!(!error.message.contains("127.0.0.1"), "{}", error.message);
}

#[tokio::test]
async fn an_openai_503_overload_busy_waits_unbilled() {
    use crate::agent::retry;
    use std::sync::Arc;
    use wisp_protocol::{AgentEvent, TurnOutcome};
    let refused = r#"{"error":{"code":"server_is_overloaded"},"request_id":"req_body"}"#;
    let ids = [
        ("x-request-id", "not-a-req-id"),
        ("request-id", "req_other"),
    ];
    let (provider, server) = serving(vec![
        stream_reply("503 Service Unavailable", &ids, refused),
        stream_reply("200 OK", &ids, &data(&[created(), completed(&[message()])])),
    ])
    .await;
    let policy = retry::RetryPolicy {
        busy_window: retry::RetryPolicy::default().for_fallback().busy_window,
        ..retry::quick()
    };
    let mut agent =
        crate::agent::Agent::new(Arc::new(provider), session()).with_retry_policy(policy);
    let (outcome, events) = turn(&mut agent, "hello", &[]).await;
    let mut retried = Vec::new();
    for event in events {
        if let AgentEvent::Retrying {
            busy, kind, detail, ..
        } = event
        {
            retried.push((
                busy.is_some(),
                kind,
                detail.map(|d| (d.possibly_billed, d.request_id)),
            ));
        }
    }
    assert_eq!(
        retried,
        vec![(true, ErrorKind::Overloaded, Some((false, None)))]
    );
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
    assert_resent(&server);
}

#[tokio::test]
async fn an_openai_agent_rejects_a_claude_model_at_the_switch_and_at_send() {
    use std::sync::Arc;
    use wisp_protocol::settings::{SettingChange, SettingValue};
    use wisp_protocol::{AgentEvent, ClientCommand, RequestId};
    let (commands, mut events, _agent) =
        crate::agent::spawn(crate::agent::Agent::new(Arc::new(provider()), session()));
    commands
        .send(ClientCommand::ChangeSettings {
            request: RequestId(1),
            changes: vec![
                SettingChange::named("model", SettingValue::Text("claude-opus-5-5".into()))
                    .unwrap(),
            ],
            save: false,
        })
        .await
        .unwrap();
    let event = until_event(&mut events, &mut Vec::new(), "the refusal", |e| {
        matches!(e, AgentEvent::Rejected { .. })
    })
    .await;
    assert!(
        matches!(event,AgentEvent::Rejected {reason,..} if reason.contains("Anthropic account"))
    );
    let claude = SessionConfig {
        model: "claude-opus-5-5".into(),
        ..session()
    };
    let refused = provider()
        .body(&request(&claude, &[], &[], &Rendering::default()))
        .unwrap_err();
    assert_eq!(refused.kind, ErrorKind::InvalidRequest);
    assert_eq!(
        refused.message,
        format!("select an OpenAI model for this account, such as {DEFAULT_MODEL}")
    );
}

#[tokio::test]
async fn reasoning_summary_and_final_text_commit_the_agent_turn() {
    reasoning_summary_agent_turn(false).await;
}

#[tokio::test]
async fn reasoning_summary_and_tool_call_commit_and_execute() {
    reasoning_summary_agent_turn(true).await;
}

async fn reasoning_summary_agent_turn(with_tool: bool) {
    use std::sync::Arc;
    use wisp_protocol::{AgentEvent, TurnOutcome};

    let reasoning = json!({"type":"reasoning","id":"rs_summary","encrypted_content":"synthetic-replay","summary":[{"type":"summary_text","text":"Checking"}]});
    let call = json!({"type":"function_call","id":"fc_echo","call_id":"call_echo","name":"echo","arguments":"{\"text\":\"echo ran\"}","status":"completed"});
    let items = vec![reasoning.clone(), if with_tool { call } else { message() }];
    let mut responses = vec![Some(vec![created(), completed(&items)])];
    if with_tool {
        responses.push(Some(vec![created(), completed(&[reasoning, message()])]));
    }
    let (provider, server) = scripted_server(responses).await;
    let scratch = tempfile::tempdir().unwrap();
    let dir = scratch.path().to_path_buf();
    let workspace = crate::workspace::Workspace {
        root: dir.clone(),
        home: None,
        temp: dir,
    };
    let mut agent = crate::agent::Agent::new(Arc::new(provider), session())
        .with_tools(crate::tool::testkit::toolbox(&["echo"]), workspace);
    let (outcome, events) = turn(&mut agent, "Hello", &[]).await;
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
    assert_eq!(asked(&server), 1 + usize::from(with_tool));
    let tool_finished = events
        .iter()
        .any(|event| matches!(event, AgentEvent::ToolCallFinished { .. }));
    assert_eq!(tool_finished, with_tool);
    assert!(agent.history().iter().any(|message| message.content.iter().any(|block| matches!(block, ContentBlock::Thinking { thinking, signature } if thinking == "Checking" && signature.is_empty()))));
    assert!(agent.history().iter().any(|message| {
        message
            .content
            .iter()
            .any(|block| matches!(block, ContentBlock::Text { text } if text == "Hello"))
    }));
    if with_tool {
        assert!(
            agent
                .history()
                .iter()
                .any(|message| message.content.iter().any(|block| matches!(
                    block,
                    ContentBlock::ToolResult {
                        is_error: false,
                        ..
                    }
                )))
        );
    }
}

#[tokio::test]
async fn a_quiet_end_after_a_call_is_replayed_as_the_marker() {
    let reasoning = |id: &str| json!({"type":"reasoning","id":id,"encrypted_content":"synthetic-replay","summary":[]});
    let call = json!({"type":"function_call","id":"fc_echo","call_id":"call_echo","name":"echo","arguments":"{\"text\":\"echo ran\"}","status":"completed"});
    let typed = json!({"type":"message","id":"msg_typed","status":"completed","role":"assistant","content":[{"type":"output_text","text":"[no text]","annotations":[]}]});
    for last in [
        vec![reasoning("rs_end")],
        Vec::new(),
        vec![reasoning("rs_end"), typed],
    ] {
        let (provider, server) = scripted_server(vec![
            Some(vec![
                created(),
                completed(&[reasoning("rs_call"), call.clone()]),
            ]),
            Some(vec![created(), completed(&last)]),
            Some(vec![created(), completed(&[message()])]),
        ])
        .await;
        let scratch = tempfile::tempdir().unwrap();
        let dir = scratch.path().to_path_buf();
        let workspace = crate::workspace::Workspace {
            root: dir.clone(),
            home: None,
            temp: dir,
        };
        let mut agent = crate::agent::Agent::new(std::sync::Arc::new(provider), session())
            .with_tools(crate::tool::testkit::toolbox(&["echo"]), workspace);
        turn(&mut agent, "Hello", &[]).await;
        turn(&mut agent, "Again", &[]).await;
        let bodies = bodies(&server);
        let (before, after) = (
            bodies[1]["input"].as_array().unwrap(),
            bodies[2]["input"].as_array().unwrap(),
        );
        assert_eq!(after[..before.len()], before[..], "{last:?}");
        let added = &after[before.len()..];
        assert_eq!(
            added[0],
            json!({"content":[{"text":"[no text]","type":"output_text"}],"role":"assistant"}),
            "{last:?}"
        );
        assert!(
            added[1..].iter().all(|item| item["role"] == "user"),
            "{added:?}"
        );
        assert!(
            !added.iter().any(|item| item["type"] == "reasoning"),
            "{added:?}"
        );
    }
}

#[test]
fn routine_events_are_quiet_and_unknown_events_pass_through() {
    let mut machine = Machine::new("gpt-6.1-sol".into(), "id".into(), BTreeSet::new(), false);
    for kind in [
        "response.in_progress",
        "response.output_item.added",
        "response.content_part.added",
        "response.content_part.done",
        "response.output_text.done",
        "response.refusal.done",
        "response.function_call_arguments.delta",
        "response.function_call_arguments.done",
        "response.reasoning_summary_part.added",
        "response.reasoning_summary_part.done",
        "response.reasoning_summary_text.done",
        "response.metadata",
        "codex.response.metadata",
        "response.web_search_call.in_progress",
        "response.web_search_call.searching",
        "response.web_search_call.completed",
    ] {
        assert!(
            machine
                .apply(json!({"type":kind,"response":{"safety_identifier":"private"}}))
                .unwrap()
                .is_empty(),
            "{kind}"
        );
    }
    let other = json!({"type":"file_citation","file_id":"file_fixture"});
    let note =
        json!({"type":"response.output_text.annotation.added","output_index":0,"annotation":other});
    assert!(machine.apply(note).unwrap().is_empty());
    let future = json!({"type":"future","response":{"safety_identifier":"safety_PLACEHOLDER","items":[{"user_id":"user_PLACEHOLDER","account_id":"account_PLACEHOLDER","email":"someone@example.com","kept":7}]}});
    let events = machine.apply(future.clone()).unwrap();
    let [ProviderEvent::Unknown { raw, .. }] = events.as_slice() else {
        panic!("{events:?}");
    };
    assert_eq!(raw, &future);
}

#[test]
fn diagnostics_drop_nested_identity_and_keep_the_usage() {
    let mut machine = Machine::new("gpt-6.1-sol".into(), "id".into(), BTreeSet::new(), false);
    let mut assembler = Assembler::default();
    let completed = json!({"type":"response.completed","response":{"id":"resp_fixture","model":"gpt-6.1-sol","status":"completed","output":[message()],
    "usage":{"input_tokens":100,"output_tokens":20,"safety_identifier":"private","details":[{"user_id":"private","account_id":"private","email":"me@example.com","plan_type":"pro","credits":{"balance":"CREDIT_CANARY"},"promo":"PROMO_CANARY","kept":7}]}}});
    for event in machine
        .apply(created())
        .unwrap()
        .into_iter()
        .chain(machine.apply(completed).unwrap())
    {
        assembler.apply(event).unwrap();
    }
    let assembled = assembler.finish().unwrap();
    assert_eq!(
        assembled.diagnostics,
        Some(json!({"input_tokens":100,"output_tokens":20,"details":[{"kept":7}]}))
    );
    assert_eq!(assembled.usage.input_tokens, 100);
    assert_eq!(assembled.usage.output_tokens, 20);
}

#[test]
fn cache_key_tracks_only_the_shared_prefix() {
    assert_eq!(
        prefix_hash(
            &json!({"model":"test-model","reasoning":{"effort":"low"},"tools":[],"instructions":"rules"})
        ),
        "09ae0e284995628b63e2a5956c20b05aa39c8ff1ce5f4c0f5fbd3dc33e03e762"
    );
    let provider = provider();
    let mut session = session();
    let rendering = Rendering::default();
    let mut messages = vec![Message::user_text("first")];
    let first = provider
        .body(&request(&session, &messages, &[], &rendering))
        .unwrap();
    messages.push(Message::user_text("second"));
    let grown = provider
        .body(&request(&session, &messages, &[], &rendering))
        .unwrap();
    let addition = [Message::turn_system(
        None,
        &[tools::added(&ToolSpec {
            name: "new_tool".into(),
            description: "New tool".into(),
            input_schema: json!({"type":"object"}),
        })],
    )
    .unwrap()];
    let mut with_addition = request(&session, &addition, &[], &rendering);
    with_addition.features = Features::using(
        &[super::super::Feature::InlineTools],
        super::super::FeatureUse::Established,
    );
    assert_eq!(
        first["prompt_cache_key"],
        provider.body(&with_addition).unwrap()["prompt_cache_key"]
    );
    assert_eq!(
        provider.prefix_bytes(&with_addition),
        provider.prefix_bytes(&request(&session, &messages, &[], &rendering))
    );
    assert_eq!(first["prompt_cache_key"], grown["prompt_cache_key"]);
    assert_eq!(first["prompt_cache_key"].as_str().unwrap().len(), 64);
    assert_eq!(
        provider
            .cache_prefix(&request(&session, &messages, &[], &rendering))
            .unwrap(),
        first["prompt_cache_key"]
    );
    let mut searching = request(&session, &messages, &[], &rendering);
    searching.server_tools = search_tool("web_search");
    let searched = provider.body(&searching).unwrap();
    assert_ne!(first["prompt_cache_key"], searched["prompt_cache_key"]);
    searching.messages = &messages[..1];
    assert_eq!(
        provider.body(&searching).unwrap()["prompt_cache_key"],
        searched["prompt_cache_key"]
    );
    session.system = Some("another stable prompt".into());
    let changed = provider
        .body(&request(&session, &messages, &[], &rendering))
        .unwrap();
    assert_ne!(first["prompt_cache_key"], changed["prompt_cache_key"]);
}

#[test]
fn replay_identity_includes_the_responses_endpoint() {
    let first = provider();
    assert_eq!(
        first.identity,
        "c0fcd2b8c82ddb856a72c0d3e9a1332e2f80177de12cb687e0415d53cb3b8a32"
    );
    let second = OpenAiProvider::from_endpoint(&endpoint(
        Credential::ApiKey("synthetic-openai-key".into()),
        "http://localhost:12345",
    ))
    .unwrap();
    assert_ne!(first.identity, second.identity);
    let native = ContentBlock::opaque(
        PROVIDER,
        json!({"type":wisp_protocol::NATIVE_ITEM,"item":{"type":"reasoning","encrypted_content":"private-replay"},"projection_blocks":0,"identity":first.identity}),
    );
    let mut messages = vec![Message::user_text("question")];
    let mut reply = Message::user_text("unused");
    reply.role = Role::Assistant;
    reply.content = vec![native];
    messages.push(reply);
    let body = second
        .body(&request(&session(), &messages, &[], &Rendering::default()))
        .unwrap();
    assert!(!body.to_string().contains("private-replay"));
}

#[test]
fn summary_parts_stream_and_finalize_with_blank_lines() {
    let mut machine = Machine::new("gpt-6.1-sol".into(), "id".into(), BTreeSet::new(), false);
    let mut assembler = Assembler::default();
    let mut streamed = String::new();
    let item = json!({"type":"reasoning","id":"rs_parts","summary":[{"type":"summary_text","text":"First"},{"type":"summary_text","text":""},{"type":"summary_text","text":"Second"}]});
    for raw in [
        created(),
        json!({"type":"response.reasoning_summary_text.delta","output_index":0,"summary_index":0,"delta":"First"}),
        json!({"type":"response.reasoning_summary_text.delta","output_index":0,"summary_index":1,"delta":""}),
        json!({"type":"response.reasoning_summary_text.delta","output_index":0,"summary_index":2,"delta":"Second"}),
        completed(&[item, message()]),
    ] {
        for event in machine.apply(raw).unwrap() {
            if let ProviderEvent::BlockDelta {
                delta: Delta::Thinking(text),
                ..
            } = &event
            {
                streamed.push_str(text);
            }
            assembler.apply(event).unwrap();
        }
    }
    assert_eq!(streamed, "First\n\nSecond");
    assert!(
        assembler
            .finish()
            .unwrap()
            .content
            .iter()
            .any(|b| matches!(b, ContentBlock::Thinking { thinking, .. } if thinking == &streamed))
    );
}

#[test]
fn stream_errors_keep_the_classification_and_a_scrubbed_reason() {
    let tokens = [
        "sk-CANARY_0123456789abcdefghijklmn",
        "org-abcdefghijklmnopqrstuvwx",
        "user-abcdefghijklmnopqrstuvwx",
        "proj_abcdefghijklmnopqrstuvwx",
        "unit-registered-secret-CANARY[private]",
    ];
    crate::session::register_secrets([tokens[4].to_owned()]);
    let escaped = "sk-proj-CANARY_ab1_cd2_ef3_gh4_ijk5";
    assert!(!wisp_protocol::model_looks_like_key(escaped));
    assert!(wisp_protocol::has_known_key_prefix(escaped));
    let email = ["CANARY+account", "example.invalid"].join("@");
    let encoded_email = "CANARY%2Baccount%40example%2einvalid";
    let encoded_slash_email = "CANARY/team%40example.invalid";
    let start = "OpenAI did not complete the response";
    let fixture = format!(
        r"Synthetic option is unavailable ({} \n{escaped} \r{escaped} \t{escaped} {email}) {}",
        tokens.join(" "),
        "界".repeat(240)
    );
    let visible = format!(
        r"Synthetic option is unavailable ({} \n[redacted] \r[redacted] \t[redacted] [redacted]) ",
        ["[redacted]"; 5].join(" ")
    );
    let opaque = "CANARY.+/=_token";
    let long_header_id = format!("req_{}", "x".repeat(100));
    let prefixed_header_id = "sk-proj-CANARY_ab1_cd2_ef3";
    let header_ids = [
        (false, None, None),
        (true, Some("req_header"), Some("req_header")),
        (
            false,
            Some(long_header_id.as_str()),
            Some(&long_header_id[..64]),
        ),
        (true, Some(prefixed_header_id), Some("[redacted]")),
    ];
    let prose = "Synthetic option 'input[3]' is unavailable";
    let payload = json!({"input":"CANARY_INPUT","account_id":"CANARY_ACCOUNT","headers":{"Authorization":format!("Bearer {opaque}")}});
    let samples = [
        (
            fixture,
            format!(
                "{start}: {visible}{}",
                "界".repeat(240 - visible.chars().count())
            ),
        ),
        (format!("{prose}  {payload:#}"), format!("{start}: {prose}")),
        (
            format!("{} {prose} {payload}", tokens[4]),
            format!("{start}: [redacted] {prose}"),
        ),
        (payload.to_string(), start.to_owned()),
        (format!("{prose} [{payload}]"), format!("{start}: {prose}")),
        (r#"["CANARY_INPUT"]"#.to_owned(), start.to_owned()),
        (
            "{'input': 'CANARY_INPUT', 'api_key': 'CANARY_ACCOUNT'} is not text".to_owned(),
            start.to_owned(),
        ),
        (
            r#"[0, "CANARY_INPUT", "CANARY_ACCOUNT"]"#.to_owned(),
            start.to_owned(),
        ),
        (
            "Check contents[0] and parts[12]".to_owned(),
            format!("{start}: Check contents[0] and parts[12]"),
        ),
        (
            format!("Authorization: BeArEr {opaque} was refused"),
            format!("{start}: Authorization: BeArEr [redacted] was refused"),
        ),
        (
            format!("Bearer {opaque} was refused"),
            format!("{start}: Bearer [redacted] was refused"),
        ),
        (
            format!("Gateway rejected bearer: 'Bearer {opaque}'"),
            format!("{start}: Gateway rejected bearer: 'Bearer [redacted]'"),
        ),
        (
            format!("Bearer Bearer {opaque}"),
            format!("{start}: Bearer Bearer [redacted]"),
        ),
        (
            format!("auth_bearer {opaque}"),
            format!("{start}: auth_bearer [redacted]"),
        ),
        (
            format!("authBearer {opaque}; 401bearer={opaque}"),
            format!("{start}: authBearer [redacted]; 401bearer=[redacted]"),
        ),
        (
            format!("Bearer`{opaque}` and Bearer<{opaque}>"),
            format!("{start}: Bearer`[redacted]` and Bearer<[redacted]>"),
        ),
        (
            format!("Bearer token <{opaque}>; Bearer\u{200b}{opaque}"),
            format!("{start}: Bearer token <[redacted]>; Bearer\u{200b}[redacted]"),
        ),
        (
            format!("Bearer token: `Bearer {opaque}`"),
            format!("{start}: Bearer token: `Bearer [redacted]`"),
        ),
        (
            format!(r#""Bearer token": "{opaque}"; Bearer token: "<{opaque}>""#),
            format!(r#"{start}: "Bearer token": "[redacted]"; Bearer token: "<[redacted]>""#),
        ),
        (
            format!(r#"'Bearer token'='<"{opaque}">'; Bearer token {opaque}"#),
            format!(r#"{start}: 'Bearer token'='<"[redacted]">'; Bearer token [redacted]"#),
        ),
        (
            format!("Contact {encoded_email} to check access"),
            format!("{start}: Contact [redacted] to check access"),
        ),
        (
            format!("Contact {encoded_slash_email} to check access"),
            format!("{start}: Contact [redacted] to check access"),
        ),
        (
            format!("Check items[0][name][2] [{payload}]"),
            format!("{start}: Check items[0][name][2]"),
        ),
        (
            format!(r#"Bearer '{opaque}' or Bearer "{opaque}""#),
            format!(r#"{start}: Bearer '[redacted]' or Bearer "[redacted]""#),
        ),
        (
            format!("Bearer:{opaque}. Bearer\u{a0}{opaque}"),
            format!("{start}: Bearer:[redacted]. Bearer\u{a0}[redacted]"),
        ),
        (
            format!(r"Header:\nBearer {opaque}"),
            format!(r"{start}: Header:\nBearer [redacted]"),
        ),
    ];
    for (code, kind, retry) in [
        ("server_error", ErrorKind::Server, RetryClass::Transient),
        (
            "server_is_overloaded",
            ErrorKind::Overloaded,
            RetryClass::Transient,
        ),
        ("slow_down", ErrorKind::Overloaded, RetryClass::Transient),
        (
            "rate_limit_exceeded",
            ErrorKind::RateLimit,
            RetryClass::Transient,
        ),
        (
            "context_length_exceeded",
            ErrorKind::ContextOverflow,
            RetryClass::Never,
        ),
        ("insufficient_quota", ErrorKind::Billing, RetryClass::Never),
        (
            "invalid_request",
            ErrorKind::InvalidRequest,
            RetryClass::Never,
        ),
    ] {
        for (fixture, expected) in &samples {
            let error = json!({"code":code,"message":fixture,"request_id":"req_nested_body"});
            for raw in [
                json!({"type":"response.failed","response":{"error":error,"request_id":"req_body"}}),
                json!({"type":"response.incomplete","response":{"error":{"code":code,"request_id":"req_nested_body"},"incomplete_details":{"reason":fixture}}}),
                json!({"type":"response.incomplete","response":{"error":error,"incomplete_details":{"reason":"not the explanation"}}}),
                json!({"type":"error","code":code,"message":fixture,"request_id":"req_body"}),
            ] {
                for (subscription, header_id, expected_id) in header_ids {
                    let mut machine = Machine::new(
                        "gpt-6.1-sol".into(),
                        "id".into(),
                        BTreeSet::new(),
                        subscription,
                    );
                    machine.request_id = header_id.map(str::to_owned);
                    machine.apply(created()).unwrap();
                    machine.apply(raw.clone()).unwrap();
                    let failure = machine.failure.unwrap();
                    assert_eq!(failure.kind, kind);
                    assert_eq!(failure.detail.retry, retry);
                    assert_eq!(failure.detail.error_type.as_deref(), Some(code));
                    assert_eq!(failure.detail.request_id.as_deref(), expected_id);
                    assert_eq!(failure.message, start);
                    assert_eq!(failure.reported_message(), *expected);
                    let debug = format!("{failure:?}");
                    assert!(
                        tokens
                            .into_iter()
                            .chain([
                                escaped,
                                email.as_str(),
                                encoded_email,
                                encoded_slash_email,
                                opaque,
                                "CANARY_INPUT",
                                "CANARY_ACCOUNT"
                            ])
                            .all(|token| !debug.contains(token))
                    );
                }
            }
        }
    }
}

#[tokio::test]
async fn an_anthropic_agent_rejects_a_known_openai_model_before_inference() {
    use std::sync::Arc;
    use wisp_protocol::settings::{SettingChange, SettingValue};
    use wisp_protocol::{AgentEvent, ClientCommand, RequestId};
    let provider = crate::provider::anthropic::AnthropicProvider::new(
        &Credential::ApiKey("synthetic".into()),
        crate::provider::anthropic::DEFAULT_BASE_URL,
    )
    .unwrap();
    let mut config = session();
    config.model = wisp_protocol::settings::DEFAULT_MODEL.into();
    let (commands, mut events, task) =
        crate::agent::spawn(crate::agent::Agent::new(Arc::new(provider), config));
    commands
        .send(ClientCommand::ChangeSettings {
            request: RequestId(1),
            changes: vec![
                SettingChange::named("model", SettingValue::Text("gpt-6.1-sol".into())).unwrap(),
            ],
            save: false,
        })
        .await
        .unwrap();
    let rejected = until_event(&mut events, &mut Vec::new(), "the refusal", |e| {
        matches!(e, AgentEvent::Rejected { .. })
    })
    .await;
    assert!(format!("{rejected:?}").contains("needs an OpenAI account"));
    drop(commands);
    task.handle.abort();
}

#[tokio::test]
async fn custom_gateways_allow_foreign_provider_model_ids_while_official_endpoints_refuse() {
    use std::sync::Arc;
    use wisp_protocol::settings::{SettingChange, SettingValue};
    use wisp_protocol::{AgentEvent, ClientCommand, RequestId};

    let custom_anthropic = crate::provider::anthropic::AnthropicProvider::new(
        &Credential::ApiKey("synthetic".into()),
        "https://gateway.example.com",
    )
    .unwrap();
    assert!(!crate::provider::Provider::official(&custom_anthropic));
    let mut config = session();
    config.model = wisp_protocol::settings::DEFAULT_MODEL.into();
    let (commands, mut events, task) =
        crate::agent::spawn(crate::agent::Agent::new(Arc::new(custom_anthropic), config));
    commands
        .send(ClientCommand::ChangeSettings {
            request: RequestId(1),
            changes: vec![
                SettingChange::named("model", SettingValue::Text("gemini-4-carbon".into()))
                    .unwrap(),
            ],
            save: false,
        })
        .await
        .unwrap();
    let accepted = until_event(&mut events, &mut Vec::new(), "gateway switch", |e| {
        matches!(
            e,
            AgentEvent::SettingsChanged { .. } | AgentEvent::Rejected { .. }
        )
    })
    .await;
    assert!(
        matches!(accepted, AgentEvent::SettingsChanged { .. }),
        "{accepted:?}"
    );
    drop(commands);
    task.handle.abort();

    let custom_openai = OpenAiProvider::from_endpoint(&ResolvedEndpoint {
        client_version: None,
        name: crate::endpoints::EndpointName::parse("gw").unwrap(),
        provider: ProviderKind::OpenAi,
        credential: Credential::ApiKey("synthetic-openai".into()),
        base_url: "https://openai-gw.example.com".into(),
        source: crate::provider::credential::Source::Environment,
        key_env: None,
        url_source: crate::endpoints::UrlSource::Official,
        pricing: crate::endpoints::PricingMode::List,
    })
    .unwrap();
    assert!(!crate::provider::Provider::official(&custom_openai));
    let mut gem_session = session();
    gem_session.model = "gemini-4-carbon".into();
    let msgs = [wisp_protocol::Message::user_text("hello")];
    let rendering = crate::provider::Rendering::default();
    let req = Request {
        session: &gem_session,
        tools: &[],
        deferred: &[],
        by_value: &[],
        server_tools: &[],
        messages: &msgs,
        previous_response_id: None,
        features: crate::provider::Features::default(),
        options: crate::provider::RequestOptions::default(),
        rendering: &rendering,
        compaction: None,
    };
    let encoded = custom_openai.body(&req).unwrap();
    assert_eq!(encoded["model"], "gemini-4-carbon");
}

#[test]
fn subscription_replay_identity_survives_access_token_rotation() {
    let credential = |token: &str| Credential::Subscription {
        kind: SubscriptionKind::ChatGpt,
        secret: token.into(),
        profile: Some(std::path::PathBuf::from("synthetic-profile")),
    };
    let first =
        OpenAiProvider::from_endpoint(&endpoint(credential("synthetic-first"), CHATGPT_BASE_URL))
            .unwrap();
    let second =
        OpenAiProvider::from_endpoint(&endpoint(credential("synthetic-second"), CHATGPT_BASE_URL))
            .unwrap();
    assert_eq!(first.identity, second.identity);
    let item = json!({"type":"reasoning","encrypted_content":"synthetic-replay"});
    let mut reply = Message::user_text("unused");
    reply.role = Role::Assistant;
    reply.content = vec![ContentBlock::opaque(
        "openai",
        json!({"type":wisp_protocol::NATIVE_ITEM,"item":item,"projection_blocks":0,"identity":first.identity}),
    )];
    let messages = [Message::user_text("question"), reply];
    let body = second
        .body(&request(&session(), &messages, &[], &Rendering::default()))
        .unwrap();
    assert_eq!(body["input"][1], item);
}

#[tokio::test]
async fn transient_failed_response_is_retried_by_the_agent() {
    use std::sync::Arc;
    let failed = json!({"type":"response.failed","response":{"error":{"code":"server_error","message":"Synthetic worker restarted"}}});
    let (provider, server) = scripted_server(vec![
        Some(vec![created(), failed]),
        Some(vec![created(), completed(&[message()])]),
    ])
    .await;
    let mut agent = crate::agent::Agent::new(Arc::new(provider), session());
    let (outcome, events) = turn(&mut agent, "hello", &[]).await;
    assert!(
        matches!(
            outcome,
            wisp_protocol::TurnOutcome::Completed {
                committed: true,
                ..
            }
        ),
        "{outcome:?}"
    );
    assert!(events.iter().any(|event| matches!(event,
        wisp_protocol::AgentEvent::Retrying { kind: ErrorKind::Server, reason, .. }
            if reason == "OpenAI did not complete the response: Synthetic worker restarted"
    )));
    assert_resent(&server);
}

#[tokio::test]
async fn plan_usage_is_priced_and_keeps_its_billing_provenance() {
    use std::sync::Arc;
    let (mut provider, server) =
        single_server(Some(vec![created(), completed(&[message()])])).await;
    provider.subscription = true;
    let mut agent = crate::agent::Agent::new(Arc::new(provider), session());
    let (outcome, _) = turn(&mut agent, "hello", &[]).await;
    let wisp_protocol::TurnOutcome::Completed {
        usage,
        cost: Some(cost),
        ..
    } = outcome
    else {
        panic!("{outcome:?}");
    };
    assert_eq!(usage.billing, wisp_protocol::Billing::ChatGptPlan);
    assert_eq!(cost.turn_billing, usage.billing);
    assert_eq!(cost.session_billing, usage.billing);
    assert!(cost.turn_usd > 0.0);
    assert_eq!(cost.session_plan_usd, Some(cost.session_usd));
    assert_eq!(asked(&server), 1);
}

#[test]
fn subscriptions_are_priced_only_when_list_pricing_is_selected() {
    for kind in [SubscriptionKind::ChatGpt, SubscriptionKind::Anthropic] {
        let mut endpoint = endpoint(
            Credential::Subscription {
                kind,
                secret: "synthetic".into(),
                profile: None,
            },
            DEFAULT_BASE_URL,
        );
        assert!(endpoint.priced());
        endpoint.pricing = PricingMode::None;
        assert!(!endpoint.priced());
    }
}

#[tokio::test]
async fn subscription_cache_routing_matches_the_stable_body_key_only_for_subscriptions() {
    use crate::plan_usage::record;
    use crate::provider::client_version::{Record, record_alias};
    let server = http::serve(
        (0..4)
            .map(|_| stream_reply("200 OK", &[], &data(&[created(), completed(&[message()])])))
            .collect(),
    )
    .await;
    let dir = tempfile::tempdir().unwrap();
    let connect = |version| {
        record::write(
            dir.path(),
            &record_alias(ProviderKind::OpenAi),
            &Record {
                version: ClientVersion::parse(version),
                checked: 0,
            },
        )
        .unwrap();
        let mut provider = subscription().with_saved_list(Some(dir.path()));
        provider.url = format!("{}/backend-api/codex/responses", server.url);
        provider
    };
    let session = session();
    let rendering = Rendering::default();
    let tools = crate::test_guard::tool_specs();
    let messages = [Message::user_text("first"), Message::user_text("more")];
    let first = request(&session, &messages[..1], &tools, &rendering);
    let second = request(&session, &messages, &tools, &rendering);
    let api = provider();
    assert!(
        !api.inference_request(&api.body(&first).unwrap(), "CANARY_CODEX_API")
            .build()
            .unwrap()
            .headers()
            .contains_key("session-id")
    );
    let old = connect("0.168.0");
    assert!(
        old.stream(&first)
            .collect::<Vec<_>>()
            .await
            .into_iter()
            .all(|event| event.is_ok())
    );
    let new = connect("0.170.0");
    let mut pinned = OpenAiProvider::from_endpoint(&ResolvedEndpoint {
        client_version: ClientVersion::parse("0.167.0"),
        ..endpoint(new.credential.clone(), CHATGPT_BASE_URL)
    })
    .unwrap();
    pinned.url.clone_from(&new.url);
    for provider in [&old, &new, &pinned] {
        assert!(
            provider
                .stream(&second)
                .collect::<Vec<_>>()
                .await
                .into_iter()
                .all(|event| event.is_ok())
        );
    }
    let requests = server.requests.lock().unwrap();
    assert_eq!(requests.len(), 4);
    for request in &requests[2..] {
        assert_eq!(http::body(&requests[1]), http::body(request));
        assert_eq!(
            http::header_lines(&requests[1]),
            http::header_lines(request)
        );
    }
    let mut keys = Vec::new();
    for sent in requests.iter() {
        let headers: BTreeMap<_, _> = http::header_lines(sent).into_iter().collect();
        let body: Value = serde_json::from_str(http::body(sent)).unwrap();
        assert_eq!(
            headers["session-id"],
            body["prompt_cache_key"].as_str().unwrap()
        );
        assert_eq!(headers["originator"], "codex_cli_rs");
        assert_eq!(
            headers["user-agent"],
            "codex_cli_rs/0.0.0 (Unknown 0; unknown) unknown"
        );
        keys.push(headers["session-id"].clone());
    }
    assert_eq!(keys[0], keys[1]);
    assert_eq!(keys[1], keys[2]);
}

#[test]
fn switched_anthropic_citations_and_search_results_reach_openai_without_mutating_history() {
    let mut history = vec![Message {
        role: Role::Assistant,
        content: vec![
            ContentBlock::opaque(
                "anthropic",
                json!({"type":"text","text":"Cited answer","citations":[{"type":"web_search_result_location","url":"https://example.org/source","title":"Source","encrypted_index":"citation-private-index"}]}),
            ),
            ContentBlock::opaque(
                "anthropic",
                json!({"type":"web_search_tool_result","tool_use_id":"s1","content":[{"type":"web_search_result","title":"Search hit","url":"https://example.org/hit","encrypted_content":"not-portable"}]}),
            ),
        ],
    }];
    history[0].content.push(ContentBlock::Opaque {
        provider: "anthropic".into(),
        raw: json!({"type":"future_content"}),
        deltas: vec![json!({"type":"future_delta","text":"Unknown streamed detail"})],
    });
    let before = history.clone();
    let session = session();
    let rendering = Rendering::default();
    let request = request(&session, &history, &[], &rendering);
    let body = provider().body(&request).unwrap().to_string();
    for text in [
        "Cited answer",
        "https://example.org/source",
        "Search hit",
        "https://example.org/hit",
        "Unknown streamed detail",
    ] {
        assert!(body.contains(text), "missing switched content");
    }
    assert!(!body.contains("not-portable"));
    assert!(!body.contains("encrypted_index") && !body.contains("citation-private-index"));
    assert_eq!(body, provider().body(&request).unwrap().to_string());
    assert_eq!(history, before);
}

#[tokio::test]
async fn a_plan_midstream_retry_preserves_billing_and_only_unknown_usage_is_lower_bound() {
    use std::sync::Arc;
    use wisp_protocol::{AgentEvent, Billing, TurnOutcome};
    for known in [false, true] {
        let mut interrupted = vec![
            created(),
            json!({"type":"response.output_text.delta","output_index":0,"delta":"partial"}),
        ];
        if known {
            interrupted.push(json!({"type":"response.failed","response":{"error":{"code":"server_error"},"usage":{"input_tokens":90,"output_tokens":12}}}));
        }
        let (mut provider, server) = scripted_server(vec![
            Some(interrupted),
            Some(vec![created(), completed(&[message()])]),
        ])
        .await;
        provider.subscription = true;
        let mut agent = crate::agent::Agent::new(Arc::new(provider), session())
            .with_retry_policy(crate::agent::retry::quick());
        let (outcome, events) = turn(&mut agent, "hello", &[]).await;
        assert_resent(&server);
        let TurnOutcome::Completed {
            usage,
            cost: Some(cost),
            ..
        } = outcome
        else {
            panic!("{outcome:?}")
        };
        assert_eq!(usage.billing, Billing::ChatGptPlan);
        assert_eq!(cost.turn_billing, Billing::ChatGptPlan);
        assert_eq!(cost.session_billing, Billing::ChatGptPlan);
        assert_eq!(cost.turn_lower_bound, !known);
        assert_eq!(cost.session_lower_bound, !known);
        let mut retries = 0;
        for event in events {
            if let AgentEvent::Retrying {
                usage,
                cost_usd,
                lower_bound,
                ..
            } = event
            {
                retries += 1;
                assert_eq!(lower_bound, !known);
                if known {
                    assert_eq!(usage.unwrap().billing, Billing::ChatGptPlan);
                    assert!(cost_usd.unwrap() > 0.0);
                } else {
                    assert!(usage.is_none());
                    assert!(cost_usd.is_none());
                }
            }
        }
        assert_eq!(retries, 1);
    }
}

#[tokio::test]
async fn model_lists_and_probes_reject_oversized_json() {
    let body = json!({"data":[], "padding":"x".repeat(super::super::body::LIMIT)}).to_string();
    let oversized = http::response("200 OK", &[], &body);
    let redirect = http::response("302 Found", &[("location", "/elsewhere")], "");
    let cases = [
        (oversized.clone(), None),
        (
            oversized,
            Some("unexpected answer (HTTP 200): the body could not be read"),
        ),
        (
            redirect,
            Some("unexpected answer (HTTP 302): a redirect, not followed"),
        ),
        (String::new(), Some("unreachable: the request failed")),
    ];
    for (reply, probe) in cases {
        let (provider, server) = serving(vec![http::Reply::Send(reply)]).await;
        match probe {
            Some(want) => assert_eq!(
                provider.probe(Duration::from_secs(5)).await.to_string(),
                want
            ),
            None => assert!(provider.model_ids().await.is_err()),
        }
        assert_eq!(asked(&server), 1);
    }
}

#[test]
fn opaque_reasoning_created_by_unknown_deltas_never_crosses_providers() {
    let mut thinking = ContentBlock::Thinking {
        thinking: "private-thinking-text".into(),
        signature: "private-signature".into(),
    };
    let mut redacted = ContentBlock::RedactedThinking {
        data: "private-redacted-data".into(),
    };
    for block in [&mut thinking, &mut redacted] {
        super::super::fold_unknown_delta(
            "anthropic",
            block,
            json!({"type":"future_delta","text":"private-delta-text"}),
        );
        assert!(matches!(block, ContentBlock::Opaque { .. }));
    }
    let history = vec![Message {
        role: Role::Assistant,
        content: vec![
            thinking,
            redacted,
            ContentBlock::opaque(
                "anthropic",
                json!({"type":"text","text":"Visible answer","extra":{"thinking":"private-nested-thinking","data":"private-nested-data","items":[{"type":"thinking","text":"private-nested-text"}]}}),
            ),
        ],
    }];
    let before = history.clone();
    let session = session();
    let rendering = Rendering::default();
    let body = provider()
        .body(&request(&session, &history, &[], &rendering))
        .unwrap()
        .to_string();
    assert!(body.contains("Visible answer"));
    assert!(
        !body.contains("private-"),
        "reasoning reached the foreign request"
    );
    assert_eq!(history, before);
}

#[tokio::test]
async fn api_key_plan_usage_never_requests_a_subscription_endpoint() {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let provider = OpenAiProvider::from_endpoint(&endpoint(
        Credential::ApiKey("fake-key".into()),
        &format!("http://{}", listener.local_addr().unwrap()),
    ))
    .unwrap();
    assert_eq!(provider.plan_usage().await, Ok(None));
    assert!(
        tokio::time::timeout(Duration::from_millis(10), listener.accept())
            .await
            .is_err()
    );
}

#[tokio::test]
async fn account_sse_and_headers_never_escape_real_root_and_child_turns() {
    account_privacy(crate::plan_usage::Display::Disabled).await;
}

#[tokio::test]
async fn displayed_account_sse_and_headers_never_escape_real_root_and_child_turns() {
    account_privacy(crate::plan_usage::Display::Enabled).await;
}

async fn account_privacy(display: crate::plan_usage::Display) {
    use crate::plan_usage::{Display, PlanUsageCache};
    use crate::session::{MemoryStore, SessionLog, SessionStore};
    use std::sync::Arc;
    use tokio::sync::mpsc;
    use wisp_protocol::{AgentEvent, AgentPath, TurnOutcome};
    let body = format!(
        "{}data: {}\n\n{}data: {}\n\ndata: {}\n\n",
        http::event("codex.rate_limits", "MALFORMED_CANARY"),
        json!({"type":"codex.rate_limits","metered_limit_name":"codex_other","limit_name":"NAME_CANARY","account_id":"ACCOUNT_CANARY","credits":{"balance":"CREDIT_CANARY"},"plan_type":"PLAN_CANARY","promo":"PROMO_CANARY"}),
        http::event(
            "codex.rate_limits",
            json!({"type":"codex.rate_limits","rate_limits":{"primary":{"used_percent":96,"window_minutes":300,"reset_at":1900000000}},"user_id":"USER_CANARY","credits":{"balance":"CREDIT_CANARY"}})
        ),
        created(),
        completed(&[message()])
    );
    let (mut provider, server) = serving(vec![
        stream_reply("200 OK", &USAGE_HEADERS, &body),
        stream_reply("200 OK", &USAGE_HEADERS, &body),
    ])
    .await;
    provider.subscription = true;
    let provider = Arc::new(provider);
    let cache = PlanUsageCache::default();
    let store = MemoryStore::default();
    for id in ["root", "child"] {
        let mut agent = crate::agent::Agent::new(provider.clone(), session())
            .with_log(
                SessionLog::new(Box::new(store.clone()), id),
                crate::agent::LogContext {
                    endpoint: "https://example.invalid".into(),
                    endpoint_name: "alias".into(),
                    ..log_context()
                },
            )
            .with_plan_usage(cache.clone(), display);
        let (_tx, mut commands) = mpsc::channel(8);
        let (events, mut received) = mpsc::channel(256);
        if display == Display::Enabled {
            let generation = cache.bind("alias", provider.clone()).unwrap();
            cache.subscribe(
                "alias",
                generation,
                id,
                wisp_protocol::RequestId(1),
                &events,
                &session().model,
            );
        }
        let outcome = agent.run_turn("hello".into(), &mut commands, &events).await;
        assert!(
            matches!(outcome, Some(TurnOutcome::Completed { .. })),
            "{outcome:?}"
        );
        for json in [
            serde_json::to_string(&*agent.history()).unwrap(),
            serde_json::to_string(&store.read(id).unwrap()).unwrap(),
        ] {
            assert!(!json.contains("CANARY"), "{json}");
            assert!(!json.contains("plan_usage"), "{json}");
        }
        tokio::task::yield_now().await;
        let mut telemetry = 0;
        while let Ok(event) = received.try_recv() {
            if event.is_account_telemetry() {
                telemetry += 1;
                assert!(!serde_json::to_string(&event).unwrap().contains("CANARY"));
                continue;
            }
            let event = if id == "child" {
                AgentEvent::Agent {
                    agent: AgentPath::new("/child"),
                    event: Box::new(event),
                }
            } else {
                event
            };
            let jsonl = wisp_protocol::jsonl::line(&event).unwrap();
            assert!(!jsonl.contains("CANARY"), "{jsonl}");
            assert!(!jsonl.contains("plan_usage"), "{jsonl}");
        }
        assert_eq!(telemetry > 0, display == Display::Enabled);
    }
    assert_eq!(asked(&server), 2);
}

#[tokio::test]
async fn usage_headers_and_both_sse_envelopes_use_the_injected_receipt_clock() {
    use crate::clock::{Clock, StillClock};
    use std::sync::Arc;
    let raw = json!({"type":"codex.rate_limits","rate_limits":{"primary":{"used_percent":75,"window_minutes":300,"reset_at":1900000000}}});
    let body = format!(
        "{}data: {raw}\n\ndata: {}\n\ndata: {}\n\n",
        http::event("codex.rate_limits", &raw),
        created(),
        completed(&[message()])
    );
    let (mut provider, server) = serving(vec![stream_reply(
        "200 OK",
        &[("x-codex-primary-used-percent", "50")],
        &body,
    )])
    .await;
    provider.subscription = true;
    let provider = provider.with_clock(Arc::new(StillClock));
    let session = session();
    let rendering = Rendering::default();
    let events = provider
        .stream(&request(&session, &[], &[], &rendering))
        .collect::<Vec<_>>()
        .await;
    let observations: Vec<_> = events
        .into_iter()
        .filter_map(|event| match event.unwrap() {
            ProviderEvent::PlanUsage(Ok(usage)) => Some(usage),
            _ => None,
        })
        .collect();
    assert_eq!(observations.len(), 3);
    assert!(
        observations
            .iter()
            .flat_map(|usage| &usage.windows)
            .all(|window| window.observed_at == StillClock.now().as_second())
    );
    assert_eq!(asked(&server), 1);
}

#[tokio::test]
async fn type_only_limit_429_waits_for_reset_and_stays_cancellable_without_private_body() {
    use crate::clock::{Clock, StillClock};
    use std::sync::Arc;
    use tokio::sync::mpsc;
    use wisp_protocol::{AgentEvent, TurnOutcome};
    let now = StillClock.now().as_second();
    let body = json!({"error":{"type":"usage_limit_reached","resets_at":now+7200,"limit_window_minutes":10080,"plan_type":"PRIVATE_CANARY","message":"PRIVATE_CANARY"}}).to_string();
    let (provider, server) = serving(vec![http::Reply::Send(http::json(
        "429 Too Many Requests",
        &body,
    ))])
    .await;
    let mut provider = provider.with_clock(Arc::new(StillClock));
    provider.subscription = true;
    let mut agent = crate::agent::Agent::new(Arc::new(provider), session());
    let (send, mut commands) = mpsc::channel(4);
    let (events, mut received) = mpsc::channel(128);
    let task = tokio::spawn(async move {
        agent.serve(&mut commands, &events).await;
    });
    send.send(wisp_protocol::ClientCommand::send("one", "hello"))
        .await
        .unwrap();
    let mut seen = Vec::new();
    let retry = until_event(&mut received, &mut seen, "the reset wait", |event| {
        matches!(event, AgentEvent::Retrying { .. })
    })
    .await;
    let AgentEvent::Retrying {
        kind: ErrorKind::RateLimit,
        detail: Some(detail),
        delay_ms: 7_200_000,
        max_attempts: 0,
        busy: Some(busy),
        ..
    } = &retry
    else {
        panic!("expected the reset wait")
    };
    assert_eq!(busy.window_ms, 0);
    assert_eq!(
        detail.limit_reached.as_ref().unwrap().resets_at,
        Some(now + 7200)
    );
    assert!(!serde_json::to_string(&retry).unwrap().contains("CANARY"));
    tokio::time::pause();
    tokio::task::yield_now().await;
    tokio::time::advance(Duration::from_secs(7_199)).await;
    while let Ok(event) = received.try_recv() {
        assert!(!matches!(
            event,
            AgentEvent::Retrying { .. } | AgentEvent::TurnEnded { .. }
        ));
        seen.push(event);
    }
    assert_eq!(asked(&server), 1, "nothing is sent before the reset");
    send.send(wisp_protocol::ClientCommand::Cancel)
        .await
        .unwrap();
    let stopped = tokio::time::Instant::now();
    let AgentEvent::TurnEnded { outcome, .. } = until_turn_end(&mut received, &mut seen).await
    else {
        unreachable!()
    };
    assert!(matches!(
        outcome,
        TurnOutcome::Error {
            gave_up: None,
            offer: Some(_),
            ..
        }
    ));
    assert!(stopped.elapsed() < Duration::from_secs(1));
    assert_eq!(
        seen.iter()
            .filter(|event| matches!(event, AgentEvent::Retrying { .. }))
            .count(),
        1
    );
    drop(send);
    task.await.unwrap();
    while let Some(event) = received.recv().await {
        assert!(!matches!(
            event,
            AgentEvent::Retrying { .. } | AgentEvent::TurnEnded { .. }
        ));
    }
    assert_eq!(asked(&server), 1);
}

#[tokio::test]
async fn stream_limit_wrappers_are_consumed_as_safe_rate_limits() {
    use crate::clock::{Clock, StillClock};
    use std::sync::Arc;
    let now = StillClock.now().as_second();
    let error = json!({"type":"usage_limit_reached","resets_in_seconds":270000,"plan_type":"PRIVATE_CANARY"});
    for refusal in [
        json!({"type":"response.failed","response":{"error":error}}),
        json!({"type":"response.incomplete","response":{"error":error}}),
        json!({"type":"error","error":error}),
    ] {
        let (mut provider, server) = scripted_server(vec![Some(vec![created(), refusal])]).await;
        provider.subscription = true;
        let provider = provider.with_clock(Arc::new(StillClock));
        let session = session();
        let rendering = Rendering::default();
        let events: Vec<_> = provider
            .stream(&request(&session, &[], &[], &rendering))
            .collect()
            .await;
        let errors: Vec<_> = events
            .iter()
            .filter_map(|event| event.as_ref().err())
            .collect();
        assert_eq!(errors.len(), 1);
        assert_eq!(errors[0].kind, ErrorKind::RateLimit);
        assert_eq!(
            errors[0].detail.limit_reached.as_ref().unwrap().resets_at,
            Some(now + 270000)
        );
        assert!(!format!("{events:?}").contains("CANARY"));
        assert_eq!(asked(&server), 1);
    }
}

#[test]
fn subscription_reauthorization_changes_native_prefix_once_without_rewriting_history() {
    let make = |path: &str| {
        OpenAiProvider::from_endpoint(&endpoint(
            Credential::Subscription {
                kind: SubscriptionKind::ChatGpt,
                secret: "synthetic-token".to_owned(),
                profile: Some(path.into()),
            },
            CHATGPT_BASE_URL,
        ))
        .unwrap()
    };
    let first = make("synthetic-old-profile");
    let replacement = make("synthetic-new-profile");
    let stable = make("synthetic-new-profile");
    assert_ne!(first.identity, replacement.identity);
    let item = json!({"type":"reasoning","id":"synthetic-reasoning","encrypted_content":"PRIVATE_ENCRYPTED_CANARY","summary":[{"type":"summary_text","text":"A retained reasoning summary"}]});
    let mut machine = Machine::new(
        "gpt-6.1-sol".into(),
        first.identity.clone(),
        BTreeSet::new(),
        true,
    );
    let mut assembler = Assembler::default();
    for event in machine
        .apply(created())
        .unwrap()
        .into_iter()
        .chain(machine.apply(completed(&[item, message()])).unwrap())
    {
        assembler.apply(event).unwrap();
    }
    let messages = vec![
        Message::user_text("question"),
        assembler.finish().unwrap().into_message(),
    ];
    let committed = serde_json::to_vec(&messages).unwrap();
    let session = session();
    let rendering = Rendering::default();
    let request = request(&session, &messages, &[], &rendering);
    let before = first.body(&request).unwrap();
    let after = replacement.body(&request).unwrap();
    assert_ne!(before, after);
    assert!(before.to_string().contains("PRIVATE_ENCRYPTED_CANARY"));
    assert!(!after.to_string().contains("PRIVATE_ENCRYPTED_CANARY"));
    assert!(!after.to_string().contains("A retained reasoning summary"));
    assert!(after.to_string().contains("Hello"));
    assert_eq!(after, stable.body(&request).unwrap());
    assert_eq!(serde_json::to_vec(&messages).unwrap(), committed);
}

#[tokio::test]
async fn an_openai_event_past_the_cap_ends_the_stream_as_before() {
    let body = format!("data: {}\n\n", "q".repeat(16 * 1024 * 1024));
    let (provider, _server) = serving(vec![stream_reply("200 OK", &[], &body)]).await;
    let session = session();
    let rendering = Rendering::default();
    let events: Vec<_> = provider
        .stream(&request(&session, &[], &[], &rendering))
        .collect()
        .await;
    let [.., Err(error)] = events.as_slice() else {
        panic!("{events:?}")
    };
    assert_eq!(events.iter().filter(|event| event.is_err()).count(), 1);
    assert_eq!(error.kind, ErrorKind::InvalidResponse);
    assert_eq!(error.message, "OpenAI event is too large");
    assert!(error.possibly_billed);
    assert_eq!(error.detail.status, Some(200));
    assert_eq!(error.detail.phase, FailurePhase::Stream);
    assert_eq!(error.detail.retry, RetryClass::Never);
    assert!(!format!("{error:?}").contains("qqqq"));
}

fn subscription() -> OpenAiProvider {
    let credential = Credential::Subscription {
        kind: SubscriptionKind::ChatGpt,
        secret: "synthetic-oauth".into(),
        profile: None,
    };
    OpenAiProvider::from_endpoint(&endpoint(credential, CHATGPT_BASE_URL)).unwrap()
}

fn added(oi: usize, item: &Value) -> Value {
    json!({"type":"response.output_item.added","output_index":oi,"item":item})
}

fn done(oi: usize, item: &Value) -> Value {
    json!({"type":"response.output_item.done","output_index":oi,"item":item})
}

fn search_items() -> Vec<Value> {
    let found = json!([{"type":"url","url":"https://example.com/source-a"},{"type":"api","name":"Example API"}]);
    let find = json!({"type":"find_in_page","url":"https://example.com/page","pattern":"example"});
    vec![
        json!({"type":"web_search_call","id":"ws_fixture_1","status":"completed","action":{"type":"search","query":"example query","queries":["example query"],"sources":found}}),
        json!({"type":"web_search_call","id":"ws_fixture_2","status":"completed","action":{"type":"open_page","url":"https://example.com/page"}}),
        json!({"type":"web_search_call","id":"ws_fixture_3","status":"failed","action":find}),
        json!({"type":"message","id":"msg_cited","status":"completed","role":"assistant","phase":"final_answer","content":[{"type":"output_text","text":"Cited ([example](https://example.com/page)).","logprobs":[],
            "annotations":[{"type":"url_citation","start_index":6,"end_index":43,"title":"Example page","url":"https://example.com/page"}]}]}),
    ]
}

fn row(index: usize, signal: &SearchSignal) -> String {
    match signal {
        SearchSignal::Started {
            call_id,
            query,
            action,
        } => format!("{index} {action:?} {call_id} {query}"),
        SearchSignal::Ended {
            call_id,
            outcome: wisp_protocol::SearchOutcome::Results { hits },
        } => {
            let hits: Vec<String> = hits
                .iter()
                .map(|h| format!("{}|{}", h.url, h.title))
                .collect();
            format!("{index} {call_id} {}", hits.join(" "))
        }
        SearchSignal::Ended { call_id, .. } => format!("{index} {call_id} failed"),
        SearchSignal::Cited { sources } => format!("{index} cited {sources:?}"),
    }
}

#[test]
fn a_search_turn_becomes_rows_and_replays_byte_for_byte() {
    let provider = subscription();
    let mut machine = Machine::new(
        "gpt-6.1-sol".into(),
        provider.identity.clone(),
        BTreeSet::new(),
        true,
    );
    let items = search_items();
    let mut stream = vec![created()];
    for (oi, item) in items.iter().enumerate().take(3) {
        let searching = json!({"type":"response.web_search_call.searching","output_index":oi});
        stream.extend([added(oi, item), searching, done(oi, item)]);
    }
    let text = &items[3]["content"][0];
    stream.extend([
        added(3, &items[3]),
        json!({"type":"response.output_text.delta","output_index":3,"delta":text["text"]}),
        json!({"type":"response.output_text.annotation.added","output_index":3,"annotation":text["annotations"][0]}),
        done(3, &items[3]),
    ]);
    let mut finished = completed(&[]);
    finished["response"]["tool_usage"] = json!({"web_search":{"num_requests":1}});
    stream.push(finished);
    let mut uncounted = Machine::new("gpt-6.1-sol".into(), "id".into(), BTreeSet::new(), true);
    let mut counted = stream.clone();
    counted.last_mut().unwrap()["response"]["tool_usage"] = json!({});
    let usage = counted
        .into_iter()
        .flat_map(|raw| uncounted.apply(raw).unwrap());
    let searches = usage.filter_map(|event| match event {
        ProviderEvent::MessageDelta { usage, .. } => Some(usage.web_search_requests),
        _ => None,
    });
    assert_eq!(searches.collect::<Vec<_>>(), [1]);
    let mut assembler = Assembler::default();
    let mut rows = Vec::new();
    for event in stream
        .into_iter()
        .flat_map(|raw| machine.apply(raw).unwrap())
    {
        assert!(!matches!(event, ProviderEvent::Unknown { .. }), "{event:?}");
        if let ProviderEvent::Search { index, signal } = &event {
            rows.push(row(*index, signal));
        }
        if let ProviderEvent::MessageDelta { usage, .. } = &event {
            assert_eq!(usage.web_search_requests, 1);
        }
        assembler.apply(event).unwrap();
    }
    let cited =
        r#"7 cited [CitedSource { url: "https://example.com/page", title: Some("Example page") }]"#;
    assert_eq!(
        rows,
        [
            "0 Search ws_fixture_1 example query",
            "0 ws_fixture_1 https://example.com/source-a| |Example API",
            "2 OpenPage ws_fixture_2 https://example.com/page",
            "2 ws_fixture_2 ",
            "4 FindInPage ws_fixture_3 example",
            "4 ws_fixture_3 failed",
            cited,
        ]
    );
    let messages = vec![
        Message::user_text("Question"),
        assembler.finish().unwrap().into_message(),
    ];
    let (session, rendering) = (session(), Rendering::default());
    let mut sent = request(&session, &messages, &[], &rendering);
    sent.server_tools = search_tool("web_search");
    let body = provider.body(&sent).unwrap();
    assert_eq!(&body["input"].as_array().unwrap()[1..], items.as_slice());
    let include = json!([
        "reasoning.encrypted_content",
        "web_search_call.action.sources"
    ]);
    assert_eq!(body["include"], include);
    let mut plain = Machine::new("gpt-6.1-sol".into(), "id".into(), BTreeSet::new(), true);
    let mut twice = items[3].clone();
    let annotation = text["annotations"][0].clone();
    twice["content"][0]["annotations"] = json!([annotation, annotation]);
    let mut cited_once = Vec::new();
    for raw in [created(), added(0, &twice), done(0, &twice)] {
        cited_once.extend(
            plain
                .apply(raw)
                .unwrap()
                .into_iter()
                .filter_map(|e| match e {
                    ProviderEvent::Search { index, signal } => Some(row(index, &signal)),
                    _ => None,
                }),
        );
    }
    assert_eq!(cited_once, [cited.replacen('7', "1", 1)]);
    assert!(plain.apply(completed(&[])).is_ok());
}

async fn search_fallback(
    first: (&'static str, String),
    official: bool,
    search: bool,
) -> Vec<Value> {
    let ok = || stream_reply("200 OK", &[], &data(&[created(), completed(&[message()])]));
    let mut replies = vec![stream_reply(first.0, &[], &first.1)];
    replies.extend(search.then(ok));
    replies.push(ok());
    let (mut provider, server) = serving(replies).await;
    provider.official = official;
    let mut agent =
        crate::agent::Agent::new(std::sync::Arc::new(provider), session()).with_web_search(search);
    for text in ["one", "two"] {
        turn(&mut agent, text, &[]).await;
    }
    bodies(&server)
}

#[tokio::test]
async fn a_refused_search_tool_falls_back_in_each_form() {
    let refusal = |message: &str| {
        let body = json!({"error":{"type":"invalid_request_error","message":message}});
        ("400 Bad Request", body.to_string())
    };
    let off = refusal("Web search is not enabled for this account");
    let stream = |error_type| {
        let failed = json!({"type":"response.failed","response":{"status":"failed","error":{"type":error_type,"message":"web_search is not supported for this model"}}});
        ("200 OK", data(&[created(), failed]))
    };
    let tools = |body: &Value| body["tools"].to_string();
    let forms = [
        (off.clone(), true),
        (stream("invalid_request_error"), true),
        (stream("invalid_request"), true),
        (refusal("Unknown tool type"), false),
    ];
    for (first, official) in forms {
        let bodies = search_fallback(first, official, true).await;
        let [refused, resend, next] = bodies.as_slice() else {
            panic!("{bodies:?}");
        };
        assert!(tools(refused).contains("web_search"));
        assert_eq!(refused["include"][1], "web_search_call.action.sources");
        for body in [resend, next] {
            assert!(!tools(body).contains("web_search"));
            assert_eq!(body["include"], json!(["reasoning.encrypted_content"]));
        }
        assert_eq!(resend["prompt_cache_key"], next["prompt_cache_key"]);
        assert_ne!(resend["prompt_cache_key"], refused["prompt_cache_key"]);
    }
    let bodies = search_fallback(off, true, false).await;
    assert_eq!(bodies.len(), 2);
    assert!(
        bodies
            .iter()
            .all(|body| !tools(body).contains("web_search"))
    );
}

#[test]
fn search_items_replay_verbatim_only_on_the_subscription_with_the_tool() {
    let items = search_items();
    let content: Vec<ContentBlock> = items
        .iter()
        .enumerate()
        .flat_map(|(oi, item)| {
            let projections = usize::from(oi == 3);
            let envelope = ContentBlock::opaque(PROVIDER, json!({"type":wisp_protocol::NATIVE_ITEM,"item":item,"projection_blocks":projections,"identity":"IDENTITY"}));
            let text = item["content"][0]["text"].as_str().map(|text| ContentBlock::Text { text: text.into() });
            std::iter::once(envelope).chain(text)
        })
        .collect();
    let session = session();
    let rendering = Rendering::default();
    let subscription = subscription();
    let api = provider();
    for (provider, search, verbatim) in [
        (&subscription, true, true),
        (&subscription, false, false),
        (&api, true, false),
    ] {
        let mut content = content.clone();
        for block in &mut content {
            if let ContentBlock::Opaque { raw, .. } = block {
                raw["identity"] = provider.identity.clone().into();
            }
        }
        let mut messages = vec![
            Message::user_text("Question"),
            Message {
                role: Role::Assistant,
                content,
            },
        ];
        let input = |messages: &[Message]| {
            let mut sent = request(&session, messages, &[], &rendering);
            sent.server_tools = if search {
                search_tool("web_search")
            } else {
                &[]
            };
            provider.body(&sent).unwrap()["input"].to_string()
        };
        let text = input(&messages);
        messages.push(Message::user_text("Next"));
        assert!(input(&messages).starts_with(text.strip_suffix(']').unwrap()));
        assert_eq!(text.contains("https://example.com/source-a"), verbatim);
        assert_eq!(
            text.contains("[web search: \\\"example query\\\"]"),
            !verbatim
        );
    }
}

fn signed_in(profile: &str) -> OpenAiProvider {
    let credential = Credential::Subscription {
        kind: SubscriptionKind::ChatGpt,
        secret: "synthetic-oauth".into(),
        profile: Some(profile.into()),
    };
    OpenAiProvider::from_endpoint(&endpoint(credential, CHATGPT_BASE_URL)).unwrap()
}

#[test]
fn a_subscriptions_list_gives_levels_default_and_images_and_the_tables_the_rest() {
    let list = list::tests::sample();
    let sol = listed_capabilities(Some(&list), "gpt-6.1-sol").unwrap();
    assert_eq!(sol.efforts, Effort::ALL);
    assert_eq!(sol.default_effort, Some(Effort::Low));
    assert_eq!(sol.images, GPT.images);
    assert_eq!(sol.max_output, Some(128_000));
    let new = listed_capabilities(Some(&list), "gpt-new").unwrap();
    assert_eq!(new.max_output, None);
    for caps in [sol, new] {
        let flags = [caps.per_message_effort, caps.inline_tools];
        assert_eq!(flags, [false; 2]);
        assert_eq!([caps.deferred_tools, caps.adaptive_thinking], [false; 2]);
    }
    let shipped = |id| model_capabilities(id).copied();
    assert_eq!(
        listed_capabilities(Some(&list), "gpt-5.5"),
        shipped("gpt-5.5")
    );
    assert_eq!(
        listed_capabilities(None, "gpt-6.1-sol"),
        shipped("gpt-6.1-sol")
    );
    assert_eq!(listed_capabilities(None, "gpt-new"), None);
    let mut odd = list::tests::entry("gpt-6.1-sol", 1, 500_000);
    odd["supported_reasoning_levels"] = serde_json::json!([{ "effort": "ultra" }]);
    let odd = ModelList::parse(&list::tests::wire(&[odd])).unwrap();
    let caps = listed_capabilities(Some(&odd), "gpt-6.1-sol");
    assert_eq!(caps, shipped("gpt-6.1-sol"));
}

#[tokio::test]
async fn a_fetched_list_is_saved_under_its_sign_in_and_a_failed_fetch_keeps_the_last() {
    let dir = tempfile::tempdir().unwrap();
    let folder = dir.path().join("models");
    let provider = signed_in("synthetic-profile").with_clock(crate::clock::TickingClock::new());
    let key = provider.list_key().unwrap().to_owned();
    assert_eq!(key.len(), 32);
    let answer = |reply: http::Reply| {
        let client = provider.client.clone();
        async move {
            let server = http::serve(vec![reply]).await;
            let response = client.get(format!("{}/models", server.url)).send().await;
            response.map_err(|_| ProviderError::malformed("unreachable"))
        }
    };
    let body = String::from_utf8(list::tests::wire(&[
        list::tests::entry("gpt-6.1-sol", 1, 500_000),
        list::tests::entry("gpt-new", 2, 272_000),
    ]))
    .unwrap();
    let reply = |status: &str| http::Reply::Send(http::json(status, &body));
    let too_long = format!(
        "HTTP/1.1 200 OK\r\ncontent-length: {}\r\n\r\n",
        LIST_LIMIT + 1
    );
    let failures = || {
        let head = http::Reply::HeadThenHang(too_long.clone());
        [reply("500 Internal Server Error"), head]
    };
    let [refused, _] = failures();
    provider.save_fetched(answer(refused).await, &folder).await;
    let none = SavedList::load(&folder, &key).unwrap();
    let near_next_round = list::ROUND_EVERY.as_secs().cast_signed() - 1;
    assert!(none.failed);
    assert!(none.stale(none.attempted + near_next_round));
    assert_eq!(
        (none.list.ids().len(), none.stale(none.attempted + 60)),
        (0, false)
    );
    let reopened = signed_in("synthetic-profile").with_saved_list(Some(&folder));
    assert!(reopened.model_list().is_none());
    provider
        .save_fetched(answer(reply("200 OK")).await, &folder)
        .await;
    let first = SavedList::load(&folder, &key).unwrap();
    assert_eq!(first.list, list::tests::sample());
    assert!(!first.failed);
    assert!(!first.stale(first.attempted + list::REFRESH_AFTER - 1));
    assert!(first.stale(first.attempted + list::REFRESH_AFTER));
    for failure in failures() {
        provider.save_fetched(answer(failure).await, &folder).await;
        let saved = SavedList::load(&folder, &key).unwrap();
        assert_eq!(saved.list, first.list);
        assert!(saved.attempted > first.attempted);
        assert!(saved.stale(saved.attempted + near_next_round));
    }
    let token_only = subscription();
    assert_eq!(token_only.list_key(), None);
    let failed = Err(ProviderError::malformed("unreachable"));
    token_only.save_fetched(failed, dir.path()).await;
    assert_eq!(std::fs::read_dir(dir.path()).unwrap().count(), 1);
}

#[test]
fn a_connection_reads_its_list_once_so_a_newer_save_changes_no_byte_it_sends() {
    let dir = tempfile::tempdir().unwrap();
    let key = signed_in("synthetic-profile")
        .list_key()
        .unwrap()
        .to_owned();
    let first = list::tests::sample();
    SavedList {
        attempted: 1,
        failed: false,
        list: first.clone(),
        default_model: None,
        web_search_models: Vec::new(),
    }
    .save(dir.path(), &key)
    .unwrap();
    let connected = signed_in("synthetic-profile").with_saved_list(Some(dir.path()));
    let session = session();
    let rendering = Rendering::default();
    let messages = [Message::user_text("question")];
    let request = request(&session, &messages, &[], &rendering);
    let sent = |provider: &OpenAiProvider| {
        let body = provider.body(&request).unwrap().to_string();
        (provider.capabilities("gpt-6.1-sol"), body)
    };
    let before = sent(&connected);
    assert_eq!(before.1, sent(&signed_in("synthetic-profile")).1);
    let mut changed = list::tests::entry("gpt-6.1-sol", 2, 272_000);
    changed["default_reasoning_level"] = json!("high");
    let added = list::tests::entry("gpt-added", 1, 100_000);
    let newer = ModelList::parse(&list::tests::wire(&[added, changed])).unwrap();
    SavedList {
        attempted: 2,
        failed: false,
        list: newer.clone(),
        default_model: None,
        web_search_models: Vec::new(),
    }
    .save(dir.path(), &key)
    .unwrap();
    assert_eq!(sent(&connected), before);
    assert_eq!(connected.model_list().as_deref(), Some(&first));
    let fresh = signed_in("synthetic-profile").with_saved_list(Some(dir.path()));
    assert_eq!(fresh.model_list().as_deref(), Some(&newer));
    let other = signed_in("synthetic-other").with_saved_list(Some(dir.path()));
    assert_eq!(other.model_list(), None);
}

#[tokio::test]
async fn a_connections_own_fetch_adds_models_and_changes_no_byte_it_sends_for_a_name_it_has() {
    let dir = tempfile::tempdir().unwrap();
    let key = signed_in("synthetic-profile")
        .list_key()
        .unwrap()
        .to_owned();
    let first = list::tests::sample();
    let saved = SavedList {
        attempted: 1,
        failed: false,
        list: first.clone(),
        default_model: None,
        web_search_models: Vec::new(),
    };
    saved.save(dir.path(), &key).unwrap();
    let clock = crate::clock::TickingClock::new();
    let connected = signed_in("synthetic-profile").with_saved_list(Some(dir.path()));
    let connected = connected.with_clock(clock.clone());
    let asked = |provider: &OpenAiProvider, model: &str| {
        let mut session = session();
        session.model = model.into();
        let rendering = Rendering::default();
        let messages = [Message::user_text("question")];
        let request = request(&session, &messages, &[], &rendering);
        let body = provider.body(&request).unwrap().to_string();
        (provider.capabilities(model), body)
    };
    let (sol, late) = (
        asked(&connected, "gpt-6.1-sol"),
        asked(&connected, "gpt-late"),
    );
    assert_eq!(late.0, None);
    let mut session = session();
    session.model = "gpt-late".into();
    let rendering = Rendering::default();
    let messages = [Message::user_text("question")];
    drop(connected.stream(&request(&session, &messages, &[], &rendering)));
    let mut changed = list::tests::entry("gpt-6.1-sol", 2, 272_000);
    changed["default_reasoning_level"] = json!("high");
    let wire = list::tests::wire(&[
        list::tests::entry("gpt-added", 1, 100_000),
        changed,
        list::tests::entry("gpt-late", 3, 272_000),
    ]);
    let body = String::from_utf8(wire).unwrap();
    let fetch = |provider: &OpenAiProvider| {
        let client = provider.client.clone();
        let reply = http::Reply::Send(http::json("200 OK", &body));
        async move {
            let server = http::serve(vec![reply]).await;
            let response = client.get(format!("{}/models", server.url)).send().await;
            response.map_err(|_| ProviderError::malformed("unreachable"))
        }
    };
    connected
        .save_fetched(fetch(&connected).await, dir.path())
        .await;
    assert_eq!(
        connected.model_list().unwrap().ids(),
        ["gpt-6.1-sol", "gpt-new", "gpt-added"]
    );
    assert!(connected.capabilities("gpt-added").is_some());
    assert_eq!(asked(&connected, "gpt-6.1-sol"), sol);
    assert_eq!(asked(&connected, "gpt-late"), late);
    let reopened = signed_in("synthetic-profile").with_saved_list(Some(dir.path()));
    assert_ne!(asked(&reopened, "gpt-6.1-sol"), sol);
    assert!(asked(&reopened, "gpt-late").0.is_some());
    let none = signed_in("synthetic-other").with_clock(clock);
    none.save_fetched(fetch(&none).await, dir.path()).await;
    assert_eq!(none.model_list(), None);
    assert_eq!(none.capabilities("gpt-added"), None);
    let other = none.list_key().unwrap();
    assert!(
        SavedList::load(dir.path(), other)
            .unwrap()
            .list
            .find("gpt-added")
            .is_some()
    );
}

#[test]
fn openai_body_rejects_foreign_provider_models() {
    let provider = provider();
    let mut session = session();
    session.model = "gemini-3.1-pro-preview".into();
    let rendering = Rendering::default();
    let messages = [Message::user_text("hi")];
    let err = provider
        .body(&request(&session, &messages, &[], &rendering))
        .unwrap_err();
    assert_eq!(err.kind, ErrorKind::InvalidRequest);
}

#[tokio::test]
async fn the_model_list_url_uses_the_codex_client_version_only_for_subscriptions() {
    let subscription = signed_in("synthetic-profile");
    let client = subscription.client_version.as_ref().unwrap();
    assert!(client.latest.is_some());
    let pin = ClientVersion::parse("0.170.0").unwrap();
    let pinned = OpenAiProvider::from_endpoint(&ResolvedEndpoint {
        client_version: Some(pin),
        ..endpoint(subscription.credential.clone(), CHATGPT_BASE_URL)
    })
    .unwrap();
    let row = pinned.client_version.as_ref().unwrap();
    let reported = client_version::resolve(row, None, ProviderKind::OpenAi).await;
    assert_eq!(reported, pin);
    for (provider, version) in [(&subscription, client.version), (&pinned, reported)] {
        let url = reqwest::Url::parse(&provider.models_url(Some(version)).unwrap()).unwrap();
        assert_eq!(url.path(), "/backend-api/codex/models");
        assert_eq!(
            url.query_pairs().collect::<Vec<_>>(),
            [("client_version".into(), version.to_string().into())]
        );
    }
    let api = provider();
    assert!(api.client_version.is_none());
    assert_eq!(
        api.models_url(None).unwrap(),
        format!("{DEFAULT_BASE_URL}/v1/models")
    );
    let gateway = OpenAiProvider::from_endpoint(&endpoint(
        Credential::ApiKey("CANARY_GATEWAY".into()),
        "https://gateway.example/openai",
    ))
    .unwrap();
    assert!(gateway.client_version.is_none());
}

#[tokio::test]
async fn subscription_401_refresh_rebuilds_scrubbed_body_and_preserves_one_refresh_limit() {
    use crate::agent::testing::{brief_turn, checklist_mail, prime_brief};
    use crate::agent::{Agent, IMPLICIT_ENDPOINT};
    use crate::lanes::Lanes;
    use crate::session::{MemoryStore, SessionLog};
    use wisp_protocol::{ErrorKind, TurnOutcome};

    struct RefreshingOpenAi {
        provider: OpenAiProvider,
        profile: PathBuf,
        url: String,
    }
    impl Provider for RefreshingOpenAi {
        fn stream(
            &self,
            request: &Request<'_>,
        ) -> BoxStream<'static, Result<ProviderEvent, ProviderError>> {
            self.provider
                .stream_refreshing(request, refresh_at(self.profile.clone(), self.url.clone()))
        }

        fn cache_prefix(&self, request: &Request<'_>) -> Option<String> {
            self.provider.cache_prefix(request)
        }
    }

    for succeeds in [false, true] {
        crate::session::reset_held_for_test();
        let canary = format!("synthetic-new-access-CANARY-{succeeds}");
        let response = if succeeds {
            stream_reply("200 OK", &[], &data(&[created(), completed(&[message()])]))
        } else {
            stream_reply("401 Unauthorized", &[], "{}")
        };
        let (_folder, profile, provider, server) =
            refreshing_subscription(vec![stream_reply("401 Unauthorized", &[], "{}"), response])
                .await;
        let token_server = http::serve(vec![http::Reply::Send(http::json("200 OK", &json!({"access_token":canary,"refresh_token":"synthetic-refresh-CANARY","expires_in":3600}).to_string()))]).await;
        let provider = Arc::new(RefreshingOpenAi {
            provider,
            profile,
            url: token_server.url.clone(),
        });
        let lanes = Lanes::new([(IMPLICIT_ENDPOINT.to_owned(), Some(1))].into(), None);
        let store = MemoryStore::default();
        let mut agent = Agent::new(provider, session())
            .with_lanes(lanes.clone())
            .with_log(
                SessionLog::new(Box::new(store.clone()), "s1"),
                crate::agent::LogContext {
                    endpoint_name: IMPLICIT_ENDPOINT.into(),
                    ..log_context()
                },
            );
        prime_brief(
            &mut agent,
            checklist_mail("refresh", "Review the code", &[&canary]),
        )
        .await;
        let (outcome, events) = brief_turn(&mut agent).await;
        if succeeds {
            assert!(
                matches!(outcome, TurnOutcome::Completed { .. }),
                "{outcome:?}"
            );
        } else {
            assert!(
                matches!(outcome, TurnOutcome::Error { kind: ErrorKind::Auth, message, .. } if message.contains("sign in again with /account"))
            );
        }
        assert!(
            events
                .iter()
                .all(|event| !matches!(event, wisp_protocol::AgentEvent::Retrying { .. }))
        );
        assert_eq!(asked(&token_server), 1);
        let requests = server.requests.lock().unwrap();
        assert_eq!(requests.len(), 2);
        assert!(http::body(&requests[0]).contains(&canary));
        let second = http::body(&requests[1]);
        assert!(
            second.contains("Review the code")
                && second.contains("[redacted]")
                && !second.contains(&canary)
        );
        assert!(lanes.enter(IMPLICIT_ENDPOINT, 0).is_ok());
        assert!(
            crate::test_guard::log_events(&store)
                .iter()
                .any(|e| matches!(
                    e,
                    wisp_protocol::log::LogEvent::InputRevised { turn: Some(1), .. }
                ))
        );
    }
}
