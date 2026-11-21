use std::collections::BTreeMap;
use std::time::Instant;

use serde_json::{Value, json};
use wisp_protocol::{
    Billing, CitedSource, ContentBlock, ErrorDetail, ErrorKind, FailurePhase, FailureSource,
    RetryClass, SearchAction, SearchHit, SearchOutcome, StopReason, StreamDiagnostics, Usage,
};

use crate::endpoints::ProviderKind;
use crate::models::ModelTable;

use super::super::sse::{Decoder, SseParser, broken};
use super::super::{Delta, ProviderError, ProviderEvent, REASONING_ITEM, SearchSignal};
use super::{CALL_SIGNATURES_ITEM, FOLD_TEXT, PART_ITEM, TEXT_SIGNATURE_ITEM};

const MAX_DIAGNOSTIC_KEYS: usize = 16;
const MAX_DIAGNOSTIC_KEY_BYTES: usize = 64;

#[derive(Debug, Clone, PartialEq, Eq)]
enum ActiveBlock {
    Text(usize),
    Thinking {
        env_index: usize,
        think_index: usize,
        thinking: String,
        signature: String,
    },
}

pub(super) struct GeminiDecoder {
    model: String,
    diagnostics: StreamDiagnostics,
    opened_at: Instant,
    billing: Billing,
    bill_tool_use_prompt: bool,
    request_id: Option<String>,
    identity: String,
    uid: String,
    sse: SseParser,
    started: bool,
    finished: bool,
    next_index: usize,
    active: Option<ActiveBlock>,
    call_seq: u64,
    search_id: Option<String>,
    search_hits: Vec<SearchHit>,
    call_signatures: BTreeMap<String, String>,
    web_search_requests: u64,
    said: bool,
    usage: Usage,
}

impl GeminiDecoder {
    pub(super) fn new(
        model: String,
        billing: Billing,
        request_id: Option<String>,
        identity: String,
    ) -> Self {
        let bill_tool_use_prompt = ModelTable::shared().find(&model).is_some_and(|row| {
            row.provider == Some(ProviderKind::Gemini) && row.bill_tool_use_prompt
        });
        Self {
            model,
            diagnostics: StreamDiagnostics::default(),
            opened_at: Instant::now(),
            billing,
            bill_tool_use_prompt,
            request_id,
            identity,
            uid: uuid::Uuid::new_v4().simple().to_string(),
            sse: SseParser::default(),
            started: false,
            finished: false,
            next_index: 0,
            active: None,
            call_seq: 0,
            search_id: None,
            search_hits: Vec::new(),
            call_signatures: BTreeMap::new(),
            web_search_requests: 0,
            said: false,
            usage: Usage {
                billing,
                ..Usage::default()
            },
        }
    }

    fn record_wire(&mut self, bytes: usize) {
        if bytes == 0 {
            return;
        }
        self.diagnostics.bytes = self
            .diagnostics
            .bytes
            .saturating_add(u64::try_from(bytes).unwrap_or(u64::MAX));
        let elapsed = u64::try_from(self.opened_at.elapsed().as_millis()).unwrap_or(u64::MAX);
        self.diagnostics.first_byte_ms.get_or_insert(elapsed);
        self.diagnostics.last_byte_ms = Some(elapsed);
    }

    fn record_chunk(&mut self, root: &Value) {
        let mut keys: Vec<_> = root
            .as_object()
            .into_iter()
            .flat_map(|object| object.keys())
            .chain(
                root.get("response")
                    .and_then(Value::as_object)
                    .into_iter()
                    .flat_map(|object| object.keys()),
            )
            .filter(|key| {
                !key.is_empty()
                    && key.len() <= MAX_DIAGNOSTIC_KEY_BYTES
                    && key.bytes().enumerate().all(|(index, byte)| {
                        byte == b'_'
                            || byte.is_ascii_alphabetic()
                            || (index > 0 && byte.is_ascii_digit())
                    })
            })
            .collect();
        keys.sort_unstable();
        keys.dedup();
        keys.truncate(MAX_DIAGNOSTIC_KEYS);
        self.diagnostics.last_event_keys = keys.into_iter().cloned().collect();
        let payload = root.get("response").unwrap_or(root);
        for candidate in payload
            .get("candidates")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
        {
            for part in candidate
                .pointer("/content/parts")
                .and_then(Value::as_array)
                .into_iter()
                .flatten()
            {
                let count = if part.get("functionCall").is_some() {
                    &mut self.diagnostics.tool_use_parts
                } else if part.get("thought").and_then(Value::as_bool) == Some(true) {
                    &mut self.diagnostics.thinking_parts
                } else if part.get("text").and_then(Value::as_str).is_some() {
                    &mut self.diagnostics.text_parts
                } else {
                    continue;
                };
                *count = count.saturating_add(1);
            }
        }
    }

    fn alloc_index(&mut self) -> usize {
        let index = self.next_index;
        self.next_index = self.next_index.saturating_add(1);
        index
    }

    fn close_active(&mut self, events: &mut Vec<ProviderEvent>) {
        if let Some(active) = self.active.take() {
            match active {
                ActiveBlock::Text(index) => {
                    events.push(ProviderEvent::BlockStop { index });
                }
                ActiveBlock::Thinking {
                    env_index,
                    think_index,
                    thinking,
                    signature,
                } => {
                    let item = json!({
                        "type": REASONING_ITEM,
                        "thought": thinking,
                        "signature": signature,
                    });
                    events.push(ProviderEvent::BlockStart {
                        index: env_index,
                        block: super::super::envelope(super::PROVIDER, &item, 1, &self.identity),
                    });
                    events.push(ProviderEvent::BlockStop { index: env_index });
                    events.push(ProviderEvent::BlockStop { index: think_index });
                }
            }
        }
    }

    fn open_text(&mut self, events: &mut Vec<ProviderEvent>) -> usize {
        if let Some(ActiveBlock::Text(index)) = self.active {
            return index;
        }
        self.close_active(events);
        let index = self.alloc_index();
        self.active = Some(ActiveBlock::Text(index));
        events.push(ProviderEvent::BlockStart {
            index,
            block: ContentBlock::Text {
                text: String::new(),
            },
        });
        index
    }

    fn open_thinking(&mut self, events: &mut Vec<ProviderEvent>) -> usize {
        if let Some(ActiveBlock::Thinking { think_index, .. }) = self.active {
            return think_index;
        }
        self.close_active(events);
        let env_index = self.alloc_index();
        let think_index = self.alloc_index();
        self.active = Some(ActiveBlock::Thinking {
            env_index,
            think_index,
            thinking: String::new(),
            signature: String::new(),
        });
        events.push(ProviderEvent::BlockStart {
            index: think_index,
            block: ContentBlock::Thinking {
                thinking: String::new(),
                signature: String::new(),
            },
        });
        think_index
    }

    fn append_thinking(&mut self, text: &str, sig: Option<&str>, events: &mut Vec<ProviderEvent>) {
        if matches!(
            &self.active,
            Some(ActiveBlock::Thinking { signature, .. }) if !signature.is_empty()
        ) {
            self.close_active(events);
        }
        let index = self.open_thinking(events);
        if let Some(ActiveBlock::Thinking {
            thinking,
            signature,
            ..
        }) = &mut self.active
        {
            thinking.push_str(text);
            if let Some(sig) = sig {
                *signature = sig.to_owned();
            }
        }
        if !text.is_empty() {
            events.push(ProviderEvent::BlockDelta {
                index,
                delta: Delta::Thinking(text.to_owned()),
            });
        }
    }

    fn current_search_index(&self) -> usize {
        match &self.active {
            Some(ActiveBlock::Text(i)) => *i,
            Some(ActiveBlock::Thinking { think_index, .. }) => *think_index,
            None => self.next_index,
        }
    }

    fn apply_part(&mut self, part: &Value, events: &mut Vec<ProviderEvent>) {
        let thought_sig = part
            .get("thoughtSignature")
            .and_then(Value::as_str)
            .filter(|s| !s.is_empty());
        self.said |= thought_sig.is_some()
            || !only_text_fields(part)
            || part
                .get("text")
                .and_then(Value::as_str)
                .is_some_and(|text| !text.trim().is_empty());
        if let Some(fc) = part.get("functionCall") {
            self.close_active(events);
            self.call_seq = self.call_seq.saturating_add(1);
            let call_id = fc
                .get("id")
                .and_then(Value::as_str)
                .filter(|s| !s.is_empty())
                .map_or_else(
                    || format!("gemini_call_{}_{}", self.uid, self.call_seq),
                    str::to_owned,
                );
            let name = fc
                .get("name")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_owned();
            let args = fc.get("args").cloned().unwrap_or_else(|| json!({}));
            if let Some(sig) = thought_sig {
                self.call_signatures.insert(call_id.clone(), sig.to_owned());
            }
            let index = self.alloc_index();
            events.push(ProviderEvent::BlockStart {
                index,
                block: ContentBlock::ToolUse {
                    id: call_id,
                    name,
                    input: json!({}),
                },
            });
            events.push(ProviderEvent::BlockDelta {
                index,
                delta: Delta::InputJson(args.to_string()),
            });
            events.push(ProviderEvent::BlockStop { index });
            return;
        }

        if !only_text_fields(part) {
            self.close_active(events);
            let index = self.alloc_index();
            let item = json!({
                "type": PART_ITEM,
                "part": part,
            });
            events.push(ProviderEvent::BlockStart {
                index,
                block: super::super::envelope(super::PROVIDER, &item, 0, &self.identity),
            });
            events.push(ProviderEvent::BlockStop { index });
            return;
        }

        let is_thought = part
            .get("thought")
            .and_then(Value::as_bool)
            .unwrap_or(false);
        let text = part.get("text").and_then(Value::as_str).unwrap_or_default();

        if is_thought {
            self.append_thinking(text, thought_sig, events);
            return;
        }

        if text.is_empty()
            && let Some(sig) = thought_sig
        {
            self.close_active(events);
            let index = self.alloc_index();
            let item = json!({
                "type": TEXT_SIGNATURE_ITEM,
                "signature": sig,
            });
            events.push(ProviderEvent::BlockStart {
                index,
                block: super::super::envelope(super::PROVIDER, &item, 0, &self.identity),
            });
            events.push(ProviderEvent::BlockStop { index });
            return;
        }

        if !text.is_empty() {
            if let Some(sig) = thought_sig {
                self.close_active(events);
                let env_index = self.alloc_index();
                let text_index = self.alloc_index();
                let item = json!({
                    "type": TEXT_SIGNATURE_ITEM,
                    "signature": sig,
                });
                events.push(ProviderEvent::BlockStart {
                    index: env_index,
                    block: super::super::envelope(super::PROVIDER, &item, 1, &self.identity),
                });
                events.push(ProviderEvent::BlockStop { index: env_index });
                events.push(ProviderEvent::BlockStart {
                    index: text_index,
                    block: ContentBlock::Text {
                        text: String::new(),
                    },
                });
                events.push(ProviderEvent::BlockDelta {
                    index: text_index,
                    delta: Delta::Text(text.to_owned()),
                });
                events.push(ProviderEvent::BlockStop { index: text_index });
                return;
            }
            let index = self.open_text(events);
            events.push(ProviderEvent::BlockDelta {
                index,
                delta: Delta::Text(text.to_owned()),
            });
        }
    }

    fn apply_grounding(&mut self, grounding: &Value, events: &mut Vec<ProviderEvent>) {
        let queries: Vec<String> = grounding
            .get("webSearchQueries")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
            .filter_map(Value::as_str)
            .map(str::trim)
            .filter(|q| !q.is_empty())
            .map(str::to_owned)
            .collect();
        let hits: Vec<SearchHit> = grounding
            .get("groundingChunks")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
            .filter_map(|chunk| {
                let web = chunk.get("web")?;
                let url = web.get("uri").and_then(Value::as_str)?.trim();
                if url.is_empty() {
                    return None;
                }
                let title = web
                    .get("title")
                    .and_then(Value::as_str)
                    .unwrap_or(url)
                    .trim()
                    .to_owned();
                Some(SearchHit {
                    title,
                    url: url.to_owned(),
                    snippet: None,
                    page_age: None,
                })
            })
            .collect();

        if self.search_id.is_none()
            && let Some(query) = queries.first()
        {
            let call_id = format!("gemini_search_{}", self.uid);
            events.push(ProviderEvent::Search {
                index: self.current_search_index(),
                signal: SearchSignal::Started {
                    call_id: call_id.clone(),
                    query: query.clone(),
                    action: SearchAction::Search,
                },
            });
            self.search_id = Some(call_id);
        }
        for hit in hits {
            if !self.search_hits.iter().any(|seen| seen.url == hit.url) {
                self.search_hits.push(hit);
            }
        }
        let count = u64::try_from(queries.len()).unwrap_or(u64::MAX);
        if count > self.web_search_requests {
            self.web_search_requests = count;
            self.usage.web_search_requests = count;
            events.push(ProviderEvent::MessageDelta {
                stop_reason: None,
                stop_details: None,
                usage: self.usage,
                iterations: Vec::new(),
                diagnostics: None,
            });
        }
    }

    fn apply_usage(&mut self, usage: &Value) {
        let prompt = usage
            .get("promptTokenCount")
            .and_then(Value::as_u64)
            .unwrap_or(0);
        let cached = usage
            .get("cachedContentTokenCount")
            .and_then(Value::as_u64)
            .unwrap_or(0)
            .min(prompt);
        let candidates = usage
            .get("candidatesTokenCount")
            .and_then(Value::as_u64)
            .unwrap_or(0);
        let thoughts = usage
            .get("thoughtsTokenCount")
            .and_then(Value::as_u64)
            .unwrap_or(0);
        let tool_prompt = usage
            .get("toolUsePromptTokenCount")
            .filter(|_| self.bill_tool_use_prompt)
            .and_then(Value::as_u64)
            .unwrap_or(0);
        self.usage = Usage {
            input_tokens: prompt.saturating_sub(cached).saturating_add(tool_prompt),
            output_tokens: candidates.saturating_add(thoughts),
            cache_read_input_tokens: cached,
            reasoning_tokens: thoughts,
            web_search_requests: self.web_search_requests,
            billing: self.billing,
            ..Usage::default()
        };
    }

    fn apply_chunk(
        &mut self,
        root: Value,
        events: &mut Vec<ProviderEvent>,
    ) -> Result<(), ProviderError> {
        if self.finished {
            return Ok(());
        }

        if let Some(google) = super::parse_google_error(&root, None) {
            let kind = google.kind.unwrap_or(ErrorKind::Server);
            let retry = super::super::classify::retry_for(kind, google.retry_ms);
            let mut detail = ErrorDetail::new(FailurePhase::Stream, FailureSource::Endpoint, retry);
            detail.status = google.code.or(Some(200));
            detail.error_type = google.reason.or(google.grpc_status);
            detail.possibly_billed = true;
            let message = google
                .message
                .as_deref()
                .and_then(super::clean_error_message)
                .unwrap_or_else(|| "Gemini stream reported an error".to_owned());
            return Err(ProviderError::classified(kind, message, detail));
        }

        let payload = root.get("response").unwrap_or(&root);

        if !self.started {
            self.started = true;
            let id = payload
                .get("responseId")
                .or_else(|| root.get("responseId"))
                .and_then(Value::as_str)
                .map(str::to_owned);
            if self.request_id.is_none() {
                self.request_id = id.clone();
            }
            let model = payload
                .get("modelVersion")
                .and_then(Value::as_str)
                .map(|m| m.strip_prefix("models/").unwrap_or(m))
                .unwrap_or(&self.model)
                .to_owned();
            events.push(ProviderEvent::MessageStart {
                id,
                model,
                usage: Usage {
                    billing: self.billing,
                    ..Usage::default()
                },
                iterations: Vec::new(),
                diagnostics: None,
            });
        }

        let candidate = payload
            .get("candidates")
            .and_then(Value::as_array)
            .and_then(|arr| arr.first());

        if let Some(parts) = candidate
            .and_then(|c| c.get("content"))
            .and_then(|c| c.get("parts"))
            .and_then(Value::as_array)
        {
            for part in parts {
                self.apply_part(part, events);
            }
        }

        if let Some(usage) = payload.get("usageMetadata") {
            self.apply_usage(usage);
        }

        if let Some(grounding) = candidate.and_then(|c| c.get("groundingMetadata")) {
            self.apply_grounding(grounding, events);
        }

        let candidate_finish = candidate
            .and_then(|c| c.get("finishReason"))
            .and_then(Value::as_str);
        let prompt_blocked = payload
            .get("promptFeedback")
            .and_then(|pf| pf.get("blockReason"))
            .and_then(Value::as_str)
            .filter(|s| !s.is_empty());

        if let Some(mut error) = candidate_finish.and_then(|reason| self.invalid_finish(reason)) {
            error.usage_complete = payload.get("usageMetadata").is_some();
            self.close_active(events);
            events.push(ProviderEvent::MessageDelta {
                stop_reason: None,
                stop_details: None,
                usage: self.usage,
                iterations: Vec::new(),
                diagnostics: None,
            });
            return Err(error);
        }

        if let Some(stop_reason) = candidate_finish
            .map(|reason| {
                if self.call_seq > 0 && reason == "STOP" {
                    StopReason::ToolUse
                } else {
                    map_stop_reason(reason)
                }
            })
            .or_else(|| prompt_blocked.map(|_| StopReason::Refusal))
        {
            self.close_active(events);
            if self.call_seq > 0 {
                let index = self.alloc_index();
                let raw = json!({
                    "type": CALL_SIGNATURES_ITEM,
                    "identity": self.identity,
                    "signatures": self.call_signatures,
                    FOLD_TEXT: true,
                });
                events.push(ProviderEvent::BlockStart {
                    index,
                    block: ContentBlock::opaque(super::PROVIDER, raw),
                });
                events.push(ProviderEvent::BlockStop { index });
            }
            if let Some(call_id) = self.search_id.take() {
                let hits = std::mem::take(&mut self.search_hits);
                let sources = hits
                    .iter()
                    .map(|hit| CitedSource {
                        url: hit.url.clone(),
                        title: Some(hit.title.clone()),
                    })
                    .collect();
                events.push(ProviderEvent::Search {
                    index: self.next_index,
                    signal: SearchSignal::Ended {
                        call_id,
                        outcome: SearchOutcome::Results { hits },
                    },
                });
                events.push(ProviderEvent::Search {
                    index: self.next_index,
                    signal: SearchSignal::Cited { sources },
                });
            }
            events.push(ProviderEvent::MessageDelta {
                stop_reason: Some(stop_reason),
                stop_details: None,
                usage: self.usage,
                iterations: Vec::new(),
                diagnostics: None,
            });
            events.push(ProviderEvent::MessageStop);
            self.finished = true;
        }

        Ok(())
    }

    fn invalid_finish(&self, reason: &str) -> Option<ProviderError> {
        let message = match reason {
            "MALFORMED_FUNCTION_CALL" => "Gemini returned a malformed tool call",
            "UNEXPECTED_TOOL_CALL" => "Gemini returned an unexpected tool call",
            "STOP" if !self.said && self.search_id.is_none() => "Gemini returned an empty reply",
            _ => return None,
        };
        let mut detail = ErrorDetail::new(
            FailurePhase::Stream,
            FailureSource::Endpoint,
            RetryClass::Transient,
        );
        detail.status = Some(200);
        detail.error_type = Some(reason.to_owned());
        detail.possibly_billed = true;
        Some(ProviderError::classified(
            ErrorKind::InvalidResponse,
            message,
            detail,
        ))
    }
}

fn only_text_fields(part: &Value) -> bool {
    part.as_object().is_some_and(|obj| {
        obj.keys()
            .all(|k| matches!(k.as_str(), "text" | "thought" | "thoughtSignature"))
    })
}

fn map_stop_reason(reason: &str) -> StopReason {
    match reason {
        "STOP" => StopReason::EndTurn,
        "MAX_TOKENS" => StopReason::MaxTokens,
        "SAFETY" | "RECITATION" | "BLOCKLIST" | "PROHIBITED_CONTENT" | "SPII" | "IMAGE_SAFETY" => {
            StopReason::Refusal
        }
        other => StopReason::Other(other.to_ascii_lowercase()),
    }
}

impl Decoder for GeminiDecoder {
    fn push(&mut self, chunk: &[u8]) -> Vec<Result<ProviderEvent, ProviderError>> {
        self.record_wire(chunk.len());
        let mut out = Vec::new();
        for event in self.sse.push(chunk) {
            let Ok(event) = event else {
                out.push(Err(ProviderError::malformed("Gemini event is too large")));
                return out;
            };
            self.diagnostics.events = self.diagnostics.events.saturating_add(1);
            let trimmed = event.data.trim();
            if trimmed.is_empty() || trimmed == "[DONE]" {
                continue;
            }
            let Ok(value) = serde_json::from_str::<Value>(trimmed) else {
                out.push(Err(ProviderError::malformed(
                    "Gemini returned malformed event JSON",
                )));
                return out;
            };
            self.record_chunk(&value);
            let mut events = Vec::new();
            let result = self.apply_chunk(value, &mut events);
            out.extend(events.into_iter().map(Ok));
            if let Err(error) = result {
                out.push(Err(error));
                return out;
            }
        }
        out
    }

    fn finish(&self) -> Option<ProviderError> {
        (!self.finished || self.sse.has_partial()).then(|| {
            let mut error = broken(
                ErrorKind::Network,
                "Gemini stream ended before finishReason",
            );
            error.detail.stream = Some(Box::new(StreamDiagnostics {
                partial_event: self.sse.has_partial(),
                stop_reason_seen: self.finished,
                ..self.diagnostics.clone()
            }));
            error
        })
    }

    fn request_id(&self) -> Option<String> {
        self.request_id.clone()
    }

    fn ended(&self) -> bool {
        self.finished
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::provider::Assembler;
    use wisp_protocol::RetryClass;

    fn sse_frame(value: &Value) -> Vec<u8> {
        format!("data: {value}\n\n").into_bytes()
    }

    #[test]
    fn streams_thinking_signature_and_text_into_assembled_message() {
        let mut decoder = GeminiDecoder::new(
            "gemini-3.1-pro-preview".into(),
            Billing::Metered,
            None,
            "id_gem_1".into(),
        );
        let c1 = sse_frame(&json!({
            "responseId": "resp_1",
            "modelVersion": "models/gemini-3.1-pro-preview",
            "candidates": [{
                "content": {
                    "role": "model",
                    "parts": [{"thought": true, "text": "Let me think..."}]
                }
            }]
        }));
        let c2 = sse_frame(&json!({
            "candidates": [{
                "content": {
                    "role": "model",
                    "parts": [
                        {"thought": true, "text": " done.", "thoughtSignature": "sig_thought_1"},
                        {"text": "Hello world!"}
                    ]
                },
                "finishReason": "STOP"
            }],
            "usageMetadata": {
                "promptTokenCount": 100,
                "candidatesTokenCount": 12,
                "thoughtsTokenCount": 8
            }
        }));

        let mut assembler = Assembler::default();
        for chunk in [c1, c2] {
            for item in decoder.push(&chunk) {
                assembler.apply(item.unwrap()).unwrap();
            }
        }
        assert!(decoder.finish().is_none());
        let msg = assembler.finish().unwrap();
        assert_eq!(msg.model, "gemini-3.1-pro-preview");
        assert_eq!(msg.stop_reason, StopReason::EndTurn);
        assert_eq!(msg.content.len(), 3);
        assert_eq!(
            msg.content[0],
            crate::provider::envelope(
                "gemini",
                &json!({
                    "type": "reasoning",
                    "thought": "Let me think... done.",
                    "signature": "sig_thought_1",
                }),
                1,
                "id_gem_1",
            )
        );
        assert_eq!(
            msg.content[1],
            ContentBlock::Thinking {
                thinking: "Let me think... done.".into(),
                signature: String::new(),
            }
        );
        assert_eq!(
            msg.content[2],
            ContentBlock::Text {
                text: "Hello world!".into(),
            }
        );
        assert_eq!(msg.usage.input_tokens, 100);
        assert_eq!(msg.usage.output_tokens, 20);
        assert_eq!(msg.usage.reasoning_tokens, 8);
    }

    #[test]
    fn unwraps_antigravity_response_envelope_transparently() {
        let mut decoder = GeminiDecoder::new(
            "gemini-3-flash-preview".into(),
            Billing::AntigravityPlan,
            None,
            "id_ag_1".into(),
        );
        let frame = sse_frame(&json!({
            "response": {
                "responseId": "ag_resp_1",
                "modelVersion": "models/gemini-3-flash-preview",
                "candidates": [{
                    "content": {
                        "role": "model",
                        "parts": [{"text": "alpha"}]
                    },
                    "finishReason": "STOP"
                }],
                "usageMetadata": {
                    "promptTokenCount": 50,
                    "candidatesTokenCount": 5
                }
            }
        }));

        let mut assembler = Assembler::default();
        let mut completions = 0;
        for item in decoder.push(&frame) {
            let event = item.unwrap();
            completions += usize::from(matches!(event, ProviderEvent::MessageStop));
            assembler.apply(event).unwrap();
        }
        assert_eq!(completions, 1);
        assert!(decoder.ended());
        assert!(decoder.finish().is_none());
        let msg = assembler.finish().unwrap();
        assert_eq!(msg.model, "gemini-3-flash-preview");
        assert_eq!(msg.usage.billing, Billing::AntigravityPlan);
        assert_eq!(msg.stop_reason, StopReason::EndTurn);
        assert_eq!(
            msg.content,
            vec![ContentBlock::Text {
                text: "alpha".into()
            }]
        );
    }

    #[test]
    fn parallel_function_calls_record_trailing_call_signatures_opaque_block() {
        let mut decoder = GeminiDecoder::new(
            "gemini-3.1-pro-preview".into(),
            Billing::Metered,
            None,
            "id_gem_calls".into(),
        );
        let frame = sse_frame(&json!({
            "candidates": [{
                "content": {
                    "role": "model",
                    "parts": [
                        {
                            "functionCall": {"name": "read_file", "args": {"path": "a.rs"}},
                            "thoughtSignature": "sig_call_1"
                        },
                        {
                            "functionCall": {"id": "explicit_2", "name": "read_file", "args": {"path": "b.rs"}},
                            "thoughtSignature": "sig_call_2"
                        }
                    ]
                },
                "finishReason": "STOP"
            }]
        }));

        let mut assembler = Assembler::default();
        for item in decoder.push(&frame) {
            assembler.apply(item.unwrap()).unwrap();
        }
        let msg = assembler.finish().unwrap();
        assert_eq!(msg.stop_reason, StopReason::ToolUse);
        assert_eq!(msg.content.len(), 3);
        let ContentBlock::ToolUse {
            id: ref first_id,
            ref name,
            ref input,
        } = msg.content[0]
        else {
            panic!("expected ToolUse");
        };
        assert!(first_id.starts_with("gemini_call_"), "{first_id}");
        assert!(first_id.ends_with("_1"), "{first_id}");
        assert_eq!(name, "read_file");
        assert_eq!(input, &json!({"path": "a.rs"}));
        assert_eq!(
            msg.content[1],
            ContentBlock::ToolUse {
                id: "explicit_2".into(),
                name: "read_file".into(),
                input: json!({"path": "b.rs"}),
            }
        );
        assert_eq!(
            msg.content[2],
            ContentBlock::opaque(
                "gemini",
                json!({
                    "type": "call_signatures",
                    "identity": "id_gem_calls",
                    "signatures": {
                        first_id.clone(): "sig_call_1",
                        "explicit_2": "sig_call_2",
                    },
                    "fold_text": true,
                })
            )
        );

        let mut second_decoder = GeminiDecoder::new(
            "gemini-3.1-pro-preview".into(),
            Billing::Metered,
            None,
            "id_gem_calls".into(),
        );
        let mut second_assembler = Assembler::default();
        for item in second_decoder.push(&frame) {
            second_assembler.apply(item.unwrap()).unwrap();
        }
        let second_msg = second_assembler.finish().unwrap();
        let ContentBlock::ToolUse {
            id: ref second_id, ..
        } = second_msg.content[0]
        else {
            panic!("expected second ToolUse");
        };
        assert_ne!(first_id, second_id);

        let mut unsigned = GeminiDecoder::new(
            "gemini-3.1-pro-preview".into(),
            Billing::Metered,
            None,
            "id_gem_calls".into(),
        );
        let frame = sse_frame(&json!({
            "candidates": [{
                "content": {"role": "model", "parts": [
                    {"functionCall": {"id": "plain_1", "name": "read_file", "args": {}}}
                ]},
                "finishReason": "STOP"
            }]
        }));
        let mut assembler = Assembler::default();
        for item in unsigned.push(&frame) {
            assembler.apply(item.unwrap()).unwrap();
        }
        let msg = assembler.finish().unwrap();
        assert_eq!(msg.content.len(), 2);
        assert_eq!(
            msg.content[1],
            ContentBlock::opaque(
                "gemini",
                json!({"type": "call_signatures", "identity": "id_gem_calls", "signatures": {}, "fold_text": true})
            )
        );
    }

    #[test]
    fn grounding_metadata_emits_one_search_with_joined_pages_sources_and_early_usage() {
        for queries in [false, true] {
            let mut decoder = GeminiDecoder::new(
                "gemini-3.1-pro-preview".into(),
                Billing::Metered,
                None,
                "id_gem_search".into(),
            );
            let mut events = Vec::new();
            for (count, pages, finish) in [
                (1, vec!["rust"], Value::Null),
                (2, vec!["rust", "gemini"], json!("STOP")),
            ] {
                let frame = json!({
                    "candidates": [{
                        "content": {"role": "model", "parts": [{"text": "Grounded answer"}]},
                        "groundingMetadata": {
                            "webSearchQueries": if queries { ["rust async trait", "gemini grounding"][..count].to_vec() } else { Vec::new() },
                            "groundingChunks": pages.iter().map(|page| json!({"web": {"uri": format!("https://example.com/{page}"), "title": page}})).collect::<Vec<_>>(),
                        }, "finishReason": finish,
                    }],
                    "usageMetadata": {"promptTokenCount": 80, "candidatesTokenCount": 15},
                });
                events.extend(
                    decoder
                        .push(&sse_frame(&frame))
                        .into_iter()
                        .map(Result::unwrap),
                );
                if count == 1 {
                    assert_eq!(events.iter().any(|event| matches!(event, ProviderEvent::MessageDelta { stop_reason: None, usage, .. } if usage.web_search_requests == 1)), queries);
                    assert!(!events.iter().any(|event| matches!(
                        event,
                        ProviderEvent::Search {
                            signal: SearchSignal::Ended { .. } | SearchSignal::Cited { .. },
                            ..
                        }
                    )));
                }
            }
            let signals: Vec<_> = events
                .iter()
                .filter_map(|event| match event {
                    ProviderEvent::Search { signal, .. } => Some(signal),
                    _ => None,
                })
                .collect();
            if queries {
                let [
                    SearchSignal::Started {
                        call_id: start,
                        query,
                        ..
                    },
                    SearchSignal::Ended {
                        call_id: end,
                        outcome: SearchOutcome::Results { hits },
                    },
                    SearchSignal::Cited { sources },
                ] = signals.as_slice()
                else {
                    panic!("one completed search and its sources expected")
                };
                assert_eq!(query, "rust async trait");
                assert_eq!(start, end);
                assert_eq!(
                    hits.iter().map(|hit| hit.url.as_str()).collect::<Vec<_>>(),
                    ["https://example.com/rust", "https://example.com/gemini"]
                );
                assert_eq!(
                    sources
                        .iter()
                        .map(|source| source.url.as_str())
                        .collect::<Vec<_>>(),
                    hits.iter().map(|hit| hit.url.as_str()).collect::<Vec<_>>()
                );
                let counts: Vec<_> = events
                    .iter()
                    .filter_map(|event| match event {
                        ProviderEvent::MessageDelta {
                            stop_reason: None,
                            usage,
                            ..
                        } => Some(usage.web_search_requests),
                        _ => None,
                    })
                    .collect();
                assert_eq!(counts, [1, 2]);
            } else {
                assert!(signals.is_empty());
            }
            let mut assembler = Assembler::default();
            for event in events {
                assembler.apply(event).unwrap();
            }
            assert_eq!(
                assembler.finish().unwrap().usage.web_search_requests,
                if queries { 2 } else { 0 }
            );
        }
    }

    #[test]
    fn cached_and_model_billed_tool_prompt_tokens_preserve_billing() {
        for billing in [Billing::Metered, Billing::AntigravityPlan] {
            for (model, tool_tokens, prompt, cached, input) in [
                ("gemini-4-carbon", json!(700), 5000_u64, 4200_u64, 1500),
                ("gemini-4-argon-low", json!(700), 5000, 4200, 1500),
                ("gemini-3.8-flash", json!(700), 5000, 4200, 800),
                ("gemini-2.5-flash-thinking", json!(700), 5000, 4200, 800),
                ("unknown-model", json!(700), 5000, 4200, 800),
                ("gemini-4-carbon", Value::Null, 5000, 4200, 800),
                ("gemini-4-carbon", json!("700"), 5000, 4200, 800),
                ("gemini-4-carbon", json!(700), 100, 200, 700),
                ("gemini-4-carbon", json!(u64::MAX), 5000, 4200, u64::MAX),
            ] {
                let mut decoder = GeminiDecoder::new(
                    model.into(),
                    billing,
                    Some("req_header_1".into()),
                    "id_gem_cache".into(),
                );
                let mut usage = json!({
                    "promptTokenCount": prompt,
                    "cachedContentTokenCount": cached,
                    "candidatesTokenCount": 100,
                    "thoughtsTokenCount": 50
                });
                if !tool_tokens.is_null() {
                    usage["toolUsePromptTokenCount"] = tool_tokens;
                }
                let frame = sse_frame(&json!({
                    "candidates": [{
                        "content": {"role": "model", "parts": [{"text": "Cached"}]},
                        "finishReason": "STOP"
                    }],
                    "usageMetadata": usage
                }));

                let mut assembler = Assembler::default();
                for item in decoder.push(&frame) {
                    assembler.apply(item.unwrap()).unwrap();
                }
                let msg = assembler.finish().unwrap();
                assert_eq!(msg.usage.input_tokens, input, "{model}: {usage}");
                assert_eq!(msg.usage.cache_read_input_tokens, cached.min(prompt));
                assert_eq!(msg.usage.output_tokens, 150);
                assert_eq!(msg.usage.reasoning_tokens, 50);
                assert_eq!(msg.usage.billing, billing);
                assert_eq!(decoder.request_id().as_deref(), Some("req_header_1"));
            }
        }
    }

    #[test]
    fn truncated_stream_without_finish_reason_returns_transient_stream_error() {
        for wrapped in [false, true] {
            for (cut, empty) in [(false, false), (true, false), (false, true)] {
                let mut decoder = GeminiDecoder::new(
                    "gemini-3.1-pro-preview".into(),
                    Billing::Metered,
                    None,
                    "id_gem_trunc".into(),
                );
                let payload = json!({
                    "candidates": [{"content": {"parts": [
                        {"text": "[text placeholder]"},
                        {"thought": true, "text": "[thought placeholder]", "thoughtSignature": "[signature placeholder]"},
                        {"functionCall": {"name": "placeholder", "args": {"nested_argument": "[argument placeholder]"}}}
                    ]}}, {"content": {"parts": [{"text": "[other candidate placeholder]"}]}}]
                });
                let mut wire = Vec::new();
                if !empty {
                    for payload in [payload, json!({"usageMetadata": {"promptTokenCount": 1}})] {
                        let root = if wrapped {
                            json!({"response": payload})
                        } else {
                            payload
                        };
                        wire.extend(sse_frame(&root));
                    }
                    if cut {
                        wire.extend_from_slice(b"data: {\"unfinished\":");
                    }
                }
                let split = wire.len() / 2;
                for chunk in [&wire[..split], &wire[split..]] {
                    assert!(decoder.push(chunk).iter().all(Result::is_ok));
                }
                let err = decoder.finish().expect("truncated stream must fail");
                assert_eq!(err.kind, ErrorKind::Network);
                assert_eq!(err.detail.phase, FailurePhase::Stream);
                assert_eq!(err.detail.retry, RetryClass::Transient);
                assert!(err.possibly_billed);
                assert_eq!(err.message, "Gemini stream ended before finishReason");
                let diagnostics = *err.detail.stream.unwrap();
                let last_event_keys = if empty {
                    Vec::new()
                } else if wrapped {
                    vec!["response".to_owned(), "usageMetadata".to_owned()]
                } else {
                    vec!["usageMetadata".to_owned()]
                };
                assert_eq!(
                    diagnostics,
                    StreamDiagnostics {
                        bytes: u64::try_from(wire.len()).unwrap(),
                        events: if empty { 0 } else { 2 },
                        text_parts: if empty { 0 } else { 2 },
                        thinking_parts: u64::from(!empty),
                        tool_use_parts: u64::from(!empty),
                        last_event_keys,
                        partial_event: cut,
                        stop_reason_seen: false,
                        first_byte_ms: diagnostics.first_byte_ms,
                        last_byte_ms: diagnostics.last_byte_ms,
                    }
                );
                assert_eq!(diagnostics.first_byte_ms.is_none(), empty);
                assert_eq!(diagnostics.last_byte_ms.is_none(), empty);
                assert!(diagnostics.first_byte_ms <= diagnostics.last_byte_ms);
                let recorded = serde_json::to_string(&diagnostics).unwrap();
                assert!(!recorded.contains("placeholder"));
                assert!(!recorded.contains("nested_argument"));
            }
        }
    }

    #[test]
    fn stream_diagnostics_keep_only_bounded_plain_root_and_response_keys() {
        let boundary = "A".repeat(64);
        let mut root = json!({
            "": "[content placeholder]",
            "9leading": "[content placeholder]",
            "has space": "[content placeholder]",
            "with/slash": "[content placeholder]",
            "with\ncontrol": "[content placeholder]",
            "non_ascii_é": "[content placeholder]",
            "_valid_2": "[content placeholder]",
            "container": {"nested_key": "[content placeholder]"},
        });
        root[&boundary] = json!("[content placeholder]");
        root["A".repeat(65)] = json!("[content placeholder]");
        for index in 0..20 {
            root[format!("k{index:02}")] = json!("[content placeholder]");
        }
        for wrapped in [false, true] {
            let mut decoder = GeminiDecoder::new(
                "gemini-3.1-pro-preview".into(),
                Billing::Metered,
                None,
                "diagnostic-keys".into(),
            );
            let mut root = root.clone();
            let mut expected = vec![boundary.clone()];
            if wrapped {
                root["response"] = json!({
                    "A_response": "[content placeholder]",
                    "B_container": {"deeper_key": "[content placeholder]"},
                    "_valid_2": "[content placeholder]",
                    "bad key": "[content placeholder]",
                });
                root["response"][&boundary] = json!("[content placeholder]");
                root["response"]["A".repeat(65)] = json!("[content placeholder]");
                expected.extend(["A_response".into(), "B_container".into()]);
            }
            expected.extend(["_valid_2".into(), "container".into()]);
            expected.extend((0..if wrapped { 11 } else { 13 }).map(|index| format!("k{index:02}")));
            assert!(decoder.push(&sse_frame(&root)).iter().all(Result::is_ok));
            let diagnostics = decoder.finish().unwrap().detail.stream.unwrap();
            assert_eq!(diagnostics.last_event_keys, expected);
            let recorded = serde_json::to_string(&diagnostics).unwrap();
            assert!(!recorded.contains("placeholder"));
            assert!(!recorded.contains("nested_key"));
            assert!(!recorded.contains("deeper_key"));
        }
    }

    #[test]
    fn invalid_and_empty_finishes_fail_with_billed_usage_even_after_calls_or_search() {
        let usage = json!({"promptTokenCount": 42, "thoughtsTokenCount": 5});
        let reply = |parts: Value, reason: &str, search: bool| {
            let mut candidate = json!({
                "content": {"role": "model", "parts": parts},
                "finishReason": reason,
                "finishMessage": "[diagnostic placeholder]",
            });
            if search {
                candidate["groundingMetadata"] = json!({
                    "webSearchQueries": ["rust async trait"],
                    "groundingChunks": [{"web": {"uri": "https://example.com/rust", "title": "rust"}}],
                });
            }
            json!({"candidates": [candidate], "usageMetadata": usage})
        };
        let decode = |frames: &[Value], wrapped: bool| {
            let mut decoder = GeminiDecoder::new(
                "gemini-4-carbon".into(),
                Billing::Metered,
                None,
                "tool-finish".into(),
            );
            let mut assembler = Assembler::default();
            let mut failure = None;
            for frame in frames {
                let frame = if wrapped {
                    json!({"response": frame})
                } else {
                    frame.clone()
                };
                for event in decoder.push(&sse_frame(&frame)) {
                    match event {
                        Ok(event) => {
                            assert!(failure.is_none());
                            assembler.apply(event).unwrap();
                        }
                        Err(error) => failure = Some(error),
                    }
                }
            }
            (decoder.ended(), assembler, failure)
        };
        let fails = |frames: &[Value], reason: &str, message: &str, searches: u64| {
            for wrapped in [false, true] {
                let (ended, assembler, failure) = decode(frames, wrapped);
                let usage = assembler.usage().unwrap();
                assert_eq!(usage.input_tokens, 42);
                assert_eq!(usage.output_tokens, 5);
                assert_eq!(usage.reasoning_tokens, 5);
                assert_eq!(usage.web_search_requests, searches);
                assert_eq!(usage.billing, Billing::Metered);
                let error = failure.unwrap();
                assert!(!ended, "{frames:?}");
                assert_eq!(error.kind, ErrorKind::InvalidResponse);
                assert_eq!(error.message, message);
                assert_eq!(error.detail.phase, FailurePhase::Stream);
                assert_eq!(error.detail.source, FailureSource::Endpoint);
                assert_eq!(error.detail.retry, RetryClass::Transient);
                assert_eq!(error.detail.status, Some(200));
                assert_eq!(error.detail.error_type.as_deref(), Some(reason));
                assert!(error.detail.possibly_billed && error.possibly_billed);
                assert!(error.usage_complete);
                assert!(!error.uncounted(true));
                assert!(assembler.finish().is_err());
            }
        };

        for (reason, message) in [
            (
                "MALFORMED_FUNCTION_CALL",
                "Gemini returned a malformed tool call",
            ),
            (
                "UNEXPECTED_TOOL_CALL",
                "Gemini returned an unexpected tool call",
            ),
        ] {
            for (call, search) in [(None, false), (Some("client_tool"), false), (None, true)] {
                let mut parts = vec![json!({"thought": true, "text": "[thought placeholder]"})];
                if let Some(name) = call {
                    parts.push(json!({"functionCall": {"name": name, "args": {}}}));
                }
                let frames = [reply(json!(parts), reason, search)];
                fails(&frames, reason, message, u64::from(search));
            }
        }

        let stop = |parts: Value| reply(parts, "STOP", false);
        let blank =
            json!({"candidates": [{"content": {"role": "model", "parts": [{"text": " "}]}}]});
        for frames in [
            vec![json!({"candidates": [{"finishReason": "STOP"}], "usageMetadata": usage})],
            vec![stop(json!([]))],
            vec![stop(json!([{"text": ""}]))],
            vec![stop(json!([{"text": " \n", "thoughtSignature": ""}]))],
            vec![stop(json!([{"thought": true, "text": ""}]))],
            vec![blank, stop(json!([]))],
        ] {
            fails(&frames, "STOP", "Gemini returned an empty reply", 0);
        }
        let said = json!({"candidates": [{"content": {"role": "model", "parts": [{"text": "[reply placeholder]"}]}}]});
        for frames in [
            vec![stop(
                json!([{"thought": true, "text": "[thought placeholder]"}]),
            )],
            vec![stop(
                json!([{"executableCode": {"language": "PYTHON", "code": "print(1)"}}]),
            )],
            vec![reply(json!([]), "STOP", true)],
            vec![said.clone(), stop(json!([]))],
            vec![said, stop(json!([{"text": ""}]))],
        ] {
            for wrapped in [false, true] {
                let (ended, assembler, failure) = decode(&frames, wrapped);
                assert!(ended && failure.is_none(), "{frames:?}");
                assert_eq!(assembler.finish().unwrap().stop_reason, StopReason::EndTurn);
            }
        }

        for (reason, expected) in [
            ("SAFETY", StopReason::Refusal),
            ("MAX_TOKENS", StopReason::MaxTokens),
        ] {
            let mut decoder = GeminiDecoder::new(
                "gemini-4-carbon".into(),
                Billing::Metered,
                None,
                "non-stop-call".into(),
            );
            let frame = sse_frame(&json!({
                "candidates": [{
                    "content": {
                        "role": "model",
                        "parts": [{"functionCall": {"name": "client_tool", "args": {}}}]
                    },
                    "finishReason": reason,
                }],
                "usageMetadata": {"promptTokenCount": 42, "candidatesTokenCount": 5},
            }));
            let mut assembler = Assembler::default();
            for event in decoder.push(&frame) {
                assembler.apply(event.unwrap()).unwrap();
            }
            assert!(decoder.ended());
            let stop_reason = assembler.finish().unwrap().stop_reason;
            assert_ne!(stop_reason, StopReason::ToolUse);
            assert_eq!(stop_reason, expected);
        }
    }

    #[test]
    fn blocked_prompt_feedback_ends_stream_cleanly_as_refusal_without_retrying() {
        let mut decoder = GeminiDecoder::new(
            "gemini-4-carbon".into(),
            Billing::Metered,
            None,
            "id_gem_blocked".into(),
        );
        let frame = sse_frame(&json!({
            "promptFeedback": {"blockReason": "SAFETY"},
            "usageMetadata": {"promptTokenCount": 42}
        }));
        let mut assembler = Assembler::default();
        for item in decoder.push(&frame) {
            assembler.apply(item.unwrap()).unwrap();
        }
        assert!(decoder.finish().is_none());
        assert!(decoder.ended());
        let msg = assembler.finish().unwrap();
        assert_eq!(msg.stop_reason, StopReason::Refusal);
        assert_eq!(msg.usage.input_tokens, 42);
    }

    #[test]
    fn unknown_parts_and_signed_text_parts_round_trip_and_trailing_chunks_after_stop_are_ignored() {
        let mut decoder = GeminiDecoder::new(
            "gemini-4-carbon".into(),
            Billing::Metered,
            None,
            "id_gem_parts".into(),
        );
        let first = sse_frame(&json!({
            "candidates": [{
                "content": {
                    "role": "model",
                    "parts": [
                        {
                            "executableCode": {"language": "PYTHON", "code": "print(1)"},
                            "thoughtSignature": "sig_code_1"
                        },
                        {
                            "text": "Final answer",
                            "thoughtSignature": "sig_text_final"
                        }
                    ]
                },
                "finishReason": "MAX_TOKENS"
            }],
            "usageMetadata": {"promptTokenCount": 12, "candidatesTokenCount": 4}
        }));
        let trailing = sse_frame(&json!({
            "candidates": [{
                "content": {"role": "model", "parts": [{"text": "spurious after stop"}]}
            }]
        }));
        let mut combined = first;
        combined.extend_from_slice(&trailing);

        let mut assembler = Assembler::default();
        for item in decoder.push(&combined) {
            assembler.apply(item.unwrap()).unwrap();
        }
        assert!(decoder.finish().is_none());
        let msg = assembler.finish().unwrap();
        assert_eq!(msg.stop_reason, StopReason::MaxTokens);
        assert_eq!(msg.content.len(), 3);
        assert_eq!(
            msg.content[0],
            crate::provider::envelope(
                "gemini",
                &json!({
                    "type": "part",
                    "part": {
                        "executableCode": {"language": "PYTHON", "code": "print(1)"},
                        "thoughtSignature": "sig_code_1"
                    }
                }),
                0,
                "id_gem_parts",
            )
        );
        assert_eq!(
            msg.content[1],
            crate::provider::envelope(
                "gemini",
                &json!({
                    "type": "text_signature",
                    "signature": "sig_text_final"
                }),
                1,
                "id_gem_parts",
            )
        );
        assert_eq!(
            msg.content[2],
            ContentBlock::Text {
                text: "Final answer".into()
            }
        );
    }

    #[test]
    fn mid_stream_error_chunk_and_malformed_json_classify_without_blind_retry() {
        let mut decoder = GeminiDecoder::new(
            "gemini-4-carbon".into(),
            Billing::Metered,
            None,
            "id_gem_err".into(),
        );
        let tail_padding = "x".repeat(260);
        let bad_arg = sse_frame(&json!([{
            "error": {
                "code": 400,
                "status": "INVALID_ARGUMENT",
                "message": format!(
                    "Invalid schema on projects/my-proj-123/models/gemini-4-carbon-medium for project_number:123456789012 with AIzaCANARY_stream_key_leak {tail_padding}"
                )
            }
        }]));
        let err = decoder
            .push(&bad_arg)
            .into_iter()
            .find_map(Result::err)
            .unwrap();
        assert_eq!(err.kind, ErrorKind::InvalidRequest);
        assert_eq!(err.detail.retry, RetryClass::Never);
        assert_eq!(err.message.chars().count(), 240);
        assert!(
            err.message.starts_with(
                "Invalid schema on projects/[redacted]/models/gemini-4-carbon-medium for project_number:[redacted] with [redacted] "
            ),
            "{}",
            err.message
        );
        assert!(!err.message.contains("AIzaCANARY_stream_key_leak"));
        assert!(!err.message.contains("my-proj-123"));
        assert!(!err.message.contains("123456789012"));

        let mut safety_decoder = GeminiDecoder::new(
            "gemini-4-carbon".into(),
            Billing::Metered,
            None,
            "id_gem_safety".into(),
        );
        let safety_frame = sse_frame(&json!({
            "candidates": [{"finishReason": "SAFETY"}]
        }));
        let mut safety_asm = Assembler::default();
        for item in safety_decoder.push(&safety_frame) {
            safety_asm.apply(item.unwrap()).unwrap();
        }
        assert_eq!(
            safety_asm.finish().unwrap().stop_reason,
            StopReason::Refusal
        );

        let mut malformed_decoder = GeminiDecoder::new(
            "gemini-4-carbon".into(),
            Billing::Metered,
            None,
            "id_gem_bad".into(),
        );
        let bad_err = malformed_decoder
            .push(b"data: {not-valid-json\n\n")
            .into_iter()
            .find_map(Result::err)
            .unwrap();
        assert_eq!(bad_err.kind, ErrorKind::InvalidResponse);
    }

    #[test]
    fn trailing_empty_text_signature_chunk_emits_no_thinking_block() {
        let mut decoder = GeminiDecoder::new(
            "gemini-4-carbon".into(),
            Billing::Metered,
            None,
            "id_gem_tail".into(),
        );
        let c1 = sse_frame(&json!({
            "candidates": [{
                "content": {
                    "role": "model",
                    "parts": [{"text": "Hello"}]
                }
            }]
        }));
        let c2 = sse_frame(&json!({
            "candidates": [{
                "content": {
                    "role": "model",
                    "parts": [{"text": "", "thoughtSignature": "sig_end"}]
                },
                "finishReason": "STOP"
            }]
        }));
        let mut assembler = Assembler::default();
        for chunk in [c1, c2] {
            for item in decoder.push(&chunk) {
                assembler.apply(item.unwrap()).unwrap();
            }
        }
        assert!(decoder.finish().is_none());
        let msg = assembler.finish().unwrap();
        assert!(
            !msg.content
                .iter()
                .any(|b| matches!(b, ContentBlock::Thinking { .. }))
        );
        assert_eq!(
            msg.content,
            vec![
                ContentBlock::Text {
                    text: "Hello".into(),
                },
                crate::provider::envelope(
                    "gemini",
                    &json!({"type": "text_signature", "signature": "sig_end"}),
                    0,
                    "id_gem_tail",
                ),
            ]
        );
        assert_eq!(
            crate::provider::reply_text(&msg.content).as_deref(),
            Some("Hello")
        );
    }

    #[test]
    fn trailing_empty_text_signature_after_thinking_stays_separate_and_signed_thoughts_do_not_concatenate()
     {
        let decode = |frames: Vec<Value>| {
            let mut decoder = GeminiDecoder::new(
                "gemini-4-neon".into(),
                Billing::Metered,
                None,
                "id_gem_parts".into(),
            );
            let mut assembler = Assembler::default();
            for frame in frames {
                for item in decoder.push(&sse_frame(&frame)) {
                    assembler.apply(item.unwrap()).unwrap();
                }
            }
            assert!(decoder.finish().is_none());
            assembler.finish().unwrap().content
        };

        let quiet_tail = decode(vec![
            json!({
                "candidates": [{
                    "content": {
                        "role": "model",
                        "parts": [{"thought": true, "text": "waiting for child"}]
                    }
                }]
            }),
            json!({
                "candidates": [{
                    "content": {
                        "role": "model",
                        "parts": [{"text": "", "thoughtSignature": "sig_tail"}]
                    },
                    "finishReason": "STOP"
                }]
            }),
        ]);
        assert_eq!(
            quiet_tail,
            vec![
                crate::provider::envelope(
                    super::super::PROVIDER,
                    &json!({
                        "type": REASONING_ITEM,
                        "thought": "waiting for child",
                        "signature": "",
                    }),
                    1,
                    "id_gem_parts",
                ),
                ContentBlock::Thinking {
                    thinking: "waiting for child".into(),
                    signature: String::new(),
                },
                crate::provider::envelope(
                    super::super::PROVIDER,
                    &json!({
                        "type": TEXT_SIGNATURE_ITEM,
                        "signature": "sig_tail",
                    }),
                    0,
                    "id_gem_parts",
                ),
            ]
        );

        let signed_alone = decode(vec![json!({
            "candidates": [{
                "content": {
                    "role": "model",
                    "parts": [{"text": "", "thoughtSignature": "sig_alone"}]
                },
                "finishReason": "STOP"
            }]
        })]);
        assert_eq!(
            signed_alone,
            vec![crate::provider::envelope(
                super::super::PROVIDER,
                &json!({"type": TEXT_SIGNATURE_ITEM, "signature": "sig_alone"}),
                0,
                "id_gem_parts",
            )]
        );

        let two_signed = decode(vec![
            json!({
                "candidates": [{
                    "content": {
                        "role": "model",
                        "parts": [{"thought": true, "text": "first", "thoughtSignature": "sig_1"}]
                    }
                }]
            }),
            json!({
                "candidates": [{
                    "content": {
                        "role": "model",
                        "parts": [{"thought": true, "text": "second", "thoughtSignature": "sig_2"}]
                    },
                    "finishReason": "STOP"
                }]
            }),
        ]);
        assert_eq!(
            two_signed,
            vec![
                crate::provider::envelope(
                    super::super::PROVIDER,
                    &json!({"type": REASONING_ITEM, "thought": "first", "signature": "sig_1"}),
                    1,
                    "id_gem_parts",
                ),
                ContentBlock::Thinking {
                    thinking: "first".into(),
                    signature: String::new(),
                },
                crate::provider::envelope(
                    super::super::PROVIDER,
                    &json!({"type": REASONING_ITEM, "thought": "second", "signature": "sig_2"}),
                    1,
                    "id_gem_parts",
                ),
                ContentBlock::Thinking {
                    thinking: "second".into(),
                    signature: String::new(),
                },
            ]
        );

        let signed_then_unsigned = decode(vec![
            json!({
                "candidates": [{
                    "content": {
                        "role": "model",
                        "parts": [{"thought": true, "text": "first", "thoughtSignature": "sig_1"}]
                    }
                }]
            }),
            json!({
                "candidates": [{
                    "content": {
                        "role": "model",
                        "parts": [{"thought": true, "text": "after"}]
                    },
                    "finishReason": "STOP"
                }]
            }),
        ]);
        assert_eq!(
            signed_then_unsigned,
            vec![
                crate::provider::envelope(
                    super::super::PROVIDER,
                    &json!({"type": REASONING_ITEM, "thought": "first", "signature": "sig_1"}),
                    1,
                    "id_gem_parts",
                ),
                ContentBlock::Thinking {
                    thinking: "first".into(),
                    signature: String::new(),
                },
                crate::provider::envelope(
                    super::super::PROVIDER,
                    &json!({"type": REASONING_ITEM, "thought": "after", "signature": ""}),
                    1,
                    "id_gem_parts",
                ),
                ContentBlock::Thinking {
                    thinking: "after".into(),
                    signature: String::new(),
                },
            ]
        );
    }
}
