use serde_json::Value;
use wisp_protocol::log::{BlockPosition, LoggedBlock};
use wisp_protocol::{ContentBlock, Message, RenderingForm, Role};

use super::{Rendering, ServerCalls};

use crate::search::WEB_SEARCH;
use wisp_protocol::{SEARCH_CITATION, SEARCH_RESULT, SERVER_TOOL_USE};

pub(crate) fn raw(block: &ContentBlock) -> Option<&Value> {
    match block {
        ContentBlock::Opaque { raw, .. } => Some(raw),
        _ => None,
    }
}

pub(crate) fn kind(block: &ContentBlock) -> Option<&str> {
    raw(block)?.get("type")?.as_str()
}

pub(crate) fn field<'a>(block: &'a ContentBlock, key: &str) -> Option<&'a str> {
    raw(block)?.get(key)?.as_str()
}

pub fn server_call(block: &ContentBlock) -> Option<&str> {
    (kind(block) == Some(SERVER_TOOL_USE))
        .then(|| field(block, "id"))
        .flatten()
}

pub fn server_result(block: &ContentBlock) -> Option<&str> {
    kind(block)
        .is_some_and(|t| t.ends_with("_tool_result"))
        .then(|| field(block, "tool_use_id"))
        .flatten()
}

pub fn is_thinking(block: &ContentBlock) -> bool {
    if let Some((item, _, _)) =
        super::openai::native_item(block).or_else(|| super::gemini::native_item(block))
    {
        return item.get("type").and_then(Value::as_str) == Some(super::REASONING_ITEM);
    }
    thinking_shaped(block)
}

pub fn thinking_shaped(block: &ContentBlock) -> bool {
    matches!(
        block,
        ContentBlock::Thinking { .. } | ContentBlock::RedactedThinking { .. }
    ) || matches!(kind(block), Some("thinking" | "redacted_thinking"))
}

pub fn native(block: &ContentBlock) -> bool {
    if super::openai::native_item(block).is_some() {
        return true;
    }
    match kind(block) {
        Some(SERVER_TOOL_USE) => {
            field(block, "name") == Some(WEB_SEARCH) && field(block, "id").is_some()
        }
        Some(SEARCH_RESULT) => field(block, "tool_use_id").is_some_and(|id| !id.is_empty()),
        Some("text") => raw(block)
            .and_then(|r| r.get("citations"))
            .and_then(Value::as_array)
            .is_some_and(|c| {
                c.iter()
                    .any(|c| c.get("type").and_then(Value::as_str) == Some(SEARCH_CITATION))
            }),
        _ => false,
    }
}

pub fn stream_indices(content: &[LoggedBlock]) -> Option<Vec<usize>> {
    let mut indices = Vec::with_capacity(content.len());
    let (mut items, mut owed) = (0, 0);
    for block in content {
        let envelope = match block {
            LoggedBlock::Inline(block) => super::openai::native_item(block),
            _ => None,
        };
        if let Some((_, projections, _)) = envelope {
            indices.push(super::openai::envelope_index(items));
            (items, owed) = (items + 1, projections);
        } else if owed > 0 {
            indices.push(super::openai::projection_index(items - 1));
            owed -= 1;
        } else {
            return None;
        }
    }
    (items > 0).then_some(indices)
}

fn byte_at(text: &str, chars: usize) -> Option<usize> {
    let ends = text.char_indices().map(|(at, _)| at);
    ends.chain([text.len()]).nth(chars)
}

pub fn cited_ends(envelope: &ContentBlock) -> Vec<(usize, String)> {
    let Some((item, ..)) = super::openai::native_item(envelope) else {
        return Vec::new();
    };
    let mut ends = Vec::new();
    let mut shift = 0;
    for part in super::openai::message_parts(item) {
        let text = super::openai::part_text(part);
        let annotations = part.get("annotations").and_then(Value::as_array);
        for annotation in annotations.into_iter().flatten() {
            let Some(source) = super::openai::search::cited(annotation) else {
                continue;
            };
            let at = |key| {
                let chars = usize::try_from(annotation.get(key)?.as_u64()?).ok()?;
                byte_at(text, chars)
            };
            if let (Some(start), Some(end)) = (at("start_index"), at("end_index"))
                && text
                    .get(start..end)
                    .is_some_and(|span| span.contains(&source.url))
            {
                ends.push((shift + end, source.url));
            }
        }
        shift += text.len();
    }
    ends.sort_by_key(|(end, _)| *end);
    ends
}

pub fn renders(block: &ContentBlock, index: usize, rendering: &Rendering) -> bool {
    server_call(block).is_some_and(|id| rendering.closed.contains(id))
        || (rendering.as_text(index) && native(block))
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Layout {
    echoed: Vec<Vec<usize>>,
    sent: Vec<Vec<String>>,
    rendered: Vec<bool>,
    flattened: Vec<bool>,
    tool_turns: Vec<bool>,
}

impl Layout {
    pub fn new(
        messages: &[Message],
        rendering: &Rendering,
        echoed: impl Fn(&[ContentBlock], &ServerCalls) -> Vec<usize>,
    ) -> Self {
        let mut earlier = ServerCalls::new();
        let mut layout = Self::default();
        for (index, message) in messages.iter().enumerate() {
            let indices = if message.role == Role::Assistant {
                echoed(&message.content, &earlier)
            } else {
                Vec::new()
            };
            let mut rendered = false;
            let mut sent = Vec::new();
            for block in indices.iter().filter_map(|b| message.content.get(*b)) {
                if renders(block, index, rendering) {
                    rendered = true;
                } else if let Some(id) = server_call(block) {
                    sent.push(id.to_owned());
                }
            }
            earlier.extend(sent.iter().cloned());
            layout.echoed.push(indices);
            layout.sent.push(sent);
            layout.rendered.push(rendered);
        }
        layout.flattened = layout.pair(messages, rendering, layout.rendered.clone());
        let seeds = (0..messages.len())
            .map(|i| layout.strips_a_tool_turn(messages, rendering, i))
            .collect();
        layout.tool_turns = layout.pair(messages, rendering, seeds);
        layout
    }

    fn ids<'a>(
        &self,
        messages: &'a [Message],
        index: usize,
        pick: fn(&ContentBlock) -> Option<&str>,
    ) -> ServerCalls {
        let (Some(message), Some(echoed)) = (messages.get(index), self.echoed.get(index)) else {
            return ServerCalls::new();
        };
        echoed
            .iter()
            .filter_map(|b| message.content.get(*b))
            .filter_map(pick)
            .map(str::to_owned)
            .collect()
    }

    fn strips_a_tool_turn(
        &self,
        messages: &[Message],
        rendering: &Rendering,
        index: usize,
    ) -> bool {
        let Some(message) = messages.get(index).filter(|_| self.rendered(index)) else {
            return false;
        };
        let echoed = || {
            self.echoed(index)
                .iter()
                .filter_map(|b| message.content.get(*b).map(|block| (*b, block)))
        };
        echoed().any(|(_, block)| matches!(block, ContentBlock::ToolUse { .. }))
            && echoed().any(|(b, block)| is_thinking(block) && !rendering.cut(index, b))
    }

    fn pair(&self, messages: &[Message], rendering: &Rendering, seeds: Vec<bool>) -> Vec<bool> {
        let assistant = |i: usize| messages.get(i).is_some_and(|m| m.role == Role::Assistant);
        let calls: Vec<ServerCalls> = (0..messages.len())
            .map(|i| self.ids(messages, i, server_call))
            .collect();
        let results: Vec<ServerCalls> = (0..messages.len())
            .map(|i| self.ids(messages, i, server_result))
            .collect();
        let mut flat = seeds;
        let mut work: Vec<usize> = (0..flat.len()).filter(|i| flat[*i]).collect();
        while let Some(i) = work.pop() {
            let mut join: Vec<usize> = (0..messages.len())
                .filter(|j| *j != i && assistant(*j))
                .filter(|j| {
                    let (later, earlier) = if *j > i { (*j, i) } else { (i, *j) };
                    !calls[earlier].is_disjoint(&results[later])
                })
                .collect();
            if let Some(before) = i.checked_sub(1)
                && assistant(before)
                && rendering.paused(before)
            {
                join.push(before);
            }
            if rendering.paused(i) && assistant(i + 1) {
                join.push(i + 1);
            }
            for j in join {
                if let Some(slot) = flat.get_mut(j)
                    && !*slot
                {
                    *slot = true;
                    work.push(j);
                }
            }
        }
        flat
    }

    pub fn carries_rendered(&self) -> bool {
        self.rendered.iter().any(|r| *r)
    }

    pub fn rendered(&self, index: usize) -> bool {
        self.rendered.get(index).copied().unwrap_or(false)
    }

    pub fn flattened(&self, index: usize) -> bool {
        self.flattened.get(index).copied().unwrap_or(false)
    }

    pub fn tool_turns(&self, index: usize) -> bool {
        self.tool_turns.get(index).copied().unwrap_or(false)
    }

    pub fn echoed(&self, index: usize) -> &[usize] {
        self.echoed.get(index).map_or(&[], Vec::as_slice)
    }

    pub fn sent(&self, index: usize) -> &[String] {
        self.sent.get(index).map_or(&[], Vec::as_slice)
    }

    pub fn first_change(
        &self,
        messages: &[Message],
        rendering: &Rendering,
        form: RenderingForm,
    ) -> Option<BlockPosition> {
        let thinking = (0..messages.len())
            .filter(|i| self.rendered(*i))
            .flat_map(|i| {
                self.echoed(i)
                    .iter()
                    .filter(move |b| {
                        messages
                            .get(i)
                            .and_then(|m| m.content.get(**b))
                            .is_some_and(is_thinking)
                            && !rendering.cut(i, **b)
                    })
                    .map(move |b| BlockPosition {
                        message: i,
                        block: *b,
                    })
            })
            .min();
        match form {
            RenderingForm::Text => None,
            RenderingForm::WithoutThinking => thinking,
            RenderingForm::Flattened => self
                .flattened
                .iter()
                .position(|f| *f)
                .map(|message| BlockPosition { message, block: 0 })
                .into_iter()
                .chain(thinking)
                .min(),
        }
    }
}

#[cfg(test)]
mod tests;
