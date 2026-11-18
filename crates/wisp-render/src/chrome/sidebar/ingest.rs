use wisp_protocol::slot::{SlotBlock, SlotContent, SlotFailure, SlotNode, SlotRow, SlotSpan};

use crate::text::sanitize;

pub(super) fn text(raw: &str) -> String {
    sanitize(&raw.replace(|c: char| c.is_whitespace() && c.is_control(), " "))
}

fn spans(list: &[SlotSpan]) -> Vec<SlotSpan> {
    list.iter().map(span).collect()
}

fn span(span: &SlotSpan) -> SlotSpan {
    match span {
        SlotSpan::Text {
            text: raw,
            tone,
            bold,
        } => SlotSpan::Text {
            text: text(raw),
            tone: *tone,
            bold: *bold,
        },
        other => other.clone(),
    }
}

fn block(block: &SlotBlock) -> SlotBlock {
    match block {
        SlotBlock::Line { spans: list } => SlotBlock::Line { spans: spans(list) },
        SlotBlock::Rows { rows } => SlotBlock::Rows {
            rows: rows
                .iter()
                .map(|row| SlotRow {
                    key: text(&row.key),
                    value: spans(&row.value),
                })
                .collect(),
        },
        SlotBlock::Gauge {
            label,
            value,
            max,
            tone,
            note,
        } => SlotBlock::Gauge {
            label: text(label),
            value: *value,
            max: *max,
            tone: *tone,
            note: spans(note),
        },
        SlotBlock::Spark {
            label,
            values,
            max,
            tone,
            note,
        } => SlotBlock::Spark {
            label: text(label),
            values: values.clone(),
            max: *max,
            tone: *tone,
            note: spans(note),
        },
        SlotBlock::List { items } => SlotBlock::List {
            items: items.iter().map(|item| spans(item)).collect(),
        },
        SlotBlock::Tree { nodes } => SlotBlock::Tree {
            nodes: nodes
                .iter()
                .map(|node| SlotNode {
                    label: text(&node.label),
                    facts: spans(&node.facts),
                    ..node.clone()
                })
                .collect(),
        },
        SlotBlock::Fill { fill } => SlotBlock::Fill { fill: fill.clone() },
        SlotBlock::Topology {
            root,
            nodes,
            flows,
            counted,
        } => SlotBlock::Topology {
            root: root.as_deref().map(text),
            nodes: nodes.clone(),
            flows: flows.clone(),
            counted: *counted,
        },
        SlotBlock::NextColumn => SlotBlock::NextColumn,
        SlotBlock::Unknown => SlotBlock::Unknown,
    }
}

pub(super) fn content(content: &SlotContent) -> SlotContent {
    SlotContent {
        note: spans(&content.note),
        blocks: content.blocks.iter().map(block).collect(),
    }
}

pub(super) fn failure(failure: &SlotFailure) -> SlotFailure {
    match failure {
        SlotFailure::Failed { message } => SlotFailure::Failed {
            message: text(message),
        },
        SlotFailure::Invalid { rule } => SlotFailure::Invalid { rule: text(rule) },
        other => other.clone(),
    }
}
