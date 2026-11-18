use serde_json::{Value, json};
use wisp_protocol::{
    CacheTtl, CitedSource, ContentBlock, Message, SearchFailure, SearchHit, SearchOutcome,
};

use super::super::search::{
    SearchCall, SearchResult, cited_sources, failure_of, search_call, search_result,
};
use super::{body, run_fixture, session};
use crate::test_guard::{http, opaque};

const SEARCH_TURN: &str = r#"event: message_start
data: {"type":"message_start","message":{"id":"msg_s","model":"claude-haiku-5-5","usage":{"input_tokens":12,"output_tokens":1}}}

event: content_block_start
data: {"type":"content_block_start","index":0,"content_block":{"type":"server_tool_use","id":"srvtoolu_01","name":"web_search","input":{}}}

event: content_block_delta
data: {"type":"content_block_delta","index":0,"delta":{"type":"input_json_delta","partial_json":"{\"query\": \"rust rip"}}

event: content_block_delta
data: {"type":"content_block_delta","index":0,"delta":{"type":"input_json_delta","partial_json":"grep crates\"}"}}

event: content_block_stop
data: {"type":"content_block_stop","index":0}

event: content_block_start
data: {"type":"content_block_start","index":1,"content_block":{"type":"web_search_tool_result","tool_use_id":"srvtoolu_01","content":[{"type":"web_search_result","title":"ripgrep - crates.io","url":"https://crates.io/crates/ripgrep","encrypted_content":"EqgfCioIARgB","page_age":"April 30, 2026"},{"type":"web_search_result","title":"BurntSushi/ripgrep","url":"https://github.com/BurntSushi/ripgrep","encrypted_content":"Eo8BCioIARgC"}]}}

event: content_block_stop
data: {"type":"content_block_stop","index":1}

event: content_block_start
data: {"type":"content_block_start","index":2,"content_block":{"type":"text","text":""}}

event: content_block_delta
data: {"type":"content_block_delta","index":2,"delta":{"type":"citations_delta","citation":{"type":"web_search_result_location","cited_text":"ripgrep recursively searches","url":"https://crates.io/crates/ripgrep","title":"ripgrep - crates.io","encrypted_index":"EpMBCioIARgD"}}}

event: content_block_delta
data: {"type":"content_block_delta","index":2,"delta":{"type":"text_delta","text":"ripgrep is a fast search tool."}}

event: content_block_delta
data: {"type":"content_block_delta","index":2,"delta":{"type":"citations_delta","citation":{"type":"web_search_result_location","cited_text":"line-oriented search","url":"https://github.com/BurntSushi/ripgrep","title":"BurntSushi/ripgrep","encrypted_index":"EpMBCioIARgE"}}}

event: content_block_delta
data: {"type":"content_block_delta","index":2,"delta":{"type":"citations_delta","citation":{"type":"web_search_result_location","cited_text":"ripgrep recursively searches","url":"https://crates.io/crates/ripgrep","title":"ripgrep - crates.io","encrypted_index":"EpMBCioIARgF"}}}

event: content_block_stop
data: {"type":"content_block_stop","index":2}

event: message_delta
data: {"type":"message_delta","delta":{"stop_reason":"end_turn"},"usage":{"output_tokens":40,"cache_creation":{"ephemeral_5m_input_tokens":2100,"ephemeral_1h_input_tokens":0},"server_tool_use":{"web_search_requests":1}}}

event: message_stop
data: {"type":"message_stop"}

"#;

fn citation(cited: &str, url: &str, title: &str, index: &str) -> Value {
    json!({
        "type": "web_search_result_location",
        "cited_text": cited,
        "url": url,
        "title": title,
        "encrypted_index": index,
    })
}

fn result_block(content: Value) -> ContentBlock {
    opaque(
        json!({"type": "web_search_tool_result", "tool_use_id": "srvtoolu_9", "content": content}),
    )
}

fn outcome(content: Value) -> SearchOutcome {
    search_result(&result_block(content)).unwrap().outcome
}

fn failed(failure: SearchFailure, code: Option<&str>) -> SearchOutcome {
    SearchOutcome::Failed {
        failure,
        code: code.map(str::to_owned),
    }
}

#[test]
fn a_search_turn_keeps_every_block_and_echoes_it_byte_for_byte() {
    let msg = run_fixture(SEARCH_TURN).1.unwrap();
    assert_eq!(msg.usage.web_search_requests, 1);
    assert_eq!(msg.usage.cache_creation_5m_input_tokens, Some(2100));
    let call = json!({
        "type": "server_tool_use",
        "id": "srvtoolu_01",
        "name": "web_search",
        "input": {"query": "rust ripgrep crates"},
    });
    let result = json!({
        "type": "web_search_tool_result",
        "tool_use_id": "srvtoolu_01",
        "content": [
            {"type": "web_search_result", "title": "ripgrep - crates.io", "url": "https://crates.io/crates/ripgrep", "encrypted_content": "EqgfCioIARgB", "page_age": "April 30, 2026"},
            {"type": "web_search_result", "title": "BurntSushi/ripgrep", "url": "https://github.com/BurntSushi/ripgrep", "encrypted_content": "Eo8BCioIARgC"},
        ],
    });
    let text = json!({
        "type": "text",
        "text": "ripgrep is a fast search tool.",
        "citations": [
            citation("ripgrep recursively searches", "https://crates.io/crates/ripgrep", "ripgrep - crates.io", "EpMBCioIARgD"),
            citation("line-oriented search", "https://github.com/BurntSushi/ripgrep", "BurntSushi/ripgrep", "EpMBCioIARgE"),
            citation("ripgrep recursively searches", "https://crates.io/crates/ripgrep", "ripgrep - crates.io", "EpMBCioIARgF"),
        ],
    });
    assert_eq!(
        msg.content,
        vec![
            opaque(call.clone()),
            opaque(result.clone()),
            opaque(text.clone())
        ]
    );

    let logged = serde_json::to_string(&msg.content).unwrap();
    let back: Vec<ContentBlock> = serde_json::from_str(&logged).unwrap();
    assert_eq!(back, msg.content);
    let session = session(None, CacheTtl::OneHour);
    let messages = [
        Message::user_text("search"),
        Message {
            role: wisp_protocol::Role::Assistant,
            content: back,
        },
        Message::user_text("thanks"),
    ];
    let sent: Value = serde_json::from_str(&body(&session, &messages, None)).unwrap();
    assert_eq!(sent["messages"][1]["content"], json!([call, result, text]));

    assert_eq!(
        search_call(&msg.content[0]),
        Some(SearchCall {
            id: "srvtoolu_01".into(),
            query: "rust ripgrep crates".into(),
        })
    );
    assert_eq!(
        search_result(&msg.content[1]),
        Some(SearchResult {
            call_id: "srvtoolu_01".into(),
            outcome: SearchOutcome::Results {
                hits: vec![
                    SearchHit {
                        title: "ripgrep - crates.io".into(),
                        url: "https://crates.io/crates/ripgrep".into(),
                        snippet: None,
                        page_age: Some("April 30, 2026".into()),
                    },
                    SearchHit {
                        title: "BurntSushi/ripgrep".into(),
                        url: "https://github.com/BurntSushi/ripgrep".into(),
                        snippet: None,
                        page_age: None,
                    },
                ],
            },
        })
    );
    assert_eq!(
        cited_sources(&msg.content[2]),
        vec![
            CitedSource {
                url: "https://crates.io/crates/ripgrep".into(),
                title: Some("ripgrep - crates.io".into()),
            },
            CitedSource {
                url: "https://github.com/BurntSushi/ripgrep".into(),
                title: Some("BurntSushi/ripgrep".into()),
            },
        ]
    );
    assert_eq!(search_call(&msg.content[1]), None);
    assert_eq!(search_result(&msg.content[0]), None);
    assert!(cited_sources(&msg.content[0]).is_empty());
}

#[test]
fn every_documented_error_code_maps_to_its_kind_and_keeps_the_code() {
    let cases = [
        ("too_many_requests", SearchFailure::RateLimited),
        ("invalid_tool_input", SearchFailure::InvalidQuery),
        ("query_too_long", SearchFailure::InvalidQuery),
        ("max_uses_exceeded", SearchFailure::Failed),
        ("request_too_large", SearchFailure::Failed),
        ("unavailable", SearchFailure::Unavailable),
    ];
    for (code, failure) in cases {
        assert_eq!(failure_of(code), failure, "{code}");
        assert_eq!(
            outcome(json!({"type": "web_search_tool_result_error", "error_code": code})),
            failed(failure, Some(code)),
            "{code}"
        );
    }
}

#[test]
fn an_undocumented_code_fails_and_is_kept_only_when_bounded() {
    let error = |code: &str| json!({"type": "web_search_tool_result_error", "error_code": code});
    assert_eq!(
        outcome(error("quota_exhausted_2")),
        failed(SearchFailure::Failed, Some("quota_exhausted_2"))
    );
    let longest = "a".repeat(64);
    assert_eq!(
        outcome(error(&longest)),
        failed(SearchFailure::Failed, Some(&longest))
    );
    for unbounded in [
        "a".repeat(65),
        "Quota".to_owned(),
        "has space".to_owned(),
        "dash-ed".to_owned(),
        "\u{1b}[2J".to_owned(),
        String::new(),
    ] {
        assert_eq!(
            outcome(error(&unbounded)),
            failed(SearchFailure::Failed, None),
            "{unbounded:?}"
        );
    }
}

fn streamed(wire: &Value) -> ContentBlock {
    let start = json!({"type": "content_block_start", "index": 0, "content_block": wire});
    let sse = [
        http::event("message_start", r#"{"type":"message_start","message":{"id":"m","model":"claude-haiku-5-5","usage":{"input_tokens":1,"output_tokens":1}}}"#),
        http::event("content_block_start", start),
        http::event("content_block_stop", r#"{"type":"content_block_stop","index":0}"#),
        http::event("message_delta", r#"{"type":"message_delta","delta":{"stop_reason":"end_turn"},"usage":{"output_tokens":2}}"#),
        http::event("message_stop", r#"{"type":"message_stop"}"#),
    ]
    .concat();
    let msg = run_fixture(&sse).1.unwrap();
    let [block] = <[ContentBlock; 1]>::try_from(msg.content).unwrap();
    let logged: ContentBlock =
        serde_json::from_str(&serde_json::to_string(&block).unwrap()).unwrap();
    let session = session(None, CacheTtl::OneHour);
    let messages = [
        Message::user_text("search"),
        Message {
            role: wisp_protocol::Role::Assistant,
            content: vec![logged.clone()],
        },
        Message::user_text("next"),
    ];
    let sent: Value = serde_json::from_str(&body(&session, &messages, None)).unwrap();
    assert_eq!(block, logged);
    assert_eq!(&sent["messages"][1]["content"], &json!([wire]));
    logged
}

fn block_raw(block: &ContentBlock) -> &Value {
    match block {
        ContentBlock::Opaque {
            provider,
            raw,
            deltas,
        } => {
            assert_eq!(provider, "anthropic");
            assert!(deltas.is_empty());
            raw
        }
        other => panic!("not opaque: {other:?}"),
    }
}

#[test]
fn unknown_shapes_fail_without_guessing_and_round_trip_unchanged() {
    let unknown = failed(SearchFailure::Failed, None);
    let shapes = [
        json!(null),
        json!("text"),
        json!({"type": "something_new", "extra": [1, 2]}),
        json!({"type": "web_search_tool_result_error"}),
        json!([{"type": "web_search_result", "title": "no url"}]),
        json!([
            {"type": "web_search_result", "title": "ok", "url": "https://a.example"},
            {"type": "web_fetch_result", "url": "https://b.example"}
        ]),
        json!([{"type": "web_search_result", "url": 7}]),
    ];
    for content in shapes {
        let sent = json!({"type": "web_search_tool_result", "tool_use_id": "srvtoolu_9", "content": content});
        let block = streamed(&sent);
        assert_eq!(block_raw(&block), &sent);
        assert_eq!(search_result(&block).unwrap().outcome, unknown, "{content}");
    }
    let missing = json!({"type": "web_search_tool_result", "tool_use_id": "srvtoolu_9"});
    let block = streamed(&missing);
    assert_eq!(block_raw(&block), &missing);
    assert_eq!(search_result(&block).unwrap().outcome, unknown);
}

#[test]
fn a_result_without_a_call_id_pairs_with_nothing_and_round_trips_unchanged() {
    let hits = json!([{"type": "web_search_result", "title": "t", "url": "https://a.example", "encrypted_content": "Eq"}]);
    for sent in [
        json!({"type": "web_search_tool_result", "content": hits}),
        json!({"type": "web_search_tool_result", "tool_use_id": "", "content": hits}),
        json!({"type": "web_search_tool_result", "tool_use_id": 7, "content": hits}),
    ] {
        let block = streamed(&sent);
        assert_eq!(block_raw(&block), &sent);
        assert_eq!(search_result(&block), None, "{sent}");
    }
}

#[test]
fn an_empty_list_is_a_search_without_results() {
    assert_eq!(
        outcome(json!([])),
        SearchOutcome::Results { hits: Vec::new() }
    );
}

#[test]
fn a_hit_without_a_title_keeps_its_url() {
    assert_eq!(
        outcome(json!([{"type": "web_search_result", "url": "https://a.example"}])),
        SearchOutcome::Results {
            hits: vec![SearchHit {
                title: String::new(),
                url: "https://a.example".into(),
                snippet: None,
                page_age: None,
            }],
        }
    );
}

#[test]
fn only_web_search_calls_and_web_citations_are_read() {
    let other_tool = opaque(
        json!({"type": "server_tool_use", "id": "s", "name": "web_fetch", "input": {"url": "x"}}),
    );
    assert_eq!(search_call(&other_tool), None);
    let foreign = ContentBlock::opaque(
        "other",
        json!({"type": "server_tool_use", "id": "s", "name": "web_search", "input": {"query": "q"}}),
    );
    assert_eq!(search_call(&foreign), None);
    let char_cited = opaque(json!({
        "type": "text",
        "text": "Paris",
        "citations": [{"type": "char_location", "cited_text": "Paris"}],
    }));
    assert!(cited_sources(&char_cited).is_empty());
    let no_query =
        opaque(json!({"type": "server_tool_use", "id": "s", "name": "web_search", "input": {}}));
    assert_eq!(
        search_call(&no_query),
        Some(SearchCall {
            id: "s".into(),
            query: String::new(),
        })
    );
}
