use serde_json::{Value, json};
use wisp_tool_text::search::{QUERY, REPORT, schema};

use crate::search::WEB_SEARCH;

pub(super) const CALL: &str = "search_call";
pub(super) const RESULT: &str = "search_result";

pub(super) fn declaration() -> Value {
    json!({
        "name": WEB_SEARCH,
        "description": format!("Search the web with Google Search. {REPORT}"),
        "parametersJsonSchema": serde_json::from_str::<Value>(&schema(QUERY)).unwrap_or_default(),
    })
}

pub(super) fn query_of(item: &Value) -> &str {
    item.pointer("/part/functionCall/args/query")
        .and_then(Value::as_str)
        .unwrap_or_default()
}

#[cfg(test)]
pub(super) fn call_item(call_id: &str, part: &Value) -> Value {
    json!({"type": CALL, "call_id": call_id, "part": part})
}

pub(super) fn response_part(item: &Value) -> Option<Value> {
    (item["type"] == RESULT).then(|| {
        let mut response = json!({
            "name": item["name"],
            "response": {"output": item["output"], "is_error": item["is_error"]},
        });
        if !item["id"].is_null() {
            response["id"] = item["id"].clone();
        }
        json!({"functionResponse": response})
    })
}

pub(in crate::provider) fn as_text(item: &Value) -> Option<String> {
    match item["type"].as_str()? {
        CALL => Some(crate::search::call_line(query_of(item))),
        RESULT => item["output"].as_str().map(str::to_owned),
        _ => None,
    }
}
