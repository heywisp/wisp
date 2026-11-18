use std::sync::Arc;

use serde_json::{Value, json};

use super::{
    USAGE_HEADERS, bodies, completed, created, data, message, request, serving, session,
    stream_reply,
};
use crate::agent::Agent;
use crate::agent::testing::{stream_error, turn, withdrawn};
use crate::provider::openai::SUBSCRIPTION_MEDIA_TYPES;
use crate::provider::{Refused, Rendering};
use crate::test_guard::http;
use crate::tool::Toolbox;
use crate::workspace::Workspace;
use wisp_tool_text::files::uncarried;

const REFUSED: &str = r#"{"error":{"type":"invalid_request_error","message":"Invalid image"}}"#;
const NOTE: &str = "[shot.png (image/png) couldn't be processed: OpenAI request refused (HTTP 400): \
invalid_request_error. It can't be read in this session.]";
const SHEET: &[u8] =
    b"PK\x03\x04\0\0\0\0\0\0\0\0\0\0\0\0\0\0\0\0\0\0\0\0\0\0\0\0\0\0xl/workbook.xml";
const BINARY_PDF: &[u8] = b"%PDF-1.7\n%\xe2\xe3\xcf\xd3\n1 0 obj << /Type /Catalog >> endobj\n";

async fn viewing(
    replies: Vec<http::Reply>,
    subscription: bool,
) -> (Agent, http::Server, tempfile::TempDir) {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("shot.png"), wisp_plugin_testkit::png()).unwrap();
    std::fs::write(dir.path().join("spec.pdf"), BINARY_PDF).unwrap();
    std::fs::write(dir.path().join("sheet.xlsx"), SHEET).unwrap();
    let (mut provider, server) = serving(replies).await;
    provider.subscription = subscription;
    let workspace = Workspace {
        root: dir.path().to_path_buf(),
        home: None,
        temp: dir.path().to_path_buf(),
    };
    let agent = Agent::new(Arc::new(provider), session()).with_tools(
        Toolbox::new([crate::tool::testkit::view_file_without_bash()]),
        workspace,
    );
    (agent, server, dir)
}

fn ok(values: &[Value]) -> http::Reply {
    stream_reply("200 OK", &USAGE_HEADERS, &data(values))
}

fn extends(earlier: &Value, later: &Value) -> bool {
    let (Some(before), Some(after)) = (earlier["input"].as_array(), later["input"].as_array())
    else {
        return false;
    };
    earlier["prompt_cache_key"] == later["prompt_cache_key"] && after.starts_with(before)
}

fn view(call: &str, path: &str) -> Value {
    json!({"type":"function_call","id":format!("fc_{call}"),"call_id":call,"name":"view_file","arguments":json!({"path":path}).to_string(),"status":"completed"})
}

fn output<'a>(body: &'a Value, call: &str) -> &'a Value {
    body["input"]
        .as_array()
        .unwrap()
        .iter()
        .find(|item| item["type"] == "function_call_output" && item["call_id"] == call)
        .map(|item| &item["output"])
        .unwrap()
}

#[tokio::test]
async fn viewed_files_reach_openai_as_each_connection_carries_them() {
    for subscription in [true, false] {
        let calls = [
            view("call_png", "shot.png"),
            view("call_pdf", "spec.pdf"),
            view("call_xlsx", "sheet.xlsx"),
        ];
        let reply = || ok(&[created(), completed(&[message()])]);
        let replies = vec![ok(&[created(), completed(&calls)]), reply(), reply()];
        let (mut agent, server, _dir) = viewing(replies, subscription).await;
        let (outcome, _) = turn(&mut agent, "look at all three", &[]).await;
        crate::agent::testing::completed(&outcome);
        let (outcome, _) = turn(&mut agent, "and now?", &[]).await;
        crate::agent::testing::completed(&outcome);

        let sent = bodies(&server);
        assert_eq!(sent.len(), 3);
        let png = output(&sent[1], "call_png");
        assert_eq!(png[0]["type"], "input_text");
        assert!(
            png[0]["text"].as_str().unwrap().contains("attached as is"),
            "{png}"
        );
        let url = format!(
            "data:image/png;base64,{}",
            wisp_media::encode_base64(&wisp_plugin_testkit::png())
        );
        assert_eq!(png[1], json!({"type":"input_image","image_url":url}));
        assert_eq!(png.as_array().unwrap().len(), 2);
        let pdf = output(&sent[1], "call_pdf");
        let data = wisp_media::encode_base64(BINARY_PDF);
        let file = json!({"type":"input_file","filename":"file.pdf",
            "file_data":format!("data:application/pdf;base64,{data}")});
        assert_eq!(pdf[1], file, "{pdf}");
        assert_eq!(pdf.as_array().unwrap().len(), 2);
        let xlsx = output(&sent[1], "call_xlsx");
        let sheet = "application/vnd.openxmlformats-officedocument.spreadsheetml.sheet";
        if subscription {
            let carried = SUBSCRIPTION_MEDIA_TYPES.as_slice();
            assert_eq!(xlsx, &json!(uncarried("sheet.xlsx", sheet, carried, None)));
        } else {
            let data = wisp_media::encode_base64(SHEET);
            let file = json!({"type":"input_file","filename":"file.xlsx",
                "file_data":format!("data:{sheet};base64,{data}")});
            assert_eq!(xlsx[1], file, "{xlsx}");
        }
        assert!(extends(&sent[0], &sent[1]));
        assert!(extends(&sent[1], &sent[2]));
        for call in ["call_png", "call_pdf", "call_xlsx"] {
            assert_eq!(output(&sent[2], call), output(&sent[1], call));
        }
    }
}

#[tokio::test]
async fn the_openai_adapter_marks_only_refusals_with_provisional_media() {
    let failed =
        json!({"type":"response.failed","response":{"error":{"type":"invalid_request_error"}}});
    let cases = [
        ("400 Bad Request", REFUSED.to_owned(), true),
        (
            "400 Bad Request",
            json!({"detail":"Invalid image"}).to_string(),
            true,
        ),
        (
            "429 Too Many Requests",
            json!({"error":{"type":"rate_limit_error"}}).to_string(),
            false,
        ),
        (
            "200 OK",
            data(&[
                json!({"type":"codex.rate_limits"}),
                json!({"type":"error","error":{"type":"invalid_request_error"}}),
            ]),
            true,
        ),
        (
            "200 OK",
            data(&[json!({"error":{"type":"invalid_request_error","message":"Invalid image"}})]),
            true,
        ),
        ("200 OK", data(&[created(), failed]), false),
        (
            "200 OK",
            data(&[json!({"type":"error","code":"invalid_image","message":"Invalid image"})]),
            false,
        ),
    ];
    let session = session();
    for (status, body, marked) in cases {
        for provisional in [true, false] {
            let (provider, _server) = serving(vec![stream_reply(status, &[], &body)]).await;
            let rendering = Rendering {
                provisional,
                ..Rendering::default()
            };
            let error = stream_error(&provider, &request(&session, &[], &[], &rendering)).await;
            assert_eq!(
                error.refused == Some(Refused::Attachment),
                marked && provisional,
                "{status} {body} {provisional} {error:?}"
            );
        }
    }
}

#[tokio::test]
async fn an_image_openai_refuses_is_withdrawn_and_the_chat_goes_on() {
    let reply = || ok(&[created(), completed(&[message()])]);
    let (mut agent, server, _dir) = viewing(
        vec![
            ok(&[created(), completed(&[view("call_png", "shot.png")])]),
            stream_reply("400 Bad Request", &USAGE_HEADERS, REFUSED),
            reply(),
            reply(),
        ],
        true,
    )
    .await;
    let (outcome, events) = turn(&mut agent, "look at shot.png", &[]).await;
    crate::agent::testing::completed(&outcome);
    let items = withdrawn(&events);
    assert_eq!(items.len(), 1, "{items:#?}");
    assert_eq!(items[0].label, "shot.png");
    assert_eq!(items[0].note, NOTE);
    let (outcome, _) = turn(&mut agent, "and now?", &[]).await;
    crate::agent::testing::completed(&outcome);

    let sent = bodies(&server);
    assert_eq!(sent.len(), 4);
    let text = output(&sent[1], "call_png")[0]["text"].as_str().unwrap();
    let mut resent = sent[1].clone();
    for item in resent["input"].as_array_mut().unwrap() {
        if item["call_id"] == "call_png" && item["type"] == "function_call_output" {
            item["output"] = json!(format!("{text}\n{NOTE}"));
        }
    }
    assert_eq!(sent[2], resent);
    assert!(extends(&sent[0], &sent[1]));
    assert!(extends(&sent[2], &sent[3]));
    assert!(!extends(&sent[1], &sent[3]));
    assert!(!sent[3].to_string().contains("input_image"));
    assert!(
        serde_json::to_string(&*agent.history())
            .unwrap()
            .contains(NOTE)
    );
}
