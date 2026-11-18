fn without_thinking(messages: &serde_json::Value) -> Vec<serde_json::Value> {
    let mut messages = messages.as_array().unwrap().clone();
    for message in &mut messages {
        if let Some(content) = message["content"].as_array_mut() {
            content.retain(|b| b["type"] != "thinking" && b["type"] != "redacted_thinking");
        }
    }
    messages
}

#[derive(Clone, Copy)]
pub(crate) struct Rebuilt {
    pub(crate) at: usize,
    pub(crate) system_changes: bool,
}

pub(crate) fn assert_prefix_stable(turns: &[serde_json::Value], rebuilt: Option<Rebuilt>) {
    assert!(turns.len() >= 2, "{turns:#?}");
    for (index, pair) in turns.windows(2).enumerate() {
        let messages = if pair[0].get("messages").is_some() {
            "messages"
        } else {
            "input"
        };
        assert_eq!(pair[0]["instructions"], pair[1]["instructions"]);
        assert_eq!(pair[0]["prompt_cache_key"], pair[1]["prompt_cache_key"]);
        if let Some(rebuilt) = rebuilt.filter(|r| r.at == index + 1) {
            assert_ne!(
                pair[0]["tools"], pair[1]["tools"],
                "the rebuild sends a new array"
            );
            assert_eq!(
                pair[0]["system"] != pair[1]["system"],
                rebuilt.system_changes,
                "the rebuild's prompt"
            );
            let (before, after) = (
                without_thinking(&pair[0][messages]),
                without_thinking(&pair[1][messages]),
            );
            assert!(after.len() > before.len(), "{pair:#?}");
            assert_eq!(before[..], after[..before.len()], "{pair:#?}");
        } else {
            assert_eq!(
                pair[0]["tools"], pair[1]["tools"],
                "the tools array never changes"
            );
            assert_eq!(pair[0]["system"], pair[1]["system"]);
            let before = pair[0][messages].as_array().unwrap();
            let after = pair[1][messages].as_array().unwrap();
            assert!(after.len() > before.len(), "{after:#?}");
            assert_eq!(
                before[..],
                after[..before.len()],
                "the first request is a prefix of the second"
            );
        }
    }
}

#[allow(
    dead_code,
    reason = "used by wisp-cli's Gemini integration test; learn shares support/prefix.rs"
)]
pub(crate) fn assert_gemini_prefix_stable(turns: &[serde_json::Value]) {
    assert!(turns.len() >= 2, "{turns:#?}");
    for pair in turns.windows(2) {
        assert_eq!(
            pair[0]["tools"], pair[1]["tools"],
            "the Gemini tools array never changes"
        );
        assert_eq!(
            pair[0]["systemInstruction"], pair[1]["systemInstruction"],
            "the Gemini systemInstruction never changes"
        );
        assert_eq!(
            pair[0]["generationConfig"], pair[1]["generationConfig"],
            "the Gemini generationConfig never changes"
        );
        let before = pair[0]["contents"].as_array().unwrap();
        let after = pair[1]["contents"].as_array().unwrap();
        assert!(after.len() > before.len(), "{after:#?}");
        assert_eq!(
            before[..],
            after[..before.len()],
            "each Gemini request contents array is a strict prefix of the next"
        );
    }
}
