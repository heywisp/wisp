use serde_json::Value;
use wisp_protocol::ContentBlock;

pub(super) fn text(block: &ContentBlock, target: &str) -> Option<String> {
    let ContentBlock::Opaque {
        provider,
        raw,
        deltas,
    } = block
    else {
        return None;
    };
    if provider == target || private_reasoning(raw) {
        return None;
    }
    let mut content = raw.clone();
    if let Some(envelope) = super::native_envelope(block) {
        content = envelope.item?.clone();
        if private_reasoning(&content) {
            return None;
        }
        if content.get("type").and_then(Value::as_str) == Some(super::openai::search::CALL) {
            return super::openai::search::subject(&content).map(|q| crate::search::call_line(&q));
        }
        if let Some(text) = super::gemini::search::as_text(&content) {
            return Some(text);
        }
        if envelope.projections.unwrap_or(0) > 0 {
            match content.get("type").and_then(Value::as_str) {
                Some("message") => {
                    let citations: Vec<Value> = super::openai::message_annotations(&content)
                        .cloned()
                        .collect();
                    if citations.is_empty() {
                        return None;
                    }
                    content = Value::Array(citations);
                }
                _ => return None,
            }
        }
    }
    if !deltas.is_empty() {
        content = serde_json::json!({"content": content, "deltas": deltas});
    }
    let mut content = super::without_identity(content);
    strip_private(&mut content);
    Some(format!("[{provider} content]\n{content}"))
}

fn private_reasoning(value: &Value) -> bool {
    matches!(
        value.get("type").and_then(Value::as_str),
        Some("thinking" | "redacted_thinking" | "reasoning" | "call_signatures" | "text_signature")
    )
}

fn strip_private(value: &mut Value) {
    if private_reasoning(value) {
        *value = Value::Null;
        return;
    }
    match value {
        Value::Object(fields) => {
            fields.retain(|key, _| {
                !matches!(
                    key.as_str(),
                    "encrypted_content"
                        | "encrypted_index"
                        | "signature"
                        | "signatures"
                        | "thoughtSignature"
                        | "identity"
                        | "thinking"
                        | "thought"
                        | "data"
                )
            });
            for value in fields.values_mut() {
                strip_private(value);
            }
        }
        Value::Array(items) => {
            for value in items {
                strip_private(value);
            }
        }
        _ => {}
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn native_reasoning_is_private_even_when_it_has_a_text_projection() {
        for projections in [0, 1] {
            let block = ContentBlock::opaque(
                "openai",
                serde_json::json!({"type":"wisp_native_item","projection_blocks":projections,"item":{"type":"reasoning","summary":[{"type":"summary_text","text":"private reasoning"}]}}),
            );
            assert!(text(&block, "anthropic").is_none());
        }
    }

    #[test]
    fn gemini_native_reasoning_envelope_is_private_across_providers() {
        let block = super::super::envelope(
            "gemini",
            &serde_json::json!({"type":"reasoning","thought":"private gemini thought","signature":"sig_gemini_secret"}),
            1,
            "id_gemini_1",
        );
        assert!(text(&block, "anthropic").is_none());
        assert!(text(&block, "openai").is_none());
        assert!(text(&block, "text").is_none());
    }

    #[test]
    fn gemini_call_signatures_opaque_is_private_across_providers() {
        let block = ContentBlock::opaque(
            "gemini",
            serde_json::json!({"type":"call_signatures","signatures":{"call_1":"sig_secret_blob"}}),
        );
        assert!(text(&block, "anthropic").is_none());
        assert!(text(&block, "openai").is_none());
    }

    #[test]
    fn nested_signatures_fields_are_stripped_from_projected_foreign_blocks() {
        let block = ContentBlock::opaque(
            "gemini",
            serde_json::json!({
                "type": "grounding_summary",
                "note": "kept",
                "nested": {
                    "signatures": {"call_1": "sig_secret_blob"},
                    "signature": "sig_other",
                    "thoughtSignature": "sig_thought_secret",
                    "thought": "thought_secret_body",
                    "visible": "ok"
                }
            }),
        );
        let projected = text(&block, "anthropic").unwrap();
        assert!(projected.contains("kept"));
        assert!(projected.contains("visible"));
        assert!(!projected.contains("sig_secret_blob"));
        assert!(!projected.contains("sig_other"));
        assert!(!projected.contains("sig_thought_secret"));
        assert!(!projected.contains("thought_secret_body"));
        assert!(!projected.contains("\"signatures\""));
        assert!(!projected.contains("\"thoughtSignature\""));
        assert!(!projected.contains("\"thought\""));

        let text_sig = super::super::envelope(
            "gemini",
            &serde_json::json!({"type": "text_signature", "signature": "sig_text_only"}),
            1,
            "id_gemini_1",
        );
        assert!(text(&text_sig, "anthropic").is_none());
        assert!(text(&text_sig, "text").is_none());

        let part_env = super::super::envelope(
            "gemini",
            &serde_json::json!({
                "type": "part",
                "part": {
                    "toolCall": {"toolType": "GOOGLE_SEARCH_WEB", "args": {"q": "rust"}},
                    "thoughtSignature": "sig_tool_call_secret"
                }
            }),
            0,
            "id_gemini_1",
        );
        let part_projected = text(&part_env, "anthropic").unwrap();
        assert!(part_projected.contains("GOOGLE_SEARCH_WEB"));
        assert!(!part_projected.contains("sig_tool_call_secret"));
    }

    #[test]
    fn a_search_item_becomes_one_line_naming_its_subject() {
        let item = |action: serde_json::Value| {
            let item = serde_json::json!({"type":"web_search_call","id":"ws_1","status":"completed","action":action});
            let raw =
                serde_json::json!({"type":"wisp_native_item","projection_blocks":0,"item":item});
            text(&ContentBlock::opaque("openai", raw), "text")
        };
        let sources = serde_json::json!([{"type":"url","url":"https://example.com/source"}]);
        let searched =
            item(serde_json::json!({"type":"search","query":"example query","sources":sources}));
        assert_eq!(searched.as_deref(), Some("[web search: \"example query\"]"));
        assert_eq!(item(serde_json::json!({"type":"search"})), None);
        let call = serde_json::json!({
            "type": "search_call", "call_id": "s1",
            "part": {
                "functionCall": {"name": crate::search::WEB_SEARCH, "args": {"query": "example query"}},
                "thoughtSignature": "placeholder",
            },
        });
        let block = super::super::envelope("gemini", &call, 0, "account");
        assert_eq!(
            text(&block, "openai").as_deref(),
            Some("[web search: \"example query\"]")
        );
        let result = super::super::envelope(
            "gemini",
            &serde_json::json!({"type":"search_result", "output":"findings"}),
            0,
            "account",
        );
        assert_eq!(text(&result, "anthropic").as_deref(), Some("findings"));
    }
}
