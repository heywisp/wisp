use std::collections::btree_map::Entry;
use std::collections::{BTreeMap, BTreeSet};

use serde_json::Value;
use wisp_protocol::log::LogEvent;
use wisp_protocol::{ContentBlock, ParentLink, Role, TodoItem};

use crate::session::REDACTED;

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub(crate) struct TodoProvenance {
    pub text: bool,
    pub section: bool,
}

pub(super) fn visible_strings(input: &Value) -> BTreeSet<String> {
    fn collect(value: &Value, into: &mut BTreeSet<String>) {
        match value {
            Value::String(text) if !text.trim().is_empty() => {
                into.insert(text.trim().to_owned());
            }
            Value::Array(values) => values.iter().for_each(|v| collect(v, into)),
            Value::Object(values) => values.values().for_each(|v| collect(v, into)),
            _ => {}
        }
    }
    let mut strings = BTreeSet::new();
    collect(input, &mut strings);
    strings
}

pub(crate) fn starting_provenance(
    items: &[TodoItem],
    parent: Option<&ParentLink>,
) -> Vec<TodoProvenance> {
    let Some(parent) = parent else {
        return unknown(items);
    };
    let brief_texts: BTreeSet<_> = parent
        .todos
        .iter()
        .map(|item| item.text.trim())
        .filter(|text| !text.is_empty())
        .collect();
    items
        .iter()
        .map(|item| TodoProvenance {
            text: brief_texts.contains(item.text.trim()),
            section: item.section.is_none(),
        })
        .collect()
}

fn unknown(items: &[TodoItem]) -> Vec<TodoProvenance> {
    vec![TodoProvenance::default(); items.len()]
}

pub(super) fn todo_provenance(
    items: &[TodoItem],
    visible: Option<&BTreeSet<String>>,
) -> Vec<TodoProvenance> {
    let Some(visible) = visible else {
        return unknown(items);
    };
    items
        .iter()
        .map(|item| TodoProvenance {
            text: visible.contains(item.text.as_str()),
            section: item
                .section
                .as_deref()
                .is_none_or(|section| visible.contains(section)),
        })
        .collect()
}

struct AssistantBatch {
    turn: u64,
    calls: BTreeMap<String, Option<BTreeSet<String>>>,
}

#[derive(Default)]
pub(crate) struct TodoFold {
    batch: Option<AssistantBatch>,
    pub(crate) todos: Vec<TodoItem>,
    pub(crate) provenance: Vec<TodoProvenance>,
}

impl TodoFold {
    pub(crate) fn see(&mut self, event: &LogEvent) {
        match event {
            LogEvent::MessageCommitted { turn, message, .. } if message.role == Role::Assistant => {
                let mut calls = BTreeMap::new();
                for block in &message.content {
                    if let Some(ContentBlock::ToolUse { id, input, .. }) = block.as_inline() {
                        match calls.entry(id.clone()) {
                            Entry::Vacant(entry) => {
                                entry.insert(Some(visible_strings(input)));
                            }
                            Entry::Occupied(mut entry) => {
                                entry.insert(None);
                            }
                        }
                    }
                }
                self.batch = Some(AssistantBatch { turn: *turn, calls });
            }
            LogEvent::TurnEnded { .. }
            | LogEvent::SessionStarted { .. }
            | LogEvent::ContextCompacted { .. } => {
                self.batch = None;
            }
            _ => {}
        }
        if let Some(items) = wisp_protocol::todos::listed(event) {
            let mut provenance = match event {
                LogEvent::SessionStarted {
                    parent: Some(parent),
                    ..
                } => starting_provenance(items, Some(parent)),
                LogEvent::TodosUpdated { turn, call_id, .. } => todo_provenance(
                    items,
                    self.batch
                        .as_ref()
                        .filter(|batch| batch.turn == *turn)
                        .and_then(|batch| batch.calls.get(call_id))
                        .and_then(Option::as_ref),
                ),
                _ => unknown(items),
            };
            for (item, origin) in items.iter().zip(&mut provenance) {
                origin.text &= !item.text.contains(REDACTED);
                origin.section &= item
                    .section
                    .as_deref()
                    .is_none_or(|section| !section.contains(REDACTED));
            }
            self.provenance = provenance;
            items.clone_into(&mut self.todos);
        }
    }
}

#[cfg(test)]
mod tests;
