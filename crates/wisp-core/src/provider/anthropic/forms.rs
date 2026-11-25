use std::collections::BTreeMap;

use serde_json::Value;
use wisp_protocol::{ContentBlock, Message, RenderingForm, Role};

use super::{
    KnownBlock, RenderedText, WireBlock, WireMessage, opaque_str, opaque_type, search, wire_block,
};
use crate::provider::forms::{Layout, is_thinking};
use crate::provider::{NO_TEXT, NotSent, Rendering, not_sent_note};

pub(super) struct Flattener<'a> {
    on: bool,
    rendering: &'a Rendering,
    layout: &'a Layout,
    names: BTreeMap<&'a str, &'a str>,
    out: Vec<WireMessage<'a>>,
    run: Option<String>,
    results: bool,
}

impl<'a> Flattener<'a> {
    pub(super) fn new(
        messages: &'a [Message],
        rendering: &'a Rendering,
        layout: &'a Layout,
    ) -> Self {
        let on = rendering.form >= RenderingForm::WithoutThinking;
        let names = if on {
            call_names(messages)
        } else {
            BTreeMap::new()
        };
        Self {
            on,
            rendering,
            layout,
            names,
            out: Vec::with_capacity(messages.len()),
            run: None,
            results: false,
        }
    }

    pub(super) fn take(&mut self, index: usize, message: &'a Message) -> bool {
        if !self.on || message.role != Role::Assistant || !self.joined(index) {
            self.flush();
            return false;
        }
        let text = self.text(&message.content, self.layout.echoed(index));
        let run = self.run.get_or_insert_with(String::new);
        if !text.is_empty() {
            if !run.is_empty() {
                run.push('\n');
            }
            run.push_str(&text);
        }
        true
    }

    fn joined(&self, index: usize) -> bool {
        match self.rendering.form {
            RenderingForm::Text => false,
            RenderingForm::WithoutThinking => self.layout.tool_turns(index),
            RenderingForm::Flattened => self.layout.flattened(index),
        }
    }

    pub(super) fn user_blocks(&mut self, content: &'a [ContentBlock]) -> Vec<WireBlock<'a>> {
        if !std::mem::take(&mut self.results) {
            return content.iter().filter_map(|b| self.user_block(b)).collect();
        }
        let results = content.iter().filter_map(|block| match block {
            ContentBlock::ToolResult {
                tool_use_id,
                content,
                is_error,
            } => {
                let name = self.names.get(tool_use_id.as_str()).copied();
                let name = name.unwrap_or(tool_use_id.as_str());
                let label = if *is_error { "error from" } else { "result of" };
                Some(RenderedText::block(format!(
                    "[{label} {name}: {}]",
                    result_text(content)
                )))
            }
            _ => None,
        });
        let rest = content
            .iter()
            .filter(|b| !matches!(b, ContentBlock::ToolResult { .. }))
            .filter_map(|b| self.user_block(b));
        results.chain(rest).collect()
    }

    fn user_block(&self, block: &'a ContentBlock) -> Option<WireBlock<'a>> {
        if !self.rendering.attachments_as_notes {
            return wire_block(block);
        }
        if let Some(media) = block.inline_media() {
            return Some(RenderedText::block(not_sent_note(
                media.media_type,
                media.slice,
                if media.slice.is_some() {
                    NotSent::Slice
                } else {
                    NotSent::Model
                },
            )));
        }
        match block {
            ContentBlock::ToolResult {
                tool_use_id,
                content,
                is_error,
            } => Some(WireBlock::Known(KnownBlock::ToolResult {
                tool_use_id,
                content: content.iter().filter_map(|b| self.user_block(b)).collect(),
                is_error: *is_error,
            })),
            _ => wire_block(block),
        }
    }

    pub(super) fn push(&mut self, message: WireMessage<'a>) {
        if message.role == "assistant" {
            self.results = false;
        }
        self.out.push(message);
    }

    pub(super) fn finish(mut self) -> Vec<WireMessage<'a>> {
        self.flush();
        self.out
    }

    fn flush(&mut self) {
        let Some(text) = self.run.take() else {
            return;
        };
        let text = if text.is_empty() {
            NO_TEXT.to_owned()
        } else {
            text
        };
        self.out.push(WireMessage {
            role: "assistant",
            content: vec![RenderedText::block(text)],
            output_config: None,
        });
        self.results = true;
    }

    fn text(&self, content: &[ContentBlock], echoed: &[usize]) -> String {
        echoed
            .iter()
            .filter_map(|b| content.get(*b))
            .filter_map(|block| self.line(block))
            .collect::<Vec<_>>()
            .join("\n")
    }

    fn line(&self, block: &ContentBlock) -> Option<String> {
        if is_thinking(block) {
            return None;
        }
        if let Some(text) = search::closed_text(block, &self.rendering.closed) {
            return Some(text);
        }
        if let Some(rendered) = search::rule_r(block) {
            return rendered;
        }
        let shown = |text: &str| (!text.trim().is_empty()).then(|| text.to_owned());
        match block {
            ContentBlock::Text { text } => shown(text),
            ContentBlock::ToolUse { name, input, .. } => Some(called(name, input)),
            ContentBlock::Opaque { raw, .. } => match opaque_type(block)? {
                "text" => shown(opaque_str(block, "text")?),
                wisp_protocol::SERVER_TOOL_USE => {
                    let name = opaque_str(block, "name").unwrap_or_default();
                    Some(called(name, raw.get("input").unwrap_or(&Value::Null)))
                }
                kind if kind.ends_with("_tool_result") => {
                    let id = opaque_str(block, "tool_use_id").unwrap_or_default();
                    let name = self.names.get(id).copied().unwrap_or(id);
                    let content = raw.get("content").unwrap_or(&Value::Null);
                    Some(format!("[result of {name}: {}]", canonical(content)))
                }
                _ => None,
            },
            _ => None,
        }
    }
}

fn canonical(value: &Value) -> String {
    serde_json::to_string(value).unwrap_or_default()
}

fn called(name: &str, input: &Value) -> String {
    format!("[called {name}: {}]", canonical(input))
}

fn result_text(content: &[ContentBlock]) -> String {
    content
        .iter()
        .filter_map(|block| match block {
            ContentBlock::Text { text } => Some(text.clone()),
            ContentBlock::Image { .. } => Some("[image]".to_owned()),
            ContentBlock::Media {
                source,
                slice: Some(slice),
            } => Some(not_sent_note(
                source.media_type().as_str(),
                Some(*slice),
                NotSent::Slice,
            )),
            ContentBlock::Media { .. } => Some("[file]".to_owned()),
            _ => None,
        })
        .collect::<Vec<_>>()
        .join("\n")
}

fn call_names(messages: &[Message]) -> BTreeMap<&str, &str> {
    messages
        .iter()
        .filter(|m| m.role == Role::Assistant)
        .flat_map(|m| m.content.iter())
        .filter_map(|block| match block {
            ContentBlock::ToolUse { id, name, .. } => Some((id.as_str(), name.as_str())),
            ContentBlock::Opaque { .. }
                if opaque_type(block) == Some(wisp_protocol::SERVER_TOOL_USE) =>
            {
                Some((opaque_str(block, "id")?, opaque_str(block, "name")?))
            }
            _ => None,
        })
        .collect()
}
