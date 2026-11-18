use super::*;
use crate::provider::openai::OpenAiProvider;
use crate::provider::openai::tests::{completed, created, data, endpoint, message};

const MODEL: &str = "gpt-6.1-sol";

pub(super) fn subscribed(server: &Server) -> Arc<dyn Provider> {
    let endpoint = endpoint(
        Credential::ApiKey("synthetic-openai-key".into()),
        &server.base,
    );
    Arc::new(
        OpenAiProvider::from_endpoint(&endpoint)
            .unwrap()
            .on_subscription(),
    )
}

pub(super) fn gpt(items: &[Value]) -> Reply {
    let mut done = completed(items);
    done["response"]["usage"]["input_tokens"] = 40_000.into();
    Reply {
        response: http::sse(&data(&[created(), done])),
        before: Vec::new(),
        gate: None,
    }
}

fn stable(server: &Server, range: std::ops::Range<usize>) {
    let bodies: Vec<_> = range.map(|n| body(server, n)).collect();
    for pair in bodies.windows(2) {
        assert_eq!(pair[0]["prompt_cache_key"], pair[1]["prompt_cache_key"]);
        assert!(
            pair[1]["input"]
                .as_array()
                .unwrap()
                .starts_with(pair[0]["input"].as_array().unwrap())
        );
    }
}

fn additions(body: &Value) -> Vec<&Value> {
    body["input"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|item| item["type"] == "additional_tools")
        .collect()
}

#[tokio::test]
async fn a_new_tool_runs_and_off_and_on_keep_the_declared_prefix() {
    let env = Env::new("openai-inline-switches");
    let call = json!({"type":"function_call","id":"fc_show","call_id":"call_show","name":"show_diff","arguments":json!({"files":[{"path":"test.rs","before":"old","after":"new"}]}).to_string(),"status":"completed"});
    let server = Server::start(vec![
        gpt(&[message()]),
        gpt(&[call]),
        gpt(&[message()]),
        gpt(&[message()]),
        gpt(&[message()]),
        gpt(&[message()]),
    ])
    .await;
    let store = MemoryStore::default();
    let kit = on_provider(
        without_show_diff(&server, &env, &store),
        subscribed(&server),
    );
    let mut session = Session::start(kit.build(on(MODEL, kit::Start::Fresh)).await.unwrap().agent);
    session.turn("u1", "one").await;
    assert_eq!(
        answered(&session.tools(1, every_default()).await),
        (When::NextTurn, Some(CacheEffect::Kept))
    );
    let second = session.turn("u2", "two").await;
    assert_eq!(
        changed(&second),
        [(
            vec!["show_diff".into()],
            vec![],
            ToolChangeVia::Inline,
            Some(CacheEffect::Kept)
        )]
    );
    assert!(changed(&session.turn("u3", "unchanged").await).is_empty());
    assert_eq!(
        answered(&session.tools(2, without("show_diff")).await),
        (When::NextTurn, Some(CacheEffect::Kept))
    );
    session.turn("u4", "off").await;
    session.tools(3, every_default()).await;
    session.turn("u5", "on").await;
    let agent = session.end().await;
    assert!(agent.history().iter().flat_map(|m| &m.content).any(|b| matches!(b, ContentBlock::ToolResult { tool_use_id, is_error: false, .. } if tool_use_id == "call_show")));
    let added = body(&server, 1);
    let input = added["input"].as_array().unwrap();
    let position = input
        .iter()
        .position(|item| item["type"] == "additional_tools")
        .unwrap();
    assert_eq!(input[position - 1]["role"], "user");
    assert_eq!(additions(&added)[0]["tools"][0]["name"], "show_diff");
    let off = body(&server, 4);
    assert!(
        off["input"]
            .to_string()
            .contains("Tools turned off: show_diff.")
    );
    assert_eq!(
        additions(&body(&server, 5)).len(),
        2,
        "an item-declared tool is turned back on by value"
    );
    stable(&server, 0..6);
    rebuilds(&store, &agent).await;
}

#[tokio::test]
async fn a_resume_keeps_the_logged_prefix_and_one_addition() {
    let env = Env::new("openai-inline-resume");
    let server = Server::start(vec![
        gpt(&[message()]),
        gpt(&[message()]),
        gpt(&[message()]),
    ])
    .await;
    let store = MemoryStore::default();
    let kit = on_provider(
        without_show_diff(&server, &env, &store),
        subscribed(&server),
    );
    let mut session = Session::start(kit.build(on(MODEL, kit::Start::Fresh)).await.unwrap().agent);
    session.turn("u1", "one").await;
    session.tools(1, every_default()).await;
    session.turn("u2", "two").await;
    session.end().await;
    let resumed_kit = on_provider(switching(&server, &env, &store), subscribed(&server));
    let mut session = Session::start(resumed(&resumed_kit, &store, &server, MODEL).await);
    assert!(changed(&session.turn("u3", "three").await).is_empty());
    let agent = session.end().await;
    assert_eq!(additions(&body(&server, 2)).len(), 1);
    stable(&server, 0..3);
    rebuilds(&store, &agent).await;
}

#[tokio::test]
async fn a_rewind_before_the_addition_restates_it() {
    let env = Env::new("openai-inline-rewind");
    let server = Server::start(vec![
        gpt(&[message()]),
        gpt(&[message()]),
        gpt(&[message()]),
    ])
    .await;
    let store = MemoryStore::default();
    let kit = on_provider(
        without_show_diff(&server, &env, &store),
        subscribed(&server),
    );
    let mut session = Session::start(kit.build(on(MODEL, kit::Start::Fresh)).await.unwrap().agent);
    session.turn("u1", "one").await;
    session.tools(1, every_default()).await;
    session.turn("u2", "two").await;
    session
        .send(ClientCommand::Rewind {
            to: wisp_protocol::RewindPoint::Turn { turn: 2 },
            request: RequestId(2),
        })
        .await;
    session
        .until(|e| {
            matches!(e, AgentEvent::Rewound { .. })
                || matches!(e, AgentEvent::Rejected { request: Some(r), .. } if *r == RequestId(2))
        })
        .await;
    assert!(changed(&session.turn("u3", "two again").await).is_empty());
    let agent = session.end().await;
    assert_eq!(additions(&body(&server, 2)), additions(&body(&server, 1)));
    stable(&server, 0..2);
    assert!(
        body(&server, 2)["input"]
            .as_array()
            .unwrap()
            .starts_with(body(&server, 0)["input"].as_array().unwrap())
    );
    rebuilds(&store, &agent).await;
}

#[tokio::test]
async fn compaction_uses_the_logged_input_and_restates_the_addition_once() {
    let env = Env::new("openai-inline-compact");
    let mut summary = message();
    summary["content"][0]["text"] = SUMMARY.into();
    let server = Server::start(vec![
        gpt(&[message()]),
        gpt(&[message()]),
        gpt(&[summary]),
        gpt(&[message()]),
        gpt(&[message()]),
    ])
    .await;
    let store = MemoryStore::default();
    let kit = on_provider(
        without_show_diff(&server, &env, &store),
        subscribed(&server),
    );
    let mut session = Session::start(kit.build(on(MODEL, kit::Start::Fresh)).await.unwrap().agent);
    session.turn("u1", "one").await;
    session.tools(1, every_default()).await;
    session.turn("u2", "two").await;
    session.compact(2).await;
    assert!(changed(&session.turn("u3", "three").await).is_empty());
    assert!(changed(&session.turn("u4", "four").await).is_empty());
    let agent = session.end().await;
    assert_eq!(additions(&body(&server, 3)).len(), 1);
    assert_eq!(additions(&body(&server, 4)).len(), 1);
    stable(&server, 0..3);
    stable(&server, 3..5);
    rebuilds(&store, &agent).await;
}

#[tokio::test]
async fn a_refused_addition_falls_back_and_the_next_opening_rebuilds_the_array() {
    let env = Env::new("openai-inline-refused");
    let refused = Reply {
        response: http::json(
            "400 Bad Request",
            r#"{"error":{"type":"invalid_request_error","message":"additional_tools is not supported"}}"#,
        ),
        before: Vec::new(),
        gate: None,
    };
    let server = Server::start(vec![
        gpt(&[message()]),
        refused,
        gpt(&[message()]),
        gpt(&[message()]),
    ])
    .await;
    let store = MemoryStore::default();
    let kit = on_provider(
        without_show_diff(&server, &env, &store),
        subscribed(&server),
    );
    let mut session = Session::start(kit.build(on(MODEL, kit::Start::Fresh)).await.unwrap().agent);
    session.turn("u1", "one").await;
    session.tools(1, every_default()).await;
    let second = session.turn("u2", "two").await;
    assert!(second.iter().any(|e| matches!(
        e,
        AgentEvent::FeatureStateChanged {
            feature: wisp_protocol::Feature::InlineTools,
            state: wisp_protocol::log::FeatureState::FallenBack,
            ..
        }
    )));
    session.turn("u3", "three").await;
    let agent = session.end().await;
    assert_eq!(additions(&body(&server, 1)).len(), 1);
    assert!(additions(&body(&server, 2)).is_empty());
    let later = body(&server, 3);
    assert!(additions(&later).is_empty());
    assert!(
        later["tools"]
            .as_array()
            .unwrap()
            .iter()
            .any(|t| t["name"] == "show_diff")
    );
    assert!(log_events(&store).iter().any(|event| matches!(
        event,
        LogEvent::InlineTools {
            state: wisp_protocol::log::FeatureState::FallenBack,
            ..
        }
    )));
    stable(&server, 0..2);
    rebuilds(&store, &agent).await;
}
