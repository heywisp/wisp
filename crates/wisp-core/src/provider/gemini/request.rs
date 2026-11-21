use std::collections::{BTreeMap, BTreeSet};

use serde_json::{Map, Value, json};
use wisp_protocol::agents::MAIL_OPEN;
use wisp_protocol::settings::GEMINI_WEB_SEARCH;
use wisp_protocol::{ContentBlock, Effort, ErrorKind, ImageSource, MediaSource, Message, Role};
use wisp_tool_text::{CONTEXT_PREFIX, SENT_PREFIX};

use super::super::{
    ModelCapabilities, NotSent, PrefixBytes, ProviderError, REASONING_ITEM, Request, WirePrint,
    not_sent_note,
};
use super::{CALL_SIGNATURES_ITEM, FOLD_TEXT, PART_ITEM, TEXT_SIGNATURE_ITEM};
use crate::endpoints::ProviderKind;
use crate::tool::Entry;

const SKIP_SIGNATURE_VALIDATOR: &str = "skip_thought_signature_validator";

fn stop_sequences() -> [String; 4] {
    [
        format!("\nuser\n{SENT_PREFIX}"),
        format!("\nuser\n{CONTEXT_PREFIX}"),
        format!("\nuser\n{MAIL_OPEN}"),
        format!("\n{MAIL_OPEN} from=\""),
    ]
}

#[derive(Debug)]
pub(super) struct BuiltBody {
    pub(super) body: Value,
}

#[derive(Debug)]
struct BuiltPrefix {
    thinking_config: Value,
    tools: Vec<Value>,
    system_instruction: Option<Value>,
}

fn validate_request(
    request: &Request<'_>,
    capabilities: Option<ModelCapabilities>,
    official: bool,
) -> Result<(), ProviderError> {
    if official
        && crate::endpoints::goes::model_provider(&request.session.model)
            .is_some_and(|p| p != ProviderKind::Gemini)
    {
        return Err(ProviderError::new(
            ErrorKind::InvalidRequest,
            format!(
                "select a Gemini model for this account, such as {}",
                super::default_model()
            ),
        ));
    }
    if request
        .server_tools
        .iter()
        .any(|tool| tool.kind != GEMINI_WEB_SEARCH)
        || request
            .compaction
            .is_some_and(|ask| ask.native_instructions.is_some())
    {
        return Err(ProviderError::new(
            ErrorKind::InvalidRequest,
            "Gemini hosted tools and native compaction are not enabled for this connection",
        ));
    }
    if request.options.debug_cache {
        return Err(ProviderError::new(
            ErrorKind::InvalidRequest,
            "Gemini does not support Anthropic cache diagnostics",
        ));
    }
    if let Some(caps) = capabilities
        && !caps.accepts_effort(None, request.session.effort)
    {
        return Err(ProviderError::new(
            ErrorKind::InvalidRequest,
            "the selected Gemini model does not support this effort",
        ));
    }
    Ok(())
}

fn build_prefix(
    request: &Request<'_>,
    capabilities: Option<ModelCapabilities>,
    official: bool,
) -> Result<BuiltPrefix, ProviderError> {
    validate_request(request, capabilities, official)?;
    Ok(BuiltPrefix {
        thinking_config: thinking_config(request.session.effort, capabilities)
            .unwrap_or(Value::Null),
        tools: build_tools(request),
        system_instruction: system_instruction(request),
    })
}

pub(super) fn build_body(
    request: &Request<'_>,
    capabilities: Option<ModelCapabilities>,
    identity: &str,
    official: bool,
) -> Result<BuiltBody, ProviderError> {
    let prefix = build_prefix(request, capabilities, official)?;
    let contents = build_contents(request, identity)?;

    let mut generation_config = Map::new();
    generation_config.insert(
        "maxOutputTokens".to_owned(),
        Value::from(request.session.max_tokens),
    );
    generation_config.insert("stopSequences".to_owned(), json!(stop_sequences()));
    if !prefix.thinking_config.is_null() {
        generation_config.insert("thinkingConfig".to_owned(), prefix.thinking_config);
    }

    let mut root = Map::new();
    if let Some(sys) = prefix.system_instruction {
        root.insert("systemInstruction".to_owned(), sys);
    }
    root.insert("contents".to_owned(), Value::Array(contents));
    if !prefix.tools.is_empty() {
        root.insert("tools".to_owned(), Value::Array(prefix.tools));
    }
    root.insert(
        "generationConfig".to_owned(),
        Value::Object(generation_config),
    );

    Ok(BuiltBody {
        body: Value::Object(root),
    })
}

pub(super) fn prefix_bytes(
    request: &Request<'_>,
    capabilities: Option<ModelCapabilities>,
    official: bool,
) -> Option<PrefixBytes> {
    let prefix = build_prefix(request, capabilities, official).ok()?;
    let effort = if prefix.thinking_config.is_null() {
        Vec::new()
    } else {
        serde_json::to_vec(&prefix.thinking_config).ok()?
    };
    let tools = Value::Array(prefix.tools);
    let system = prefix.system_instruction.unwrap_or(Value::Null);
    Some(PrefixBytes {
        model: serde_json::to_vec(&request.session.model).ok()?,
        effort,
        thinking: serde_json::to_vec(&prefix.thinking_config).ok()?,
        tools: serde_json::to_vec(&tools).ok()?,
        system: serde_json::to_vec(&system).ok()?,
    })
}

pub(super) fn wire_print(
    request: &Request<'_>,
    capabilities: Option<ModelCapabilities>,
    identity: &str,
    official: bool,
) -> Option<WirePrint> {
    validate_request(request, capabilities, official).ok()?;
    let contents = build_contents(request, identity).ok()?;
    let messages = contents
        .iter()
        .map(crate::session::sha256_json_hex)
        .collect::<Option<Vec<_>>>()?;
    Some(WirePrint {
        betas: String::new(),
        pinned: String::new(),
        blocks: None,
        messages,
        thinking: BTreeSet::new(),
    })
}

pub(super) fn cache_prefix(
    request: &Request<'_>,
    capabilities: Option<ModelCapabilities>,
    official: bool,
) -> Option<String> {
    let prefix = build_prefix(request, capabilities, official).ok()?;
    let tools = Value::Array(prefix.tools);
    let system = prefix.system_instruction.unwrap_or(Value::Null);
    crate::session::sha256_json_hex(&(
        &request.session.model,
        &prefix.thinking_config,
        &tools,
        &system,
    ))
}

fn system_instruction(request: &Request<'_>) -> Option<Value> {
    let joined = request
        .session
        .system
        .iter()
        .chain(request.session.instructions.iter())
        .map(String::as_str)
        .filter(|s| !s.is_empty())
        .collect::<Vec<_>>()
        .join("\n\n");
    (!joined.is_empty()).then(|| json!({ "parts": [{ "text": joined }] }))
}

pub(super) fn thinking_config(
    effort: Effort,
    capabilities: Option<ModelCapabilities>,
) -> Option<Value> {
    let caps = capabilities?;
    if let Some(budgets) = caps.thinking_budgets {
        let budget = budgets.get(effort)?;
        return Some(json!({
            "includeThoughts": true,
            "thinkingBudget": budget,
        }));
    }
    if caps.adaptive_thinking && !caps.efforts.is_empty() && !caps.effort_in_id {
        let level = match effort {
            Effort::Low => "LOW",
            Effort::Medium => "MEDIUM",
            Effort::Xhigh if caps.efforts.contains(&Effort::Xhigh) => "XHIGH",
            Effort::High | Effort::Xhigh | Effort::Max => "HIGH",
        };
        return Some(json!({
            "includeThoughts": true,
            "thinkingLevel": level,
        }));
    }
    if caps.adaptive_thinking {
        return Some(json!({
            "includeThoughts": true,
        }));
    }
    None
}

fn build_tools(request: &Request<'_>) -> Vec<Value> {
    let mut declarations = Vec::new();
    for entry in crate::tool::ordered(request.tools, &[], request.server_tools) {
        match entry {
            Entry::Client { spec, .. } => {
                declarations.push(json!({
                    "name": spec.name,
                    "description": spec.description,
                    "parametersJsonSchema": clean_json_schema(&spec.input_schema),
                }));
            }
            Entry::Server(spec) if spec.kind == GEMINI_WEB_SEARCH => {
                if request.rendering.search_only {
                    return vec![json!({"googleSearch": {}})];
                }
                declarations.push(super::search::declaration());
            }
            Entry::Server(_) => {}
        }
    }
    let mut tools = Vec::new();
    if !declarations.is_empty() {
        tools.push(json!({ "functionDeclarations": declarations }));
    }
    tools
}

pub(super) fn clean_json_schema(schema: &Value) -> Value {
    match schema {
        Value::Object(src) => {
            let mut out = Map::new();
            for (key, val) in src {
                if key == "$schema" {
                    continue;
                }
                out.insert(key.clone(), clean_json_schema(val));
            }
            Value::Object(out)
        }
        Value::Array(items) => Value::Array(items.iter().map(clean_json_schema).collect()),
        other => other.clone(),
    }
}

fn answered_reply(messages: &[Message], index: usize) -> Option<&Message> {
    messages[..index]
        .iter()
        .rev()
        .find(|m| m.role == Role::Assistant)
}

fn adjacent_tool_name<'a>(
    messages: &'a [Message],
    result_msg_index: usize,
    tool_use_id: &str,
) -> Option<&'a str> {
    let assistant = answered_reply(messages, result_msg_index)?;
    assistant.content.iter().find_map(|block| match block {
        ContentBlock::ToolUse { id, name, .. } if id == tool_use_id => Some(name.as_str()),
        _ => None,
    })
}

fn folds_text(messages: &[Message], index: usize) -> bool {
    answered_reply(messages, index).is_some_and(|assistant| {
        assistant.content.iter().any(|block| {
            matches!(block, ContentBlock::Opaque { provider, raw, .. }
                    if provider == super::PROVIDER
                        && raw.get("type").and_then(Value::as_str) == Some(CALL_SIGNATURES_ITEM)
                        && raw.get(FOLD_TEXT) == Some(&Value::Bool(true)))
        })
    })
}

fn fold_into_result(contents: &mut [Value], parts: &mut [Value], text: &str) -> bool {
    let earlier = contents
        .last_mut()
        .filter(|turn| turn.get("role").and_then(Value::as_str) == Some("user"))
        .and_then(|turn| turn.get_mut("parts"))
        .and_then(Value::as_array_mut);
    let output = parts
        .iter_mut()
        .rev()
        .chain(
            earlier
                .into_iter()
                .flat_map(|earlier| earlier.iter_mut().rev()),
        )
        .find_map(|part| part.pointer_mut("/functionResponse/response/output"));
    let Some(Value::String(output)) = output else {
        return false;
    };
    if !output.is_empty() {
        output.push_str("\n\n");
    }
    output.push_str(text);
    true
}

fn index_tool_names(messages: &[Message]) -> BTreeMap<&str, &str> {
    let mut map = BTreeMap::new();
    for message in messages {
        for block in &message.content {
            if let ContentBlock::ToolUse { id, name, .. } = block {
                map.insert(id.as_str(), name.as_str());
            }
        }
    }
    map
}

fn extract_call_signatures<'a>(
    content: &'a [ContentBlock],
    identity: &str,
) -> BTreeMap<&'a str, &'a str> {
    let mut out = BTreeMap::new();
    for block in content {
        let ContentBlock::Opaque { provider, raw, .. } = block else {
            continue;
        };
        if provider == super::PROVIDER
            && raw.get("type").and_then(Value::as_str) == Some(CALL_SIGNATURES_ITEM)
            && raw.get("identity").and_then(Value::as_str) == Some(identity)
            && let Some(sigs) = raw.get("signatures").and_then(Value::as_object)
        {
            for (call_id, sig) in sigs {
                if let Some(sig_str) = sig.as_str().filter(|s| !s.is_empty()) {
                    out.insert(call_id.as_str(), sig_str);
                }
            }
        }
    }
    out
}

fn media_part(carried: &[&str], why: NotSent, media_type: &str, data: &str) -> Value {
    if carried.contains(&media_type) {
        json!({
            "inlineData": {
                "mimeType": media_type,
                "data": data,
            }
        })
    } else {
        json!({
            "text": not_sent_note(media_type, why),
        })
    }
}

fn build_contents(request: &Request<'_>, identity: &str) -> Result<Vec<Value>, ProviderError> {
    let tool_names = index_tool_names(request.messages);
    let (carried, why) = if request.rendering.attachments_as_notes {
        (&[][..], NotSent::Model)
    } else {
        (super::MEDIA_TYPES, NotSent::Server)
    };

    let mut contents: Vec<Value> = Vec::new();
    for (mi, message) in request.messages.iter().enumerate() {
        let as_text = request.rendering.as_text(mi);
        match message.role {
            Role::Assistant => {
                let signatures = if as_text {
                    BTreeMap::new()
                } else {
                    extract_call_signatures(&message.content, identity)
                };
                let has_signed_call = message.content.iter().any(|block| match block {
                    ContentBlock::ToolUse { id, .. } => signatures.contains_key(id.as_str()),
                    _ => false,
                });

                let is_quiet = crate::provider::quiet_end(&message.content);
                let mut parts = Vec::new();
                let mut responses = Vec::new();
                let mut has_signed_thinking = false;
                let mut first_call_seen = false;
                let mut first_unsigned_call_idx: Option<usize> = None;
                let mut index = 0;

                while index < message.content.len() {
                    let block = &message.content[index];
                    if let Some((item, projections, item_identity)) = super::native_item(block) {
                        let end = index.saturating_add(1).saturating_add(projections);
                        if end > message.content.len() {
                            return Err(ProviderError::malformed(
                                "invalid Gemini native item projection",
                            ));
                        }
                        let item_type = item.get("type").and_then(Value::as_str);
                        let is_reasoning = item_type == Some(REASONING_ITEM);
                        let cut =
                            is_reasoning && (index..end).any(|bi| request.rendering.cut(mi, bi));
                        if !cut && !as_text && item_identity == identity {
                            match item_type {
                                Some(REASONING_ITEM) => {
                                    let thought = item
                                        .get("thought")
                                        .and_then(Value::as_str)
                                        .unwrap_or_default();
                                    let signature = item
                                        .get("signature")
                                        .and_then(Value::as_str)
                                        .unwrap_or_default();
                                    if !thought.is_empty() || !signature.is_empty() {
                                        let mut part = Map::new();
                                        part.insert("thought".to_owned(), Value::Bool(true));
                                        part.insert(
                                            "text".to_owned(),
                                            Value::String(thought.to_owned()),
                                        );
                                        if !signature.is_empty() {
                                            part.insert(
                                                "thoughtSignature".to_owned(),
                                                Value::String(signature.to_owned()),
                                            );
                                            has_signed_thinking = true;
                                        }
                                        parts.push(Value::Object(part));
                                    }
                                }
                                Some(TEXT_SIGNATURE_ITEM) if projections == 0 => {
                                    let signature = item
                                        .get("signature")
                                        .and_then(Value::as_str)
                                        .unwrap_or_default();
                                    if !signature.is_empty() {
                                        let mut part = Map::new();
                                        part.insert(
                                            "text".to_owned(),
                                            Value::String(String::new()),
                                        );
                                        part.insert(
                                            "thoughtSignature".to_owned(),
                                            Value::String(signature.to_owned()),
                                        );
                                        has_signed_thinking = true;
                                        parts.push(Value::Object(part));
                                    }
                                }
                                Some(TEXT_SIGNATURE_ITEM) if projections == 1 => {
                                    let signature = item
                                        .get("signature")
                                        .and_then(Value::as_str)
                                        .unwrap_or_default();
                                    if let ContentBlock::Text { text } = &message.content[index + 1]
                                        && (!text.is_empty() || !signature.is_empty())
                                    {
                                        let mut part = Map::new();
                                        part.insert("text".to_owned(), Value::String(text.clone()));
                                        if !signature.is_empty() {
                                            part.insert(
                                                "thoughtSignature".to_owned(),
                                                Value::String(signature.to_owned()),
                                            );
                                            has_signed_thinking = true;
                                        }
                                        parts.push(Value::Object(part));
                                    }
                                }
                                Some(super::search::RESULT) => {
                                    if let Some(response) = super::search::response_part(item) {
                                        responses.push(response);
                                    }
                                }
                                Some(PART_ITEM | super::search::CALL) => {
                                    if let Some(raw_part) = item.get("part") {
                                        if raw_part
                                            .get("thoughtSignature")
                                            .and_then(Value::as_str)
                                            .is_some_and(|s| !s.is_empty())
                                        {
                                            has_signed_thinking = true;
                                        }
                                        if item_type == Some(super::search::CALL) {
                                            if !first_call_seen
                                                && raw_part
                                                    .get("thoughtSignature")
                                                    .and_then(Value::as_str)
                                                    .is_none_or(str::is_empty)
                                            {
                                                first_unsigned_call_idx = Some(parts.len());
                                            }
                                            first_call_seen = true;
                                        }
                                        parts.push(raw_part.clone());
                                    }
                                }
                                _ => {}
                            }
                            index = end;
                            continue;
                        }
                        if cut {
                            index = end;
                            continue;
                        }
                        if let Some(text) = super::super::foreign::text(block, "text") {
                            parts.push(json!({ "text": text }));
                        }
                        index += 1;
                        continue;
                    }

                    match block {
                        ContentBlock::Text { text } => {
                            if !(is_quiet && has_signed_thinking) && !text.is_empty() {
                                parts.push(json!({ "text": text }));
                            }
                        }
                        ContentBlock::ToolUse { id, name, input } => {
                            let mut part = Map::new();
                            part.insert(
                                "functionCall".to_owned(),
                                json!({
                                    "id": id,
                                    "name": name,
                                    "args": input,
                                }),
                            );
                            if let Some(sig) = signatures.get(id.as_str()) {
                                part.insert(
                                    "thoughtSignature".to_owned(),
                                    Value::String((*sig).to_owned()),
                                );
                            } else if !first_call_seen {
                                first_unsigned_call_idx = Some(parts.len());
                            }
                            first_call_seen = true;
                            parts.push(Value::Object(part));
                        }
                        ContentBlock::Image {
                            source: ImageSource::Base64 { media_type, data },
                        } => {
                            parts.push(media_part(carried, why, media_type.as_str(), data));
                        }
                        ContentBlock::Media {
                            source: MediaSource::Base64 { media_type, data },
                        } => {
                            parts.push(media_part(carried, why, media_type.as_str(), data));
                        }
                        ContentBlock::Opaque { provider, .. } => {
                            if provider != super::PROVIDER
                                && let Some(text) =
                                    super::super::foreign::text(block, super::PROVIDER)
                            {
                                parts.push(json!({ "text": text }));
                            }
                        }
                        ContentBlock::Thinking { .. }
                        | ContentBlock::RedactedThinking { .. }
                        | ContentBlock::Effort { .. }
                        | ContentBlock::ToolAddition { .. }
                        | ContentBlock::ToolRemoval { .. }
                        | ContentBlock::ToolResult { .. } => {}
                    }
                    index += 1;
                }

                if !has_signed_thinking
                    && !has_signed_call
                    && let Some(idx) = first_unsigned_call_idx
                    && let Some(Value::Object(part)) = parts.get_mut(idx)
                {
                    part.insert(
                        "thoughtSignature".to_owned(),
                        Value::String(SKIP_SIGNATURE_VALIDATOR.to_owned()),
                    );
                }
                push_turn(&mut contents, "model", parts);
                push_turn(&mut contents, "user", responses);
            }
            Role::User | Role::System => {
                let fold = folds_text(request.messages, mi);
                let mut parts = Vec::new();
                for block in &message.content {
                    match block {
                        ContentBlock::Text { text } => {
                            if !text.is_empty()
                                && !(fold && fold_into_result(&mut contents, &mut parts, text))
                            {
                                parts.push(json!({ "text": text }));
                            }
                        }
                        ContentBlock::Image {
                            source: ImageSource::Base64 { media_type, data },
                        } => {
                            parts.push(media_part(carried, why, media_type.as_str(), data));
                        }
                        ContentBlock::Media {
                            source: MediaSource::Base64 { media_type, data },
                        } => {
                            parts.push(media_part(carried, why, media_type.as_str(), data));
                        }
                        ContentBlock::ToolResult {
                            tool_use_id,
                            content,
                            is_error,
                        } => {
                            let name = adjacent_tool_name(request.messages, mi, tool_use_id)
                                .or_else(|| tool_names.get(tool_use_id.as_str()).copied())
                                .unwrap_or("unknown_tool");
                            let mut lines = Vec::new();
                            let mut inline_media = Vec::new();
                            for inner in content {
                                match inner {
                                    ContentBlock::Text { text } => lines.push(text.clone()),
                                    ContentBlock::Image {
                                        source: ImageSource::Base64 { media_type, data },
                                    } => {
                                        inline_media.push(media_part(
                                            carried,
                                            why,
                                            media_type.as_str(),
                                            data,
                                        ));
                                    }
                                    ContentBlock::Media {
                                        source: MediaSource::Base64 { media_type, data },
                                    } => {
                                        inline_media.push(media_part(
                                            carried,
                                            why,
                                            media_type.as_str(),
                                            data,
                                        ));
                                    }
                                    ContentBlock::Opaque { .. } => {
                                        if let Some(text) =
                                            super::super::foreign::text(inner, super::PROVIDER)
                                        {
                                            lines.push(text);
                                        }
                                    }
                                    _ => {
                                        return Err(ProviderError::new(
                                            ErrorKind::InvalidRequest,
                                            "this Gemini connection cannot represent this block inside a tool result",
                                        ));
                                    }
                                }
                            }
                            parts.push(json!({
                                "functionResponse": {
                                    "id": tool_use_id,
                                    "name": name,
                                    "response": {
                                        "output": lines.join("\n"),
                                        "is_error": is_error,
                                    }
                                }
                            }));
                            parts.extend(inline_media);
                        }
                        ContentBlock::Opaque { .. } => {
                            if let Some(text) = super::super::foreign::text(block, super::PROVIDER)
                                && !(fold && fold_into_result(&mut contents, &mut parts, &text))
                            {
                                parts.push(json!({ "text": text }));
                            }
                        }
                        ContentBlock::Thinking { .. }
                        | ContentBlock::RedactedThinking { .. }
                        | ContentBlock::Effort { .. }
                        | ContentBlock::ToolAddition { .. }
                        | ContentBlock::ToolRemoval { .. }
                        | ContentBlock::ToolUse { .. } => {}
                    }
                }
                push_turn(&mut contents, "user", parts);
            }
        }
    }
    Ok(contents)
}

fn push_turn(contents: &mut Vec<Value>, role: &str, parts: Vec<Value>) {
    if parts.is_empty() {
        return;
    }
    if let Some(last) = contents.last_mut()
        && last.get("role").and_then(Value::as_str) == Some(role)
        && let Some(existing) = last.get_mut("parts").and_then(Value::as_array_mut)
    {
        existing.extend(parts);
        return;
    }
    contents.push(json!({
        "role": role,
        "parts": parts,
    }));
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::provider::{Features, Rendering, RequestOptions, ServerToolSpec};
    use crate::tool::ToolSpec;
    use wisp_protocol::log::{BlockPosition, Cut, LoggedMessage};
    use wisp_protocol::{CacheTtl, ImageType, MessageTime, SessionConfig};

    fn gemini_session(model: &str, effort: Effort) -> SessionConfig {
        SessionConfig {
            model: model.into(),
            system: Some("Core system".into()),
            effort,
            cache_ttl: CacheTtl::OneHour,
            max_tokens: 8192,
            message_time: MessageTime::Off,
            instructions: Some("AGENTS instructions".into()),
            context_note: false,
        }
    }

    fn make_req<'a>(
        session: &'a SessionConfig,
        messages: &'a [Message],
        tools: &'a [ToolSpec],
        server_tools: &'a [ServerToolSpec],
        rendering: &'a Rendering,
    ) -> Request<'a> {
        Request {
            session,
            tools,
            deferred: &[],
            by_value: &[],
            server_tools,
            messages,
            previous_response_id: None,
            features: Features::default(),
            options: RequestOptions::default(),
            rendering,
            compaction: None,
        }
    }

    const TEST_ID: &str = "id_gemini_account_a";

    fn content_digests(body: &Value) -> Vec<String> {
        body["contents"]
            .as_array()
            .unwrap()
            .iter()
            .map(|entry| crate::session::sha256_json_hex(entry).unwrap())
            .collect()
    }

    #[test]
    fn search_and_client_calls_replay_together_and_foreign_history_is_text() {
        let session = gemini_session("gemini-4-carbon", Effort::High);
        let rendering = Rendering::default();
        let part = json!({
            "functionCall": {"id": "s1", "name": crate::search::WEB_SEARCH, "args": {"query": "q"}},
            "thoughtSignature": "placeholder",
        });
        let call = super::super::search::call_item("s1", &part);
        let result = json!({
            "type": "search_result", "call_id": "s1", "id": "s1",
            "name": crate::search::WEB_SEARCH, "output": "findings", "is_error": false,
            "sources": [{"title": "Example", "url": "https://example.com/source"}],
        });
        let messages = [
            Message::user_text("go"),
            Message {
                role: Role::Assistant,
                content: vec![
                    crate::provider::envelope("gemini", &call, 0, TEST_ID),
                    ContentBlock::ToolUse {
                        id: "c1".into(),
                        name: "read".into(),
                        input: json!({"path": "file"}),
                    },
                    crate::provider::envelope("gemini", &result, 0, TEST_ID),
                ],
            },
            Message {
                role: Role::User,
                content: vec![ContentBlock::ToolResult {
                    tool_use_id: "c1".into(),
                    content: vec![ContentBlock::Text {
                        text: "file text".into(),
                    }],
                    is_error: false,
                }],
            },
        ];
        let request = make_req(&session, &messages, &[], &[], &rendering);
        let contents = build_contents(&request, TEST_ID).unwrap();
        assert_eq!(contents.len(), 3);
        assert_eq!(contents[1]["role"], "model");
        assert_eq!(contents[1]["parts"][0], part);
        assert_eq!(contents[1]["parts"][1]["functionCall"]["id"], "c1");
        assert!(contents[1]["parts"][1].get("thoughtSignature").is_none());
        assert_eq!(contents[2]["role"], "user");
        assert_eq!(contents[2]["parts"].as_array().unwrap().len(), 2);
        assert_eq!(contents[2]["parts"][0]["functionResponse"]["id"], "s1");
        assert_eq!(contents[2]["parts"][1]["functionResponse"]["id"], "c1");
        assert!(
            !serde_json::to_string(&contents)
                .unwrap()
                .contains("sources")
        );
        let foreign = build_contents(&request, "other account").unwrap();
        assert_eq!(
            foreign[1]["parts"][0],
            json!({"text": crate::search::call_line("q")})
        );
        assert_eq!(foreign[1]["parts"][2], json!({"text": "findings"}));
        assert!(
            !serde_json::to_string(&foreign)
                .unwrap()
                .contains("placeholder")
        );
        let mut unsigned = messages.clone();
        let mut unsigned_part = part;
        unsigned_part
            .as_object_mut()
            .unwrap()
            .remove("thoughtSignature");
        let unsigned_call = super::super::search::call_item("s1", &unsigned_part);
        unsigned[1].content[0] = crate::provider::envelope("gemini", &unsigned_call, 0, TEST_ID);
        let request = make_req(&session, &unsigned, &[], &[], &rendering);
        let replayed = build_contents(&request, TEST_ID).unwrap();
        assert_eq!(
            replayed[1]["parts"][0]["thoughtSignature"],
            SKIP_SIGNATURE_VALIDATOR
        );
        assert!(replayed[1]["parts"][1].get("thoughtSignature").is_none());
    }

    #[test]
    fn system_instruction_joins_system_and_instructions_and_keeps_role_system_as_user_delta() {
        let session = gemini_session("gemini-4-carbon", Effort::High);
        let rendering = Rendering::default();
        let messages = [
            Message::user_text("first question"),
            Message {
                role: Role::Assistant,
                content: vec![ContentBlock::Text {
                    text: "first answer".into(),
                }],
            },
            Message {
                role: Role::System,
                content: vec![ContentBlock::Text {
                    text: "<mode_delta>".into(),
                }],
            },
            Message {
                role: Role::User,
                content: vec![
                    ContentBlock::Text {
                        text: String::new(),
                    },
                    ContentBlock::Text {
                        text: "second question".into(),
                    },
                ],
            },
        ];
        let caps = super::super::model_capabilities(&session.model).copied();
        let built = build_body(
            &make_req(&session, &messages, &[], &[], &rendering),
            caps,
            TEST_ID,
            true,
        )
        .unwrap();
        assert_eq!(
            built.body["systemInstruction"],
            json!({"parts": [{"text": "Core system\n\nAGENTS instructions"}]})
        );
        let contents = built.body["contents"].as_array().unwrap();
        assert_eq!(contents.len(), 3);
        assert_eq!(contents[2]["role"], "user");
        assert_eq!(
            contents[2]["parts"],
            json!([{"text": "<mode_delta>"}, {"text": "second question"}])
        );
    }

    #[test]
    fn prefix_bytes_and_cache_prefix_stay_stable_across_turns_and_attribute_changes() {
        let s1 = gemini_session("gemini-4-carbon", Effort::High);
        let rendering = Rendering::default();
        let turn1 = [Message::user_text("turn 1")];
        let turn2 = [
            Message::user_text("turn 1"),
            Message {
                role: Role::Assistant,
                content: vec![ContentBlock::Text {
                    text: crate::provider::NO_TEXT.into(),
                }],
            },
            Message::user_text("turn 2"),
        ];
        let caps = super::super::model_capabilities(&s1.model).copied();
        let r1 = make_req(&s1, &turn1, &[], &[], &rendering);
        let r2 = make_req(&s1, &turn2, &[], &[], &rendering);
        let pb1 = prefix_bytes(&r1, caps, true).unwrap();
        let pb2 = prefix_bytes(&r2, caps, true).unwrap();
        assert_eq!(pb1, pb2);
        assert!(!pb1.model.is_empty());
        assert!(!pb1.effort.is_empty());
        assert!(!pb1.thinking.is_empty());
        assert!(!pb1.tools.is_empty());
        assert!(!pb1.system.is_empty());
        assert_eq!(cache_prefix(&r1, caps, true), cache_prefix(&r2, caps, true));

        let tag = |request: &Request<'_>| {
            let body = build_body(request, caps, TEST_ID, true).unwrap().body;
            let tagged = super::super::antigravity::tagged(
                body.clone(),
                &request.session.model,
                super::super::antigravity::data()
                    .unwrap()
                    .session_id
                    .as_deref(),
            );
            for (key, value) in body.as_object().unwrap() {
                assert_eq!(
                    serde_json::to_vec(&tagged[key]).unwrap(),
                    serde_json::to_vec(value).unwrap()
                );
            }
            tagged
        };
        let tagged1 = tag(&r1);
        let tagged2 = tag(&r2);
        let contents2 = tagged2["contents"].as_array().unwrap();
        let wp1 = wire_print(&r1, caps, TEST_ID, true).unwrap();
        let wp2 = wire_print(&r2, caps, TEST_ID, true).unwrap();
        assert_eq!(wp1.blocks, None);
        assert!(wp1.betas.is_empty() && wp1.pinned.is_empty() && wp1.thinking.is_empty());
        assert_eq!(wp1.messages, content_digests(&tagged1));
        assert_eq!(wp2.messages, content_digests(&tagged2));
        assert_eq!(&wp2.messages[..wp1.messages.len()], wp1.messages.as_slice());
        assert_eq!(contents2.len(), 3);
        assert_eq!(
            contents2[1],
            json!({"role": "model", "parts": [{"text": crate::provider::NO_TEXT}]})
        );
        let id = tagged1["labels"]["cascade_id"].as_str().unwrap();
        assert!(uuid::Uuid::parse_str(id).is_ok());
        for (offset, body) in [&tagged1, &tagged2].into_iter().enumerate() {
            let n = offset + 1;
            assert_eq!(body["labels"]["cascade_id"], id);
            assert_eq!(body["labels"]["trajectory_id"], id);
            assert_eq!(body["labels"]["request_id"], format!("{id}-{n}"));
            assert_eq!(body["labels"]["last_step_index"], n.to_string());
        }
        let mut turn3 = turn2.to_vec();
        turn3.push(Message {
            role: Role::Assistant,
            content: vec![
                crate::provider::envelope(
                    super::super::PROVIDER,
                    &json!({"type": REASONING_ITEM, "thought": "quiet check", "signature": ""}),
                    1,
                    TEST_ID,
                ),
                ContentBlock::Thinking {
                    thinking: "quiet check".into(),
                    signature: String::new(),
                },
                crate::provider::envelope(
                    super::super::PROVIDER,
                    &json!({"type": TEXT_SIGNATURE_ITEM, "signature": "sig_quiet_turn"}),
                    0,
                    TEST_ID,
                ),
                ContentBlock::Text {
                    text: crate::provider::NO_TEXT.into(),
                },
            ],
        });
        turn3.push(Message::user_text("turn 3"));
        let r3 = make_req(&s1, &turn3, &[], &[], &rendering);
        assert_eq!(prefix_bytes(&r3, caps, true).unwrap(), pb1);
        assert_eq!(cache_prefix(&r3, caps, true), cache_prefix(&r1, caps, true));
        let tagged3 = tag(&r3);
        assert_eq!(tagged3["labels"]["cascade_id"], id);
        let contents3 = tagged3["contents"].as_array().unwrap();
        assert_eq!(&contents3[..3], contents2.as_slice());
        let wp3 = wire_print(&r3, caps, TEST_ID, true).unwrap();
        assert_eq!(wp3.messages, content_digests(&tagged3));
        assert_eq!(&wp3.messages[..wp2.messages.len()], wp2.messages.as_slice());
        assert_eq!(
            contents3[3],
            json!({
                "role": "model",
                "parts": [
                    {"thought": true, "text": "quiet check"},
                    {"text": "", "thoughtSignature": "sig_quiet_turn"},
                ]
            })
        );

        let other = [Message::user_text("another conversation")];
        let other = make_req(&s1, &other, &[], &[], &rendering);
        assert_ne!(tag(&other)["labels"]["cascade_id"], id);

        let mut searched = turn2.to_vec();
        let call = super::super::search::call_item(
            "s1",
            &json!({"functionCall": {"name": crate::search::WEB_SEARCH, "args": {"query": "q"}}}),
        );
        searched[1]
            .content
            .push(crate::provider::envelope("gemini", &call, 0, TEST_ID));
        let result = json!({
            "type": "search_result", "call_id": "s1", "name": crate::search::WEB_SEARCH,
            "output": "findings", "is_error": false, "sources": [],
        });
        searched[1]
            .content
            .push(crate::provider::envelope("gemini", &result, 0, TEST_ID));
        let server = [crate::search::tool(GEMINI_WEB_SEARCH).unwrap().clone()];
        let before = make_req(&s1, &turn1, &[], &server, &rendering);
        let after = make_req(&s1, &searched, &[], &server, &rendering);
        assert_eq!(
            prefix_bytes(&before, caps, true),
            prefix_bytes(&after, caps, true)
        );
        assert_eq!(
            cache_prefix(&before, caps, true),
            cache_prefix(&after, caps, true)
        );

        let mut s_low = s1.clone();
        s_low.effort = Effort::Low;
        let r_low = make_req(&s_low, &turn1, &[], &[], &rendering);
        let pb_low = prefix_bytes(&r_low, caps, true).unwrap();
        assert_ne!(pb1.effort, pb_low.effort);
        assert_ne!(pb1.thinking, pb_low.thinking);
        assert_ne!(
            cache_prefix(&r1, caps, true),
            cache_prefix(&r_low, caps, true)
        );

        let mut variant = gemini_session("gemini-4-carbon-medium", Effort::Medium);
        let caps = super::super::model_capabilities(&variant.model).copied();
        let before = make_req(&variant, &turn1, &[], &[], &rendering);
        assert_ne!(tag(&before)["labels"]["cascade_id"], id);
        let prefix = prefix_bytes(&before, caps, true);
        let cache = cache_prefix(&before, caps, true);
        variant.effort = Effort::Max;
        let after = make_req(&variant, &turn2, &[], &[], &rendering);
        assert_eq!(prefix, prefix_bytes(&after, caps, true));
        assert_eq!(cache, cache_prefix(&after, caps, true));

        let s_unlisted_hi = gemini_session("gemini-3.9-future-exp", Effort::High);
        let s_unlisted_lo = gemini_session("gemini-3.9-future-exp", Effort::Low);
        let r_unlisted_hi = make_req(&s_unlisted_hi, &turn1, &[], &[], &rendering);
        let r_unlisted_lo = make_req(&s_unlisted_lo, &turn1, &[], &[], &rendering);
        let pb_unlisted_hi =
            prefix_bytes(&r_unlisted_hi, Some(super::super::LISTED_ONLY), true).unwrap();
        let pb_unlisted_lo =
            prefix_bytes(&r_unlisted_lo, Some(super::super::LISTED_ONLY), true).unwrap();
        assert!(pb_unlisted_hi.effort.is_empty());
        assert_eq!(pb_unlisted_hi, pb_unlisted_lo);
        assert_eq!(
            cache_prefix(&r_unlisted_hi, Some(super::super::LISTED_ONLY), true),
            cache_prefix(&r_unlisted_lo, Some(super::super::LISTED_ONLY), true)
        );
    }

    #[test]
    fn parameters_json_schema_strips_meta_schema_and_preserves_unions_refs_and_constraints() {
        let raw = json!({
            "$schema": "http://json-schema.org/draft-07/schema#",
            "type": "object",
            "additionalProperties": false,
            "$defs": {
                "Target": {
                    "$schema": "http://json-schema.org/draft-07/schema#",
                    "type": ["string", "integer", "null"],
                    "exclusiveMinimum": 0
                }
            },
            "properties": {
                "mode": {"const": "fast"},
                "target": {"$ref": "#/$defs/Target"}
            },
            "required": ["mode", "target"]
        });
        let cleaned = clean_json_schema(&raw);
        assert_eq!(
            cleaned,
            json!({
                "type": "object",
                "additionalProperties": false,
                "$defs": {
                    "Target": {
                        "type": ["string", "integer", "null"],
                        "exclusiveMinimum": 0
                    }
                },
                "properties": {
                    "mode": {"const": "fast"},
                    "target": {"$ref": "#/$defs/Target"}
                },
                "required": ["mode", "target"]
            })
        );

        let session = gemini_session("gemini-4-carbon", Effort::Medium);
        let rendering = Rendering::default();
        let messages = [Message::user_text("call tool")];
        let tools = [ToolSpec {
            name: "inspect".into(),
            description: "Inspect target".into(),
            input_schema: raw,
        }];
        let built = build_body(
            &make_req(&session, &messages, &tools, &[], &rendering),
            super::super::model_capabilities(&session.model).copied(),
            TEST_ID,
            true,
        )
        .unwrap();
        let decl = &built.body["tools"][0]["functionDeclarations"][0];
        assert!(decl.get("parameters").is_none());
        assert_eq!(decl["parametersJsonSchema"], cleaned);
    }

    #[test]
    fn thinking_config_follows_adaptive_thinking_budgets_pretiered_and_unlisted_capabilities() {
        let stop_list = stop_sequences();
        let rendered_mail = wisp_protocol::agents::render_mail(
            &wisp_protocol::AgentPath::root(),
            wisp_protocol::agents::MailKind::Report,
            "ok",
        );
        assert!(rendered_mail.starts_with(&stop_list[3][1..]));
        let stops = json!([
            format!("\nuser\n{SENT_PREFIX}"),
            format!("\nuser\n{CONTEXT_PREFIX}"),
            format!("\nuser\n{MAIL_OPEN}"),
            format!("\n{MAIL_OPEN} from=\""),
        ]);
        let rendering = Rendering::default();
        let messages = [Message::user_text("hi")];

        let s4 = gemini_session("gemini-4-carbon", Effort::Medium);
        let caps4 = super::super::model_capabilities(&s4.model).copied();
        let b4 = build_body(
            &make_req(&s4, &messages, &[], &[], &rendering),
            caps4,
            TEST_ID,
            true,
        )
        .unwrap();
        assert_eq!(b4.body["generationConfig"]["stopSequences"], stops);
        assert_eq!(
            b4.body["generationConfig"]["thinkingConfig"],
            json!({"includeThoughts": true, "thinkingLevel": "MEDIUM"})
        );

        for model in ["gemini-4-carbon", "gemini-4-argon", "gemini-3.8-flash"] {
            let caps = super::super::model_capabilities(model).copied();
            let s_refused = gemini_session(model, Effort::Xhigh);
            let refused = build_body(
                &make_req(&s_refused, &messages, &[], &[], &rendering),
                caps,
                TEST_ID,
                true,
            )
            .unwrap_err();
            assert_eq!(refused.kind, ErrorKind::InvalidRequest, "{model}");
            assert_eq!(
                refused.message,
                "the selected Gemini model does not support this effort"
            );
            assert_eq!(
                thinking_config(Effort::Xhigh, caps),
                Some(json!({"includeThoughts": true, "thinkingLevel": "HIGH"})),
                "{model}"
            );
            let s_high = gemini_session(model, Effort::High);
            let built_high = build_body(
                &make_req(&s_high, &messages, &[], &[], &rendering),
                caps,
                TEST_ID,
                true,
            )
            .unwrap();
            assert_eq!(
                serde_json::to_vec(&built_high.body).unwrap(),
                serde_json::to_vec(&json!({
                    "contents": [{"parts": [{"text": "hi"}], "role": "user"}],
                    "generationConfig": {
                        "maxOutputTokens": 8192,
                        "stopSequences": stops,
                        "thinkingConfig": {"includeThoughts": true, "thinkingLevel": "HIGH"}
                    },
                    "systemInstruction": {"parts": [{"text": "Core system\n\nAGENTS instructions"}]}
                }))
                .unwrap(),
                "{model}"
            );
        }

        for family in ["gemini-4-hydrogen", "gemini-4-neon"] {
            let caps_f = super::super::model_capabilities(family).copied();
            for (effort, expected_level) in [(Effort::High, "HIGH"), (Effort::Xhigh, "XHIGH")] {
                let s_f = gemini_session(family, effort);
                let b_f = build_body(
                    &make_req(&s_f, &messages, &[], &[], &rendering),
                    caps_f,
                    TEST_ID,
                    true,
                )
                .unwrap();
                assert_eq!(
                    b_f.body["generationConfig"]["thinkingConfig"],
                    json!({"includeThoughts": true, "thinkingLevel": expected_level}),
                    "{family}"
                );
            }

            let xhigh_id = format!("{family}-xhigh");
            let caps_x = super::super::model_capabilities(&xhigh_id).copied();
            for effort in [Effort::Low, Effort::Medium, Effort::High, Effort::Xhigh] {
                let s_x = gemini_session(&xhigh_id, effort);
                let b_x = build_body(
                    &make_req(&s_x, &messages, &[], &[], &rendering),
                    caps_x,
                    TEST_ID,
                    true,
                )
                .unwrap();
                assert_eq!(
                    b_x.body["generationConfig"]["thinkingConfig"],
                    json!({"includeThoughts": true}),
                    "{xhigh_id}"
                );
            }
        }

        let s_pretier = gemini_session("gemini-4-carbon-medium", Effort::High);
        let caps_pretier = super::super::model_capabilities(&s_pretier.model).copied();
        let b_pretier = build_body(
            &make_req(&s_pretier, &messages, &[], &[], &rendering),
            caps_pretier,
            TEST_ID,
            true,
        )
        .unwrap();
        assert_eq!(
            b_pretier.body["generationConfig"]["thinkingConfig"],
            json!({"includeThoughts": true})
        );

        for effort in [Effort::Low, Effort::Medium, Effort::High] {
            let session = gemini_session("gemini-4-carbon-medium", effort);
            let body = build_body(
                &make_req(&session, &messages, &[], &[], &rendering),
                caps_pretier,
                TEST_ID,
                true,
            )
            .unwrap();
            assert_eq!(body.body, b_pretier.body);
        }

        let s25 = gemini_session("gemini-2.5-pro", Effort::Max);
        let caps25 = super::super::model_capabilities(&s25.model).copied();
        let b25 = build_body(
            &make_req(&s25, &messages, &[], &[], &rendering),
            caps25,
            TEST_ID,
            true,
        )
        .unwrap();
        assert_eq!(
            b25.body["generationConfig"]["thinkingConfig"],
            json!({"includeThoughts": true, "thinkingBudget": 32768})
        );

        let s_unlisted = gemini_session("gemini-3.9-future-exp", Effort::Medium);
        let b_unlisted = build_body(
            &make_req(&s_unlisted, &messages, &[], &[], &rendering),
            Some(super::super::LISTED_ONLY),
            TEST_ID,
            true,
        )
        .unwrap();
        assert!(
            b_unlisted.body["generationConfig"]
                .get("thinkingConfig")
                .is_none()
        );
    }

    #[test]
    fn call_signatures_round_trip_and_respect_rendering_cuts() {
        let session = gemini_session("gemini-4-carbon", Effort::High);
        let messages = [
            Message::user_text("run tools"),
            Message {
                role: Role::Assistant,
                content: vec![
                    crate::provider::envelope(
                        "gemini",
                        &json!({
                            "type": "reasoning",
                            "thought": "step 1",
                            "signature": "sig_think",
                        }),
                        1,
                        TEST_ID,
                    ),
                    ContentBlock::Thinking {
                        thinking: "step 1".into(),
                        signature: String::new(),
                    },
                    ContentBlock::ToolUse {
                        id: "call_a".into(),
                        name: "read".into(),
                        input: json!({"p": "1"}),
                    },
                    ContentBlock::opaque(
                        "gemini",
                        json!({
                            "type": "call_signatures",
                            "identity": TEST_ID,
                            "signatures": {"call_a": "sig_call_a"},
                        }),
                    ),
                ],
            },
        ];
        let caps = super::super::model_capabilities(&session.model).copied();
        let intact = build_body(
            &make_req(&session, &messages, &[], &[], &Rendering::default()),
            caps,
            TEST_ID,
            true,
        )
        .unwrap();
        let parts = intact.body["contents"][1]["parts"].as_array().unwrap();
        assert_eq!(parts.len(), 2);
        assert_eq!(parts[0]["thoughtSignature"], "sig_think");
        assert_eq!(parts[1]["thoughtSignature"], "sig_call_a");

        for cut_block in [0, 1] {
            let mut cut_rendering = Rendering::default();
            cut_rendering.cuts.push(Cut {
                from: BlockPosition {
                    message: 1,
                    block: cut_block,
                },
                through: 1,
                tools: false,
            });
            let after_cut = build_body(
                &make_req(&session, &messages, &[], &[], &cut_rendering),
                caps,
                TEST_ID,
                true,
            )
            .unwrap();
            let cut_parts = after_cut.body["contents"][1]["parts"].as_array().unwrap();
            assert_eq!(cut_parts.len(), 1);
            assert_eq!(cut_parts[0]["thoughtSignature"], "sig_call_a");
        }

        let quiet_with_sig = [
            Message::user_text("quiet 1"),
            Message {
                role: Role::Assistant,
                content: vec![
                    crate::provider::envelope(
                        super::super::PROVIDER,
                        &json!({"type": REASONING_ITEM, "thought": "cut me", "signature": ""}),
                        1,
                        TEST_ID,
                    ),
                    ContentBlock::Thinking {
                        thinking: "cut me".into(),
                        signature: String::new(),
                    },
                    crate::provider::envelope(
                        super::super::PROVIDER,
                        &json!({"type": TEXT_SIGNATURE_ITEM, "signature": "sig_keep"}),
                        0,
                        TEST_ID,
                    ),
                    ContentBlock::Text {
                        text: crate::provider::NO_TEXT.into(),
                    },
                ],
            },
        ];
        let mut quiet_cut = Rendering::default();
        quiet_cut.cuts.push(Cut {
            from: BlockPosition {
                message: 1,
                block: 0,
            },
            through: 1,
            tools: false,
        });
        let cut_with_sig = build_body(
            &make_req(&session, &quiet_with_sig, &[], &[], &quiet_cut),
            caps,
            TEST_ID,
            true,
        )
        .unwrap();
        assert_eq!(
            cut_with_sig.body["contents"][1]["parts"],
            json!([{"text": "", "thoughtSignature": "sig_keep"}])
        );

        let as_text_rendering = Rendering {
            native_as_text: vec![false, true],
            ..Rendering::default()
        };
        let compacted = build_body(
            &make_req(&session, &quiet_with_sig, &[], &[], &as_text_rendering),
            caps,
            TEST_ID,
            true,
        )
        .unwrap();
        assert_eq!(
            compacted.body["contents"][1]["parts"],
            json!([{"text": crate::provider::NO_TEXT}])
        );

        let quiet_signed_reasoning = [
            Message::user_text("quiet 2"),
            Message {
                role: Role::Assistant,
                content: vec![
                    crate::provider::envelope(
                        super::super::PROVIDER,
                        &json!({"type": REASONING_ITEM, "thought": "cut all", "signature": "sig_r"}),
                        1,
                        TEST_ID,
                    ),
                    ContentBlock::Thinking {
                        thinking: "cut all".into(),
                        signature: String::new(),
                    },
                    ContentBlock::Text {
                        text: crate::provider::NO_TEXT.into(),
                    },
                ],
            },
        ];
        let cut_without_sig = build_body(
            &make_req(&session, &quiet_signed_reasoning, &[], &[], &quiet_cut),
            caps,
            TEST_ID,
            true,
        )
        .unwrap();
        assert_eq!(
            cut_without_sig.body["contents"][1]["parts"],
            json!([{"text": crate::provider::NO_TEXT}])
        );

        let legacy_whitespace = [
            Message::user_text("legacy"),
            Message {
                role: Role::Assistant,
                content: vec![ContentBlock::Text {
                    text: " [no text]\n".into(),
                }],
            },
        ];
        let legacy = build_body(
            &make_req(
                &session,
                &legacy_whitespace,
                &[],
                &[],
                &Rendering::default(),
            ),
            caps,
            TEST_ID,
            true,
        )
        .unwrap();
        assert_eq!(
            legacy.body["contents"][1]["parts"],
            json!([{"text": " [no text]\n"}])
        );
    }

    #[test]
    fn unsigned_parallel_tool_calls_attach_skip_validator_only_on_first_call() {
        let session = gemini_session("gemini-4-carbon", Effort::High);
        let messages = [
            Message::user_text("parallel"),
            Message {
                role: Role::Assistant,
                content: vec![
                    ContentBlock::ToolUse {
                        id: "foreign_1".into(),
                        name: "alpha".into(),
                        input: json!({"cmd": "ls"}),
                    },
                    ContentBlock::ToolUse {
                        id: "foreign_2".into(),
                        name: "alpha".into(),
                        input: json!({"cmd": "pwd"}),
                    },
                ],
            },
        ];
        let caps = super::super::model_capabilities(&session.model).copied();
        let built = build_body(
            &make_req(&session, &messages, &[], &[], &Rendering::default()),
            caps,
            TEST_ID,
            true,
        )
        .unwrap();
        let parts = built.body["contents"][1]["parts"].as_array().unwrap();
        assert_eq!(parts.len(), 2);
        assert_eq!(
            parts[0]["thoughtSignature"],
            "skip_thought_signature_validator"
        );
        assert!(parts[1].get("thoughtSignature").is_none());

        let mut marked = messages.clone();
        marked[1].content.push(ContentBlock::opaque(
            "gemini",
            json!({"type": "call_signatures", "identity": TEST_ID, "signatures": {}, "fold_text": true}),
        ));
        for identity in [TEST_ID, "id_other_gemini_acct"] {
            let body = |history: &[Message]| {
                build_body(
                    &make_req(&session, history, &[], &[], &Rendering::default()),
                    caps,
                    identity,
                    true,
                )
                .unwrap()
                .body
            };
            assert_eq!(body(&marked), body(&messages));
        }
    }

    #[test]
    fn cross_provider_and_cross_account_switches_isolate_thinking_and_signatures() {
        let session = gemini_session("gemini-4-carbon", Effort::High);
        let caps = super::super::model_capabilities(&session.model).copied();
        let rendering = Rendering::default();

        let claude_history = [
            Message::user_text("inspect repo"),
            Message {
                role: Role::Assistant,
                content: vec![
                    ContentBlock::Thinking {
                        thinking: "claude private reasoning".into(),
                        signature: "anthropic_sig_secret".into(),
                    },
                    ContentBlock::ToolUse {
                        id: "toolu_1".into(),
                        name: "read".into(),
                        input: json!({"path": "a.rs"}),
                    },
                    ContentBlock::ToolUse {
                        id: "toolu_2".into(),
                        name: "read".into(),
                        input: json!({"path": "b.rs"}),
                    },
                ],
            },
            Message {
                role: Role::User,
                content: vec![
                    ContentBlock::ToolResult {
                        tool_use_id: "toolu_1".into(),
                        content: vec![ContentBlock::Text { text: "ok1".into() }],
                        is_error: false,
                    },
                    ContentBlock::ToolResult {
                        tool_use_id: "toolu_2".into(),
                        content: vec![ContentBlock::Text { text: "ok2".into() }],
                        is_error: false,
                    },
                ],
            },
            Message::user_text("continue on gemini"),
        ];
        let from_claude = build_body(
            &make_req(&session, &claude_history, &[], &[], &rendering),
            caps,
            TEST_ID,
            true,
        )
        .unwrap();
        let claude_wire = from_claude.body.to_string();
        assert!(!claude_wire.contains("claude private reasoning"));
        assert!(!claude_wire.contains("anthropic_sig_secret"));
        let claude_parts = from_claude.body["contents"][1]["parts"].as_array().unwrap();
        assert_eq!(claude_parts.len(), 2);
        assert_eq!(
            claude_parts[0]["thoughtSignature"],
            "skip_thought_signature_validator"
        );
        assert!(claude_parts[1].get("thoughtSignature").is_none());

        let openai_history = [
            Message::user_text("inspect on openai"),
            Message {
                role: Role::Assistant,
                content: vec![
                    crate::provider::envelope(
                        "openai",
                        &json!({
                            "type": "reasoning",
                            "id": "rs_1",
                            "encrypted_content": "enc_openai_secret",
                            "summary": [{"type": "summary_text", "text": "openai private summary"}],
                        }),
                        1,
                        "id_openai_1",
                    ),
                    ContentBlock::Thinking {
                        thinking: "openai private summary".into(),
                        signature: String::new(),
                    },
                    ContentBlock::ToolUse {
                        id: "call_oa_1".into(),
                        name: "read".into(),
                        input: json!({"path": "c.rs"}),
                    },
                ],
            },
        ];
        let from_openai = build_body(
            &make_req(&session, &openai_history, &[], &[], &rendering),
            caps,
            TEST_ID,
            true,
        )
        .unwrap();
        let openai_wire = from_openai.body.to_string();
        assert!(!openai_wire.contains("openai private summary"));
        assert!(!openai_wire.contains("enc_openai_secret"));
        let oa_parts = from_openai.body["contents"][1]["parts"].as_array().unwrap();
        assert_eq!(oa_parts.len(), 1);
        assert_eq!(
            oa_parts[0]["thoughtSignature"],
            "skip_thought_signature_validator"
        );

        let gemini_history = [
            Message::user_text("turn on gemini account a"),
            Message {
                role: Role::Assistant,
                content: vec![
                    crate::provider::envelope(
                        "gemini",
                        &json!({
                            "type": "reasoning",
                            "thought": "gemini signed thought",
                            "signature": "sig_gemini_a",
                        }),
                        1,
                        TEST_ID,
                    ),
                    ContentBlock::Thinking {
                        thinking: "gemini signed thought".into(),
                        signature: String::new(),
                    },
                    crate::provider::envelope(
                        "gemini",
                        &json!({
                            "type": "reasoning",
                            "thought": "gemini unsigned thought",
                            "signature": "",
                        }),
                        1,
                        TEST_ID,
                    ),
                    ContentBlock::Thinking {
                        thinking: "gemini unsigned thought".into(),
                        signature: String::new(),
                    },
                    ContentBlock::ToolUse {
                        id: "gem_call_1".into(),
                        name: "read".into(),
                        input: json!({"path": "x.rs"}),
                    },
                    ContentBlock::ToolUse {
                        id: "gem_call_2".into(),
                        name: "read".into(),
                        input: json!({"path": "y.rs"}),
                    },
                    ContentBlock::opaque(
                        "gemini",
                        json!({
                            "type": "call_signatures",
                            "identity": TEST_ID,
                            "signatures": {
                                "gem_call_1": "sig_call_a1",
                                "gem_call_2": "sig_call_a2",
                            },
                            "fold_text": true,
                        }),
                    ),
                ],
            },
            Message {
                role: Role::User,
                content: vec![
                    ContentBlock::ToolResult {
                        tool_use_id: "gem_call_1".into(),
                        content: vec![ContentBlock::Text { text: "rx".into() }],
                        is_error: false,
                    },
                    ContentBlock::ToolResult {
                        tool_use_id: "gem_call_2".into(),
                        content: vec![ContentBlock::Text { text: "ry".into() }],
                        is_error: false,
                    },
                ],
            },
            Message {
                role: Role::Assistant,
                content: vec![
                    crate::provider::envelope(
                        super::super::PROVIDER,
                        &json!({
                            "type": REASONING_ITEM,
                            "thought": "gemini quiet thought",
                            "signature": "",
                        }),
                        1,
                        TEST_ID,
                    ),
                    ContentBlock::Thinking {
                        thinking: "gemini quiet thought".into(),
                        signature: String::new(),
                    },
                    crate::provider::envelope(
                        super::super::PROVIDER,
                        &json!({
                            "type": TEXT_SIGNATURE_ITEM,
                            "signature": "sig_quiet_end",
                        }),
                        0,
                        TEST_ID,
                    ),
                    ContentBlock::Text {
                        text: crate::provider::NO_TEXT.into(),
                    },
                ],
            },
            Message::user_text("follow up"),
        ];

        let same_account = build_body(
            &make_req(&session, &gemini_history, &[], &[], &rendering),
            caps,
            TEST_ID,
            true,
        )
        .unwrap();
        let same_parts = same_account.body["contents"][1]["parts"]
            .as_array()
            .unwrap();
        assert_eq!(same_parts.len(), 4);
        assert_eq!(same_parts[0]["thoughtSignature"], "sig_gemini_a");
        assert_eq!(same_parts[2]["thoughtSignature"], "sig_call_a1");
        assert_eq!(same_parts[3]["thoughtSignature"], "sig_call_a2");
        assert_eq!(
            same_account.body["contents"][3]["parts"],
            json!([
                {"thought": true, "text": "gemini quiet thought"},
                {"text": "", "thoughtSignature": "sig_quiet_end"},
            ])
        );

        let other_account = build_body(
            &make_req(&session, &gemini_history, &[], &[], &rendering),
            caps,
            "id_gemini_account_b",
            true,
        )
        .unwrap();
        let other_wire = other_account.body.to_string();
        assert!(!other_wire.contains("gemini signed thought"));
        assert!(!other_wire.contains("gemini unsigned thought"));
        assert!(!other_wire.contains("gemini quiet thought"));
        assert!(!other_wire.contains("sig_gemini_a"));
        assert!(!other_wire.contains("sig_call_a1"));
        assert!(!other_wire.contains("sig_call_a2"));
        assert!(!other_wire.contains("sig_quiet_end"));
        let other_parts = other_account.body["contents"][1]["parts"]
            .as_array()
            .unwrap();
        assert_eq!(other_parts.len(), 2);
        assert_eq!(
            other_parts[0]["thoughtSignature"],
            "skip_thought_signature_validator"
        );
        assert!(other_parts[1].get("thoughtSignature").is_none());
        assert_eq!(
            other_account.body["contents"][3]["parts"],
            json!([{"text": crate::provider::NO_TEXT}])
        );

        let claude_session = gemini_session("claude-sonnet-4-6", Effort::High);
        let to_claude_bytes = crate::provider::anthropic::build_body(&make_req(
            &claude_session,
            &gemini_history,
            &[],
            &[],
            &rendering,
        ))
        .unwrap();
        let to_claude = String::from_utf8(to_claude_bytes).unwrap();
        assert!(!to_claude.contains("gemini signed thought"));
        assert!(!to_claude.contains("gemini unsigned thought"));
        assert!(!to_claude.contains("gemini quiet thought"));
        assert!(!to_claude.contains("sig_gemini_a"));
        assert!(!to_claude.contains("sig_call_a1"));
        assert!(!to_claude.contains("sig_quiet_end"));
        assert!(!to_claude.contains("call_signatures"));
        assert!(!to_claude.contains("fold_text"));
        assert!(to_claude.contains("gem_call_1"));
        let claude_json: Value = serde_json::from_str(&to_claude).unwrap();
        assert_eq!(
            claude_json["messages"][3]["content"],
            json!([{"type": "text", "text": crate::provider::NO_TEXT}])
        );

        let gpt_session = gemini_session("gpt-5.4", Effort::High);
        let to_openai_json = crate::provider::openai::build_body(&make_req(
            &gpt_session,
            &gemini_history,
            &[],
            &[],
            &rendering,
        ))
        .unwrap();
        let to_openai = to_openai_json.to_string();
        assert!(!to_openai.contains("gemini signed thought"));
        assert!(!to_openai.contains("gemini unsigned thought"));
        assert!(!to_openai.contains("gemini quiet thought"));
        assert!(!to_openai.contains("sig_gemini_a"));
        assert!(!to_openai.contains("sig_call_a1"));
        assert!(!to_openai.contains("sig_quiet_end"));
        assert!(!to_openai.contains("call_signatures"));
        assert!(!to_openai.contains("fold_text"));
        assert!(to_openai.contains("gem_call_1"));
        assert!(
            to_openai_json["input"]
                .as_array()
                .unwrap()
                .contains(&json!({
                    "role": "assistant",
                    "content": [{"type": "output_text", "text": crate::provider::NO_TEXT}],
                }))
        );
    }

    #[test]
    fn tool_results_resolve_function_name_inline_images_and_foreign_opaque_projections() {
        let session = gemini_session("gemini-4-carbon", Effort::High);
        let rendering = Rendering {
            sends_media: true,
            ..Default::default()
        };
        let search_item = json!({
            "type": "wisp_native_item",
            "projection_blocks": 0,
            "item": {
                "type": "web_search_call",
                "id": "ws_1",
                "status": "completed",
                "action": {"type": "search", "query": "rust borrow checker"}
            }
        });
        let messages = [
            Message::user_text("inspect"),
            Message {
                role: Role::Assistant,
                content: vec![
                    ContentBlock::opaque("openai", search_item.clone()),
                    ContentBlock::ToolUse {
                        id: "tu_1".into(),
                        name: "screenshot".into(),
                        input: json!({}),
                    },
                ],
            },
            Message {
                role: Role::User,
                content: vec![ContentBlock::ToolResult {
                    tool_use_id: "tu_1".into(),
                    content: vec![
                        ContentBlock::Text {
                            text: "captured".into(),
                        },
                        ContentBlock::opaque("openai", search_item),
                        ContentBlock::Image {
                            source: ImageSource::Base64 {
                                media_type: ImageType::Png,
                                data: "iVBORw0KGgo=".into(),
                            },
                        },
                    ],
                    is_error: false,
                }],
            },
        ];
        let caps = super::super::model_capabilities(&session.model).copied();
        let built = build_body(
            &make_req(&session, &messages, &[], &[], &rendering),
            caps,
            TEST_ID,
            true,
        )
        .unwrap();
        let assistant_parts = built.body["contents"][1]["parts"].as_array().unwrap();
        assert_eq!(
            assistant_parts[0]["text"],
            "[web search: \"rust borrow checker\"]"
        );
        let result_parts = built.body["contents"][2]["parts"].as_array().unwrap();
        assert_eq!(result_parts.len(), 2);
        assert_eq!(result_parts[0]["functionResponse"]["name"], "screenshot");
        assert_eq!(
            result_parts[0]["functionResponse"]["response"]["output"],
            "captured\n[web search: \"rust borrow checker\"]"
        );
        assert_eq!(result_parts[1]["inlineData"]["mimeType"], "image/png");
    }

    #[test]
    fn mail_folds_into_the_last_function_response_after_a_marked_reply() {
        let session = gemini_session("gemini-4-carbon", Effort::High);
        let caps = super::super::model_capabilities(&session.model).copied();
        let rendering = Rendering::default();
        let png = || ContentBlock::Image {
            source: ImageSource::Base64 {
                media_type: ImageType::Png,
                data: "iVBORw0KGgo=".into(),
            },
        };
        let call = |id: &str| ContentBlock::ToolUse {
            id: id.into(),
            name: "probe".into(),
            input: json!({}),
        };
        let marker = ContentBlock::opaque(
            "gemini",
            json!({"type": "call_signatures", "identity": TEST_ID, "signatures": {}, "fold_text": true}),
        );
        let history = |marked: bool| {
            let mut reply = vec![call("c1"), call("c2")];
            if marked {
                reply.push(marker.clone());
            }
            vec![
                Message::user_text("probe twice"),
                Message {
                    role: Role::Assistant,
                    content: reply,
                },
                Message {
                    role: Role::User,
                    content: vec![
                        ContentBlock::ToolResult {
                            tool_use_id: "c1".into(),
                            content: vec![ContentBlock::Text { text: "one".into() }],
                            is_error: false,
                        },
                        ContentBlock::ToolResult {
                            tool_use_id: "c2".into(),
                            content: vec![png()],
                            is_error: true,
                        },
                        ContentBlock::Text {
                            text: "mail".into(),
                        },
                    ],
                },
                Message {
                    role: Role::System,
                    content: vec![ContentBlock::Effort {
                        level: Effort::Low,
                        rendered: true,
                    }],
                },
                Message {
                    role: Role::User,
                    content: vec![
                        ContentBlock::Text {
                            text: "late steer".into(),
                        },
                        ContentBlock::opaque(
                            "openai",
                            json!({"type": "wisp_native_item", "projection_blocks": 0, "item": {"type": "web_search_call", "action": {"type": "search", "query": "q"}}}),
                        ),
                    ],
                },
                Message {
                    role: Role::Assistant,
                    content: vec![
                        crate::provider::envelope(
                            "gemini",
                            &json!({"type": "part", "part": {"functionResponse": {"response": {"output": "raw"}}}}),
                            0,
                            TEST_ID,
                        ),
                        call("c3"),
                        marker.clone(),
                    ],
                },
                Message::user_text("no result here"),
            ]
        };
        let contents = |marked: bool| {
            build_body(
                &make_req(&session, &history(marked), &[], &[], &rendering),
                caps,
                TEST_ID,
                true,
            )
            .unwrap()
            .body["contents"]
                .clone()
        };
        let response = |id: &str, output: &str, is_error: bool| json!({"functionResponse": {"id": id, "name": "probe", "response": {"output": output, "is_error": is_error}}});
        let image = json!({"inlineData": {"mimeType": "image/png", "data": "iVBORw0KGgo="}});

        let folded = contents(true);
        assert_eq!(
            folded[2]["parts"],
            json!([
                response("c1", "one", false),
                response("c2", "mail\n\nlate steer\n\n[web search: \"q\"]", true),
                image.clone(),
            ])
        );
        assert_eq!(
            folded[3]["parts"][0]["functionResponse"]["response"]["output"],
            "raw"
        );
        assert_eq!(folded[4]["parts"], json!([{"text": "no result here"}]));

        let old = contents(false);
        assert_eq!(
            old[2]["parts"],
            json!([
                response("c1", "one", false),
                response("c2", "", true),
                image,
                {"text": "mail"},
                {"text": "late steer"},
                {"text": "[web search: \"q\"]"},
            ])
        );
        assert_eq!(old[4], folded[4]);
    }

    #[test]
    fn google_search_server_tool_emits_google_search_declaration_and_gates_foreign_model_on_official()
     {
        let session = gemini_session("gemini-4-carbon", Effort::High);
        let rendering = Rendering::default();
        let messages = [Message::user_text("search")];
        let server_tools = [ServerToolSpec {
            name: "search".into(),
            kind: GEMINI_WEB_SEARCH.into(),
        }];
        let client_tools = [ToolSpec {
            name: "read".into(),
            description: "Read file".into(),
            input_schema: json!({"type": "object", "properties": {"path": {"type": "string"}}}),
        }];
        let caps = super::super::model_capabilities(&session.model).copied();
        let built = build_body(
            &make_req(
                &session,
                &messages,
                &client_tools,
                &server_tools,
                &rendering,
            ),
            caps,
            TEST_ID,
            true,
        )
        .unwrap();
        let tools = built.body["tools"].as_array().unwrap();
        assert_eq!(tools.len(), 1);
        let declarations = tools[0]["functionDeclarations"].as_array().unwrap();
        assert_eq!(
            declarations
                .iter()
                .map(|d| d["name"].as_str().unwrap())
                .collect::<Vec<_>>(),
            ["read", crate::search::WEB_SEARCH]
        );
        assert_eq!(declarations[1], super::super::search::declaration());
        assert!(!built.body.to_string().contains("googleSearch"));
        let flagged = Rendering {
            search_only: true,
            ..rendering.clone()
        };
        let built = build_body(
            &make_req(&session, &messages, &[], &server_tools, &flagged),
            caps,
            TEST_ID,
            true,
        )
        .unwrap();
        assert_eq!(built.body["tools"], json!([{"googleSearch": {}}]));

        let mut wrong = session.clone();
        wrong.model = "claude-opus-5-5".into();
        let err = build_body(
            &make_req(&wrong, &messages, &[], &[], &rendering),
            None,
            TEST_ID,
            true,
        )
        .unwrap_err();
        assert_eq!(err.kind, ErrorKind::InvalidRequest);

        let gateway_ok = build_body(
            &make_req(&wrong, &messages, &[], &[], &rendering),
            None,
            TEST_ID,
            false,
        )
        .unwrap();
        assert_eq!(gateway_ok.body["contents"][0]["parts"][0]["text"], "search");
    }

    #[test]
    fn adjacent_tool_name_and_native_part_and_text_signature_preserve_append_only_history() {
        let session = gemini_session("gemini-4-carbon", Effort::High);
        let caps = super::super::model_capabilities(&session.model).copied();
        let rendering = Rendering::default();

        let turn1 = vec![
            Message::user_text("read then run"),
            Message {
                role: Role::Assistant,
                content: vec![
                    ContentBlock::ToolUse {
                        id: "gemini_call_1".into(),
                        name: "read_file".into(),
                        input: json!({"path": "a.rs"}),
                    },
                    ContentBlock::opaque(
                        "gemini",
                        json!({"type": "call_signatures", "identity": TEST_ID, "signatures": {"gemini_call_1": "sig_old"}}),
                    ),
                ],
            },
            Message {
                role: Role::User,
                content: vec![
                    ContentBlock::ToolResult {
                        tool_use_id: "gemini_call_1".into(),
                        content: vec![ContentBlock::Text {
                            text: "fn a() {}".into(),
                        }],
                        is_error: false,
                    },
                    ContentBlock::Text {
                        text: "early mail".into(),
                    },
                ],
            },
        ];
        let body1 = build_body(
            &make_req(&session, &turn1, &[], &[], &rendering),
            caps,
            TEST_ID,
            true,
        )
        .unwrap();

        let mut turn2 = turn1.clone();
        turn2.push(Message {
            role: Role::Assistant,
            content: vec![
                crate::provider::envelope(
                    "gemini",
                    &json!({
                        "type": "part",
                        "part": {"executableCode": {"language": "PYTHON", "code": "print(42)"}}
                    }),
                    0,
                    TEST_ID,
                ),
                crate::provider::envelope(
                    "gemini",
                    &json!({"type": "text_signature", "signature": "sig_txt_9"}),
                    1,
                    TEST_ID,
                ),
                ContentBlock::Text {
                    text: "running check".into(),
                },
                ContentBlock::ToolUse {
                    id: "gemini_call_1".into(),
                    name: "run_check".into(),
                    input: json!({"cmd": "ls"}),
                },
                ContentBlock::opaque(
                    "gemini",
                    json!({"type": "call_signatures", "identity": TEST_ID, "signatures": {}, "fold_text": true}),
                ),
            ],
        });
        turn2.push(Message {
            role: Role::User,
            content: vec![
                ContentBlock::ToolResult {
                    tool_use_id: "gemini_call_1".into(),
                    content: vec![ContentBlock::Text {
                        text: "Cargo.toml".into(),
                    }],
                    is_error: false,
                },
                ContentBlock::Text {
                    text: "steer".into(),
                },
                ContentBlock::Text {
                    text: "steer note".into(),
                },
                ContentBlock::Image {
                    source: ImageSource::Base64 {
                        media_type: ImageType::Png,
                        data: "iVBORw0KGgo=".into(),
                    },
                },
            ],
        });

        let body2 = build_body(
            &make_req(&session, &turn2, &[], &[], &rendering),
            caps,
            TEST_ID,
            true,
        )
        .unwrap();

        let c1 = body1.body["contents"].as_array().unwrap();
        let c2 = body2.body["contents"].as_array().unwrap();
        assert_eq!(&c2[..c1.len()], c1.as_slice());
        assert_eq!(c2[2]["parts"][0]["functionResponse"]["name"], "read_file");
        assert_eq!(c2[2]["parts"][1], json!({"text": "early mail"}));
        assert_eq!(c2[4]["parts"][0]["functionResponse"]["name"], "run_check");
        assert_eq!(
            c2[4]["parts"][0]["functionResponse"]["response"]["output"],
            "Cargo.toml\n\nsteer\n\nsteer note"
        );
        assert_eq!(c2[4]["parts"][1]["inlineData"]["mimeType"], "image/png");
        assert_eq!(c2[4]["parts"].as_array().unwrap().len(), 2);

        let mut turn3 = turn2.clone();
        turn3.push(Message {
            role: Role::Assistant,
            content: vec![ContentBlock::Text {
                text: "checked".into(),
            }],
        });
        turn3.push(Message::user_text("next"));
        let body3 = build_body(
            &make_req(&session, &turn3, &[], &[], &rendering),
            caps,
            TEST_ID,
            true,
        )
        .unwrap();
        let c3 = body3.body["contents"].as_array().unwrap();
        assert_eq!(&c3[..c2.len()], c2);

        for (history, body) in [(&turn1, &body1), (&turn2, &body2), (&turn3, &body3)] {
            let resumed: Vec<Message> = history
                .iter()
                .map(|message| {
                    let line =
                        serde_json::to_string(&LoggedMessage::from(message.clone())).unwrap();
                    serde_json::from_str::<LoggedMessage>(&line)
                        .unwrap()
                        .inline()
                        .unwrap()
                })
                .collect();
            let rebuilt = build_body(
                &make_req(&session, &resumed, &[], &[], &rendering),
                caps,
                TEST_ID,
                true,
            )
            .unwrap();
            assert_eq!(
                serde_json::to_vec(&rebuilt.body).unwrap(),
                serde_json::to_vec(&body.body).unwrap()
            );
        }
        assert_eq!(
            c2[3]["parts"][0],
            json!({"executableCode": {"language": "PYTHON", "code": "print(42)"}})
        );
        assert_eq!(
            c2[3]["parts"][1],
            json!({"text": "running check", "thoughtSignature": "sig_txt_9"})
        );

        let other_body = build_body(
            &make_req(&session, &turn2, &[], &[], &rendering),
            caps,
            "id_other_gemini_acct",
            true,
        )
        .unwrap();
        assert_eq!(
            other_body.body["contents"][3]["parts"][1],
            json!({"text": "running check"})
        );
    }

    #[test]
    fn trailing_zero_projection_text_signature_replays_without_thought_flag() {
        let session = gemini_session("gemini-4-carbon", Effort::High);
        let caps = super::super::model_capabilities(&session.model).copied();
        let rendering = Rendering::default();
        let messages = [
            Message::user_text("hi"),
            Message {
                role: Role::Assistant,
                content: vec![
                    ContentBlock::Text {
                        text: "Hello".into(),
                    },
                    crate::provider::envelope(
                        "gemini",
                        &json!({"type": "text_signature", "signature": "sig_end"}),
                        0,
                        TEST_ID,
                    ),
                ],
            },
            Message::user_text("follow up"),
        ];
        let built = build_body(
            &make_req(&session, &messages, &[], &[], &rendering),
            caps,
            TEST_ID,
            true,
        )
        .unwrap();
        let parts = built.body["contents"][1]["parts"].as_array().unwrap();
        assert_eq!(
            parts.as_slice(),
            [
                json!({"text": "Hello"}),
                json!({"text": "", "thoughtSignature": "sig_end"}),
            ]
        );
        assert!(parts[1].get("thought").is_none());

        let other = build_body(
            &make_req(&session, &messages, &[], &[], &rendering),
            caps,
            "id_other_gemini_acct",
            true,
        )
        .unwrap();
        assert_eq!(
            other.body["contents"][1]["parts"],
            json!([{"text": "Hello"}])
        );

        let signed_answer = [
            Message::user_text("hi"),
            Message {
                role: Role::Assistant,
                content: vec![
                    crate::provider::envelope(
                        super::super::PROVIDER,
                        &json!({
                            "type": REASONING_ITEM,
                            "thought": "plan",
                            "signature": "sig_plan",
                        }),
                        1,
                        TEST_ID,
                    ),
                    ContentBlock::Thinking {
                        thinking: "plan".into(),
                        signature: String::new(),
                    },
                    ContentBlock::Text {
                        text: "answer".into(),
                    },
                ],
            },
            Message::user_text("next"),
        ];
        let replayed = build_body(
            &make_req(&session, &signed_answer, &[], &[], &rendering),
            caps,
            TEST_ID,
            true,
        )
        .unwrap();
        assert_eq!(
            replayed.body["contents"][1]["parts"],
            json!([
                {"thought": true, "text": "plan", "thoughtSignature": "sig_plan"},
                {"text": "answer"},
            ])
        );
    }

    #[test]
    fn wire_print_detects_edits_rejects_invalid_requests_and_isolates_identity() {
        let session = gemini_session("gemini-4-carbon", Effort::High);
        let caps = super::super::model_capabilities(&session.model).copied();
        let rendering = Rendering::default();
        let base = [
            Message::user_text("first"),
            Message {
                role: Role::Assistant,
                content: vec![
                    crate::provider::envelope(
                        super::super::PROVIDER,
                        &json!({"type": REASONING_ITEM, "thought": "plan", "signature": "sig_a"}),
                        1,
                        TEST_ID,
                    ),
                    ContentBlock::Thinking {
                        thinking: "plan".into(),
                        signature: String::new(),
                    },
                    ContentBlock::Text {
                        text: "answer".into(),
                    },
                ],
            },
            Message::user_text("second"),
        ];
        let req = make_req(&session, &base, &[], &[], &rendering);
        let wp_a = wire_print(&req, caps, TEST_ID, true).unwrap();
        let body_a = build_body(&req, caps, TEST_ID, true).unwrap().body;
        assert_eq!(wp_a.messages, content_digests(&body_a));

        let mut edited = base.clone();
        edited[1].content[2] = ContentBlock::Text {
            text: "edited answer".into(),
        };
        let wp_edited = wire_print(
            &make_req(&session, &edited, &[], &[], &rendering),
            caps,
            TEST_ID,
            true,
        )
        .unwrap();
        assert_eq!(wp_edited.messages[0], wp_a.messages[0]);
        assert_ne!(wp_edited.messages[1], wp_a.messages[1]);
        assert_eq!(wp_edited.messages[2], wp_a.messages[2]);

        let merged = [
            Message {
                role: Role::System,
                content: vec![ContentBlock::Text {
                    text: "<delta>".into(),
                }],
            },
            Message::user_text("follow-up"),
        ];
        let merged_req = make_req(&session, &merged, &[], &[], &rendering);
        let wp_merged = wire_print(&merged_req, caps, TEST_ID, true).unwrap();
        let body_merged = build_body(&merged_req, caps, TEST_ID, true).unwrap().body;
        assert_eq!(wp_merged.messages.len(), 1);
        assert_eq!(wp_merged.messages, content_digests(&body_merged));

        let wp_other = wire_print(&req, caps, "id_gemini_account_b", true).unwrap();
        let body_other = build_body(&req, caps, "id_gemini_account_b", true)
            .unwrap()
            .body;
        assert_eq!(wp_other.messages, content_digests(&body_other));
        assert_ne!(wp_other.messages[1], wp_a.messages[1]);

        let mut debug_req = req;
        debug_req.options.debug_cache = true;
        assert!(wire_print(&debug_req, caps, TEST_ID, true).is_none());

        let bad_effort = gemini_session("gemini-4-carbon", Effort::Xhigh);
        assert!(
            wire_print(
                &make_req(&bad_effort, &base, &[], &[], &rendering),
                caps,
                TEST_ID,
                true
            )
            .is_none()
        );

        let wrong_model = gemini_session("claude-opus-5-5", Effort::High);
        assert!(
            wire_print(
                &make_req(&wrong_model, &base, &[], &[], &rendering),
                None,
                TEST_ID,
                true
            )
            .is_none()
        );

        let malformed = [Message {
            role: Role::Assistant,
            content: vec![crate::provider::envelope(
                super::super::PROVIDER,
                &json!({"type": REASONING_ITEM, "thought": "plan", "signature": "sig"}),
                5,
                TEST_ID,
            )],
        }];
        assert!(
            wire_print(
                &make_req(&session, &malformed, &[], &[], &rendering),
                caps,
                TEST_ID,
                true
            )
            .is_none()
        );
    }
}
