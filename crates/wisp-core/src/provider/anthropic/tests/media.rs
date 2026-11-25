use std::fs;
use std::path::PathBuf;

use serde_json::{Value, json};
use wisp_protocol::{CacheTtl, ContentBlock, ImageType, Message, Mime, Role, SessionConfig};

use super::{api_key, session};
use crate::agent::testing::text_of;
use crate::provider::anthropic::media::{Carried, MEDIA_TYPES, carried};
use crate::provider::anthropic::{AnthropicProvider, build_body};
use crate::provider::{Features, Provider, Request, UNRENDERED};
use crate::test_guard::RED_PIXEL as PIXEL;
use crate::tool::snap::mime;

const PDF: &str = "JVBERi0xLjQK";

fn request<'a>(session: &'a SessionConfig, messages: &'a [Message]) -> Request<'a> {
    Request {
        session,
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
    }
}

fn encoded(messages: &[Message]) -> Result<String, String> {
    let session = session(None, CacheTtl::OneHour);
    build_body(&request(&session, messages))
        .map(|bytes| String::from_utf8(bytes).unwrap())
        .map_err(|e| e.message)
}

fn snap_exchange(results: Vec<ContentBlock>) -> Vec<Message> {
    let calls = results
        .iter()
        .filter_map(|block| match block {
            ContentBlock::ToolResult { tool_use_id, .. } => Some(ContentBlock::ToolUse {
                id: tool_use_id.clone(),
                name: "snap".into(),
                input: json!({}),
            }),
            _ => None,
        })
        .collect();
    vec![
        Message::user_text("take a snap"),
        Message {
            role: Role::Assistant,
            content: calls,
        },
        Message {
            role: Role::User,
            content: results,
        },
    ]
}

fn result(id: &str, text: &str, attached: ContentBlock) -> ContentBlock {
    ContentBlock::ToolResult {
        tool_use_id: id.into(),
        content: vec![ContentBlock::Text { text: text.into() }, attached],
        is_error: false,
    }
}

#[test]
fn the_listed_media_types_are_sorted_and_each_has_a_block() {
    let mut sorted = MEDIA_TYPES.clone();
    sorted.sort_unstable();
    sorted.dedup();
    assert_eq!(sorted, *MEDIA_TYPES);
    for media_type in MEDIA_TYPES.iter() {
        assert!(media_type.parse::<Mime>().is_ok(), "{media_type}");
    }
    assert_eq!(carried("application/pdf"), Some(Carried::Document));
    assert_eq!(carried("image/webp"), Some(Carried::Image));
    assert_eq!(carried("application/zip"), None);
    let provider = AnthropicProvider::new(&api_key("k"), "http://127.0.0.1:9").unwrap();
    assert_eq!(provider.media_types(), *MEDIA_TYPES);
}

#[test]
fn an_image_and_a_media_block_of_the_same_data_send_the_same_bytes() {
    let image = snap_exchange(vec![result(
        "toolu_1",
        "snap.png",
        ContentBlock::image(ImageType::Png, PIXEL),
    )]);
    let media = snap_exchange(vec![result(
        "toolu_1",
        "snap.png",
        ContentBlock::media(mime("image/png"), PIXEL),
    )]);
    assert_eq!(encoded(&image).unwrap(), encoded(&media).unwrap());
    let pasted = |block: ContentBlock| {
        vec![Message {
            role: Role::User,
            content: vec![
                block,
                ContentBlock::Text {
                    text: "and?".into(),
                },
            ],
        }]
    };
    assert_eq!(
        encoded(&pasted(ContentBlock::image(ImageType::Png, PIXEL))).unwrap(),
        encoded(&pasted(ContentBlock::media(mime("image/png"), PIXEL))).unwrap()
    );
}

#[test]
fn a_tool_result_sends_its_text_then_each_file_in_order() {
    let messages = snap_exchange(vec![
        result(
            "toolu_1",
            "snap.png: image/png",
            ContentBlock::media(mime("image/png"), PIXEL),
        ),
        result(
            "toolu_2",
            "snap.pdf: application/pdf",
            ContentBlock::media(mime("application/pdf"), PDF),
        ),
    ]);
    let session = SessionConfig {
        model: "claude-sonnet-5-5".into(),
        system: Some("You are Wisp.".into()),
        effort: wisp_protocol::Effort::Low,
        cache_ttl: CacheTtl::OneHour,
        max_tokens: 1024,
        message_time: wisp_protocol::MessageTime::Utc,
        instructions: None,
        context_note: true,
    };
    let body = text_of(&request(&session, &messages)) + "\n";
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures/requests/tool_result_media.json");
    if std::env::var_os("WISP_BLESS").is_some() {
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(&path, &body).unwrap();
    }
    let stored = fs::read_to_string(&path).unwrap();
    assert_eq!(stored, body);
    let wire: Value = serde_json::from_str(&body).unwrap();
    let results = &wire["messages"][2]["content"];
    assert_eq!(
        results[0]["content"],
        json!([
            {"type": "text", "text": "snap.png: image/png"},
            {"type": "image", "source": {"type": "base64", "media_type": "image/png", "data": PIXEL}}
        ])
    );
    assert_eq!(
        results[1]["content"],
        json!([
            {"type": "text", "text": "snap.pdf: application/pdf"},
            {"type": "document", "source": {"type": "base64", "media_type": "application/pdf", "data": PDF}}
        ])
    );
}

#[test]
fn a_type_with_no_block_goes_as_the_not_sent_note() {
    use wisp_protocol::FileSlice;

    let zip = ContentBlock::media(mime("application/zip"), "UEsDBA==");
    let mut messages = snap_exchange(vec![result("toolu_1", "a.zip", zip.clone())]);
    messages.push(Message {
        role: Role::User,
        content: vec![zip],
    });
    let wire: Value = serde_json::from_str(&encoded(&messages).unwrap()).unwrap();
    let note = json!({"type": "text", "text": "[application/zip file not sent: this server can't carry it]"});
    assert_eq!(wire["messages"][2]["content"][0]["content"][1], note);
    assert_eq!(wire["messages"][3]["content"][0], note);
    for (kind, slice, label) in [
        (
            "application/pdf",
            FileSlice::pages(14, 16).unwrap(),
            "pages 14-16",
        ),
        (
            "audio/wav",
            FileSlice::seconds(30, 90).unwrap(),
            "seconds 30-90",
        ),
    ] {
        let block = ContentBlock::media_sliced(mime(kind), "YQ==", Some(slice));
        let mut messages = snap_exchange(vec![result("toolu_1", "attached", block.clone())]);
        messages.push(Message {
            role: Role::User,
            content: vec![block],
        });
        let body = encoded(&messages).unwrap();
        let wire: Value = serde_json::from_str(&body).unwrap();
        let note = json!({"type":"text","text":format!("[{kind} file ({label}) not sent: this model can't read the selected part]")});
        assert_eq!(wire["messages"][2]["content"][0]["content"][1], note);
        assert_eq!(wire["messages"][3]["content"][0], note);
        assert!(!body.contains("YQ=="));
    }
}
