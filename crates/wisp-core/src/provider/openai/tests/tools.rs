use super::*;
use crate::provider::{AssembledMessage, Feature, FeatureUse};
use wisp_protocol::ToolDefinition;

fn tool(name: &str, field: &str, kind: &str) -> ToolSpec {
    ToolSpec {
        name: name.into(),
        description: "A harmless local test. Call it exactly as requested.".into(),
        input_schema: json!({"type":"object","properties":{field:{"type":kind}},"required":[field],"additionalProperties":false}),
    }
}

pub(super) fn added(spec: &ToolSpec) -> ContentBlock {
    ContentBlock::ToolAddition {
        tool: ToolRef::ToolDefinition {
            definition: ToolDefinition {
                name: spec.name.clone(),
                description: spec.description.clone(),
                input_schema: spec.input_schema.clone(),
            },
        },
    }
}

fn named(name: &str) -> ToolRef {
    ToolRef::ToolReference { name: name.into() }
}

fn established() -> Features {
    Features::using(&[Feature::InlineTools], FeatureUse::Established)
}

async fn live_response(
    provider: &OpenAiProvider,
    mut request: Request<'_>,
    stage: &str,
    expected: Option<(&str, Value)>,
    sent: &mut usize,
) -> AssembledMessage {
    assert!(*sent < 12, "the approved live request cap was reached");
    *sent += 1;
    request.features = established();
    let mut assembler = Assembler::default();
    let result = tokio::time::timeout(Duration::from_secs(90), async {
        let mut stream = provider.stream(&request);
        while let Some(event) = stream.next().await {
            assembler.apply(event?)?;
        }
        Ok::<(), ProviderError>(())
    })
    .await
    .unwrap_or_else(|_| {
        Err(ProviderError::new(
            ErrorKind::Timeout,
            "live request timed out",
        ))
    });
    let usage = assembler.usage().unwrap_or_default();
    let response = result.and_then(|()| assembler.finish());
    let usage = response.as_ref().map_or(usage, |r| r.usage);
    let cached = usage.cache_read_input_tokens;
    let calls: Vec<_> = response
        .as_ref()
        .ok()
        .into_iter()
        .flat_map(|r| &r.content)
        .filter_map(|b| {
            if let ContentBlock::ToolUse { name, input, .. } = b {
                Some((name.as_str(), input))
            } else {
                None
            }
        })
        .collect();
    let called = match &expected {
        Some((name, input)) => calls == vec![(*name, input)],
        None => calls.is_empty(),
    };
    let cache_passed = stage == "prime" || cached > 0;
    let passed = response.is_ok() && called && cache_passed;
    let error = response.as_ref().err();
    let names: Vec<_> = calls
        .iter()
        .map(|(name, _)| match *name {
            "probe_base" => "probe_base",
            "probe_added" => "probe_added",
            _ => "unknown",
        })
        .collect();
    let stop = response.as_ref().map_or("none", |r| match &r.stop_reason {
        StopReason::Other(_) => "other",
        reason => reason.as_str(),
    });
    eprintln!(
        "model={} request={} stage={stage} pass={passed} call_pass={called} cache_pass={cache_passed} error_kind={:?} http_status={:?} called_tools={names:?} stop={stop} input_tokens={} cached_tokens={cached} output_tokens={}",
        request.session.model,
        *sent,
        error.map(|e| e.kind),
        error.and_then(|e| e.detail.status),
        usage.input_tokens,
        usage.output_tokens
    );
    assert!(
        response.is_ok() && called,
        "live call failed; stop this run"
    );
    response.expect("a passing call completed")
}

fn live_inputs(
    model: &str,
    filler_rows: usize,
) -> (SessionConfig, [ToolSpec; 1], Rendering, Vec<Message>) {
    let mut config = session();
    config.model = model.into();
    config.effort = Effort::Low;
    config.max_tokens = 128;
    let mut instructions = String::from(
        "This is a harmless tool and cache test. Follow the latest request. Use a requested tool when available. Keep final replies under ten words. Never repeat filler or discuss reasoning.\n",
    );
    for index in 0..filler_rows {
        instructions.push_str(&format!("Fixed reference row {index:04}: amber birch cedar dune elm fern glen hazel iris juniper.\n"));
    }
    config.system = Some(instructions);
    (
        config,
        [tool("probe_base", "token", "string")],
        Rendering::default(),
        vec![Message::user_text("Reply OK.")],
    )
}

#[tokio::test]
#[ignore = "live: up to twelve short requests on the ChatGPT subscription, including two warm-up attempts per model; only with the owner's approval"]
async fn subscription_additional_tools_live() {
    let provider = super::voice::signed_in();
    assert!(
        provider.subscription,
        "the live check requires the subscription"
    );
    let lists = crate::models::ModelTable::shared()
        .provider(ProviderKind::OpenAi)
        .unwrap();
    additional_tools_live(&provider, &lists.subscription_additional_tools, 320).await;
}

#[tokio::test]
#[ignore = "live: up to twelve short requests on an OpenAI API account, with API cost and two warm-up attempts per model; only with the owner's approval"]
async fn api_additional_tools_live() {
    let provider = super::voice::signed_in();
    assert!(
        !provider.subscription
            && provider.official
            && matches!(provider.credential, Credential::ApiKey(_)),
        "the live check requires an official OpenAI API-key account"
    );
    let lists = crate::models::ModelTable::shared()
        .provider(ProviderKind::OpenAi)
        .unwrap();
    additional_tools_live(&provider, &lists.api_additional_tools, 80).await;
}

async fn additional_tools_live(provider: &OpenAiProvider, models: &[String], filler_rows: usize) {
    let mut sent = 0;
    for model in models {
        let (config, tools, rendering, mut messages) = live_inputs(model, filler_rows);
        let first = live_response(
            provider,
            request(&config, &messages, &tools, &rendering),
            "prime",
            None,
            &mut sent,
        )
        .await;
        messages.push(first.into_message());
        messages.push(Message::user_text("Reply OK, without calling a tool."));
        let mut warmed = None;
        for _ in 0..2 {
            tokio::time::sleep(Duration::from_secs(2)).await;
            let reply = live_response(
                provider,
                request(&config, &messages, &tools, &rendering),
                "warmup",
                None,
                &mut sent,
            )
            .await;
            if reply.usage.cache_read_input_tokens > 0 {
                warmed = Some(reply);
                break;
            }
        }
        let warmed = warmed.expect("cache not warm; stop this run");
        messages.push(warmed.into_message());
        messages.push(Message::user_text(
            "Call probe_added once with token ITEM. Do not call probe_base.",
        ));
        messages.push(
            Message::turn_system(
                None,
                &[
                    added(&tool("probe_added", "token", "string")),
                    ContentBlock::ToolRemoval {
                        tool: named("probe_base"),
                    },
                ],
            )
            .unwrap(),
        );
        let second = live_response(
            provider,
            request(&config, &messages, &tools, &rendering),
            "addition",
            Some(("probe_added", json!({"token":"ITEM"}))),
            &mut sent,
        )
        .await;
        assert!(
            second.usage.cache_read_input_tokens > 0,
            "cache lost on the tool addition"
        );
        let call_id = second
            .content
            .iter()
            .find_map(|b| {
                if let ContentBlock::ToolUse { id, .. } = b {
                    Some(id.clone())
                } else {
                    None
                }
            })
            .unwrap();
        messages.push(second.into_message());
        messages.push(Message {
            role: Role::User,
            content: vec![ContentBlock::ToolResult {
                tool_use_id: call_id,
                content: vec![ContentBlock::Text { text: "OK".into() }],
                is_error: false,
            }],
        });
        messages.push(Message::user_text("Reply OK, without calling a tool."));
        let third = live_response(
            provider,
            request(&config, &messages, &tools, &rendering),
            "replay",
            None,
            &mut sent,
        )
        .await;
        assert!(
            third.usage.cache_read_input_tokens > 0,
            "cache lost on replay"
        );
        messages.push(third.into_message());
        messages.push(Message::user_text(
            "The latest probe_base is now on. Call it once with number 7 and no token.",
        ));
        messages.push(
            Message::turn_system(None, &[added(&tool("probe_base", "number", "integer"))]).unwrap(),
        );
        let fourth = live_response(
            provider,
            request(&config, &messages, &tools, &rendering),
            "redeclare",
            Some(("probe_base", json!({"number":7}))),
            &mut sent,
        )
        .await;
        assert!(
            fourth.usage.cache_read_input_tokens > 0,
            "cache lost on the changed definition"
        );
    }
}

#[test]
fn tool_changes_render_at_their_message_as_sorted_definitions_then_notes_then_text() {
    let provider = provider();
    let config = session();
    let rendering = Rendering::default();
    let tools = [tool("base", "token", "string")];
    let messages = [
        Message::user_text("use alpha"),
        Message::turn_system(
            None,
            &[
                added(&tool("zebra", "token", "string")),
                ContentBlock::ToolRemoval {
                    tool: named("base"),
                },
                added(&tool("alpha", "token", "string")),
                ContentBlock::ToolAddition {
                    tool: named("base"),
                },
                ContentBlock::Text {
                    text: "Latest instructions".into(),
                },
            ],
        )
        .unwrap(),
    ];
    let original = request(&config, &messages[..1], &tools, &rendering);
    let prefix = provider.prefix_bytes(&original);
    let first = provider.body(&original).unwrap();
    let mut request = request(&config, &messages, &tools, &rendering);
    request.features = established();
    let (body, declared) = provider.render(&request).unwrap();
    for field in ["tools", "instructions", "prompt_cache_key"] {
        assert_eq!(body[field], first[field], "{field}");
    }
    assert_eq!(provider.prefix_bytes(&request), prefix);
    assert_eq!(
        body["input"],
        json!([
            {"role":"user","content":[{"type":"input_text","text":"use alpha"}]},
            {"type":"additional_tools","role":"developer","tools":[
                {"type":"function","name":"alpha","description":"A harmless local test. Call it exactly as requested.","parameters":{"type":"object","properties":{"token":{"type":"string"}},"required":["token"],"additionalProperties":false},"strict":false},
                {"type":"function","name":"zebra","description":"A harmless local test. Call it exactly as requested.","parameters":{"type":"object","properties":{"token":{"type":"string"}},"required":["token"],"additionalProperties":false},"strict":false}
            ]},
            {"role":"developer","content":[{"type":"input_text","text":"Tools turned off: base.\nTools turned on: base."}]},
            {"role":"developer","content":[{"type":"input_text","text":"Latest instructions"}]}
        ])
    );
    assert_eq!(
        declared,
        BTreeSet::from(["alpha".into(), "base".into(), "zebra".into()])
    );
}

#[test]
fn by_name_definitions_are_declared_and_unrendered_or_undeclared_changes_are_omitted() {
    let provider = provider();
    let config = session();
    let tools = [tool("base", "token", "string")];
    let extra = [tool("extra", "token", "string")];
    let messages = [Message::turn_system(
        None,
        &[
            ContentBlock::ToolAddition {
                tool: named("extra"),
            },
            ContentBlock::ToolRemoval {
                tool: named("extra"),
            },
            ContentBlock::ToolAddition {
                tool: named("unknown"),
            },
            ContentBlock::ToolRemoval {
                tool: named("unknown"),
            },
            added(&tools[0]),
        ],
    )
    .unwrap()];
    let rendering = Rendering {
        tools_point: 1,
        array: BTreeSet::from(["base".into()]),
        ..Rendering::default()
    };
    let mut request = request(&config, &messages, &tools, &rendering);
    request.features = established();
    request.by_value = &extra;
    let (body, declared) = provider.render(&request).unwrap();
    assert_eq!(
        body["input"],
        json!([
            {"type":"additional_tools","role":"developer","tools":[function_tool(&extra[0].name, &extra[0].description, &extra[0].input_schema)]},
            {"role":"developer","content":[{"type":"input_text","text":"Tools turned off: extra."}]}
        ])
    );
    assert_eq!(declared, BTreeSet::from(["base".into(), "extra".into()]));
    request.features = Features::default();
    let (body, declared) = provider.render(&request).unwrap();
    assert_eq!(body["input"], json!([]));
    assert_eq!(declared, BTreeSet::from(["base".into()]));
}

#[test]
fn additional_tools_match_their_quoted_source_and_exact_connection() {
    let table = crate::models::ModelTable::shared();
    let lists = table.provider(ProviderKind::OpenAi).unwrap();
    let gateway = OpenAiProvider::from_endpoint(&endpoint(
        Credential::ApiKey("synthetic-openai-key".into()),
        "https://gateway.example/v1",
    ))
    .unwrap();
    for (connection, models, source) in [
        (
            subscription(),
            lists.subscription_additional_tools.as_slice(),
            "subscription_additional_tools",
        ),
        (
            provider(),
            lists.api_additional_tools.as_slice(),
            "api_additional_tools",
        ),
        (gateway, &[][..], "api_additional_tools"),
    ] {
        let quote = &lists.sources[source];
        assert!(quote.contains("https://developers.openai.com/api/docs/guides/tools-tool-search"));
        for model in models {
            assert!(quote.contains(model), "{model}");
            assert!(table.capabilities(ProviderKind::OpenAi, model).is_some());
            assert!(
                !connection
                    .capabilities(&format!("{model}-dated"))
                    .is_some_and(|caps| caps.inline_tools)
            );
        }
        for row in table
            .rows()
            .iter()
            .filter(|row| row.provider == Some(ProviderKind::OpenAi))
        {
            assert_eq!(
                connection
                    .capabilities(&row.id)
                    .is_some_and(|caps| caps.inline_tools),
                models.contains(&row.id),
                "{}: {}",
                source,
                row.id
            );
        }
        assert!(
            !connection
                .capabilities("unknown")
                .is_some_and(|caps| caps.inline_tools)
        );
    }
    assert!(subscription().features_documented());
    assert!(!provider().features_documented());
}

#[tokio::test]
async fn only_an_invalid_request_naming_a_sent_additional_tools_item_rejects_inline_tools() {
    for (status, kind, text, features, rejects) in [
        (
            "400 Bad Request",
            "invalid_request_error",
            "additional_tools is not supported",
            established(),
            true,
        ),
        (
            "422 Unprocessable Entity",
            "invalid_request_error",
            "additional_tools is not supported",
            established(),
            true,
        ),
        (
            "400 Bad Request",
            "invalid_request_error",
            "some other field is not supported",
            established(),
            false,
        ),
        (
            "400 Bad Request",
            "invalid_request_error",
            "additional_tools is not supported",
            Features::default(),
            false,
        ),
        (
            "500 Internal Server Error",
            "server_error",
            "additional_tools failed",
            established(),
            false,
        ),
    ] {
        let reply = json!({"error":{"type":kind,"message":text}}).to_string();
        let (provider, _server) = serving(vec![stream_reply(status, &[], &reply)]).await;
        let config = session();
        let rendering = Rendering::default();
        let messages =
            [Message::turn_system(None, &[added(&tool("extra", "token", "string"))]).unwrap()];
        let mut request = request(&config, &messages, &[], &rendering);
        request.features = features;
        let error = provider
            .stream(&request)
            .filter_map(|event| futures::future::ready(event.err()))
            .next()
            .await
            .unwrap();
        assert_eq!(
            error.rejected,
            if rejects {
                vec![Feature::InlineTools]
            } else {
                vec![]
            },
            "{status}: {text}"
        );
    }
}

#[test]
fn an_additional_tools_item_follows_the_complete_function_output_batch() {
    let provider =
        OpenAiProvider::from_endpoint(&endpoint(Credential::ApiKey("x".into()), DEFAULT_BASE_URL))
            .unwrap();
    let config = session();
    let tools = [tool("base", "token", "string")];
    let extra = tool("extra", "number", "integer");
    let mut messages = vec![
        Message::user_text("Find extra."),
        Message {
            role: Role::Assistant,
            content: vec![
                ContentBlock::ToolUse {
                    id: "one".into(),
                    name: "base".into(),
                    input: json!({"token":"one"}),
                },
                ContentBlock::ToolUse {
                    id: "two".into(),
                    name: "base".into(),
                    input: json!({"token":"two"}),
                },
            ],
        },
        Message {
            role: Role::User,
            content: vec![
                ContentBlock::ToolResult {
                    tool_use_id: "one".into(),
                    content: vec![],
                    is_error: false,
                },
                ContentBlock::ToolResult {
                    tool_use_id: "two".into(),
                    content: vec![],
                    is_error: false,
                },
            ],
        },
    ];
    let rendering = Rendering::default();
    let before = provider
        .render(&request(&config, &messages, &tools, &rendering))
        .unwrap()
        .0;
    messages.push(Message::turn_system(None, &[added(&extra)]).unwrap());
    let mut changed = request(&config, &messages, &tools, &rendering);
    changed.features = established();
    let after = provider.render(&changed).unwrap().0;
    assert_eq!(before["tools"], after["tools"]);
    let items = after["input"].as_array().unwrap();
    assert_eq!(items[items.len() - 3]["type"], "function_call_output");
    assert_eq!(items[items.len() - 2]["type"], "function_call_output");
    assert_eq!(items.last().unwrap()["type"], "additional_tools");
    assert_eq!(items.last().unwrap()["tools"][0]["name"], "extra");
}

#[test]
fn after_output_capabilities_are_separate_exact_id_lists_and_gateways_never_inherit_them() {
    let lists = crate::models::ModelTable::shared()
        .provider(ProviderKind::OpenAi)
        .unwrap();
    for (url, subscription) in [
        (DEFAULT_BASE_URL, false),
        (DEFAULT_BASE_URL, true),
        ("https://gateway.example/v1", false),
    ] {
        let mut provider =
            OpenAiProvider::from_endpoint(&endpoint(Credential::ApiKey("x".into()), url)).unwrap();
        if subscription {
            provider = provider.on_subscription();
        }
        for model in ["gpt-6.1-sol", "gpt-6-astra"] {
            let expected = url == DEFAULT_BASE_URL
                && if subscription {
                    &lists.subscription_tools_after_outputs
                } else {
                    &lists.api_tools_after_outputs
                }
                .iter()
                .any(|id| id == model);
            assert_eq!(
                provider.capabilities(model).unwrap().tools_after_results,
                expected
            );
        }
    }
}
