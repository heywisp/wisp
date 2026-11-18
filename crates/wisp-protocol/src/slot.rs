use std::collections::BTreeSet;

use serde::{Deserialize, Serialize};

use crate::context::{FillPart, PartTokens};

pub const PANEL_BYTES: u64 = 16_384;
pub const PAGE_BYTES: u64 = 65_536;
pub const PANEL_ITEMS: usize = 200;
pub const PAGE_ITEMS: usize = 1_000;
pub const MAX_DEPTH: u8 = 8;
pub const MAX_SPARK_VALUES: usize = 120;

pub const TOO_MANY_ITEMS: &str = "too_many_items";
pub const TREE_DEPTH: &str = "tree_depth";
pub const GAUGE_RANGE: &str = "gauge_range";
pub const SPARK_RANGE: &str = "spark_range";
pub const FILL_RANGE: &str = "fill_range";
pub const TOPOLOGY_KEYS: &str = "topology_keys";

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct SlotContent {
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub note: Vec<SlotSpan>,
    pub blocks: Vec<SlotBlock>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum SlotBlock {
    Line {
        spans: Vec<SlotSpan>,
    },
    Rows {
        rows: Vec<SlotRow>,
    },
    Gauge {
        label: String,
        value: u64,
        max: u64,
        #[serde(default, skip_serializing_if = "SlotTone::is_text")]
        tone: SlotTone,
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        note: Vec<SlotSpan>,
    },
    Spark {
        label: String,
        values: Vec<u64>,
        max: u64,
        #[serde(default, skip_serializing_if = "SlotTone::is_text")]
        tone: SlotTone,
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        note: Vec<SlotSpan>,
    },
    List {
        items: Vec<Vec<SlotSpan>>,
    },
    Tree {
        nodes: Vec<SlotNode>,
    },
    NextColumn,
    Fill {
        fill: SlotFill,
    },
    Topology {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        root: Option<String>,
        nodes: Vec<SlotTopologyNode>,
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        flows: Vec<SlotFlow>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        counted: Option<SlotNoun>,
    },
    #[serde(other)]
    Unknown,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SlotTopologyNode {
    pub key: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub parent: Option<String>,
    pub state: NodeState,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SlotFlow {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub from: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub to: Option<String>,
    pub at_ms: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SlotNoun {
    Agents,
    #[serde(other)]
    Unknown,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SlotFill {
    pub used: u64,
    #[serde(default, skip_serializing_if = "is_false")]
    pub estimated: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub window: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub limit: Option<u64>,
    #[serde(default, skip_serializing_if = "is_false")]
    pub auto_compact: bool,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub parts: Vec<PartTokens>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cached: Option<u8>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum SlotSpan {
    Text {
        text: String,
        #[serde(default, skip_serializing_if = "SlotTone::is_text")]
        tone: SlotTone,
        #[serde(default, skip_serializing_if = "is_false")]
        bold: bool,
    },
    Elapsed {
        since_ms: u64,
        #[serde(default, skip_serializing_if = "SlotTone::is_text")]
        tone: SlotTone,
    },
    Usd {
        micros: u64,
        #[serde(default, skip_serializing_if = "is_false")]
        at_least: bool,
        #[serde(default, skip_serializing_if = "SlotTone::is_text")]
        tone: SlotTone,
    },
    Fill {
        fill: SlotFill,
    },
    #[serde(other)]
    Unknown,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SlotRow {
    pub key: String,
    pub value: Vec<SlotSpan>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SlotNode {
    pub depth: u8,
    pub state: NodeState,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub role: Option<String>,
    pub label: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub facts: Vec<SlotSpan>,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SlotTone {
    #[default]
    Text,
    Muted,
    Faint,
    Accent,
    Ok,
    Error,
    #[serde(other)]
    Unknown,
}

impl SlotTone {
    pub fn is_text(&self) -> bool {
        *self == Self::Text
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum NodeState {
    Working,
    Waiting,
    Done,
    Failed,
    Stopped,
    #[serde(other)]
    Unknown,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SlotEntry {
    pub name: String,
    pub slot: SlotPlace,
    pub title: String,
    pub default_on: bool,
    pub order: u32,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub disabled: Option<SlotDisabled>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SlotPlace {
    Sidebar,
    #[serde(other)]
    Unknown,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SlotView {
    #[default]
    Panel,
    Page,
    #[serde(other)]
    Unknown,
}

impl SlotView {
    pub fn is_panel(&self) -> bool {
        *self == Self::Panel
    }

    fn limits(self) -> Limits {
        match self {
            Self::Page => Limits {
                bytes: PAGE_BYTES,
                items: PAGE_ITEMS,
            },
            Self::Panel | Self::Unknown => Limits {
                bytes: PANEL_BYTES,
                items: PANEL_ITEMS,
            },
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum SlotDisabled {
    Cycle {
        cycle: Vec<String>,
    },
    #[serde(other)]
    Unknown,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum SlotFailure {
    Failed {
        message: String,
    },
    TimedOut,
    TooLarge {
        bytes: u64,
    },
    Invalid {
        rule: String,
    },
    #[serde(other)]
    Unknown,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SlotInvalid {
    TooLarge { bytes: u64 },
    Rule(&'static str),
}

struct Limits {
    bytes: u64,
    items: usize,
}

pub fn check(content: &SlotContent, view: SlotView) -> Result<(), SlotInvalid> {
    let topology = (content.blocks.iter()).any(|b| matches!(b, SlotBlock::Topology { .. }));
    let limits = if topology { SlotView::Page } else { view }.limits();
    let bytes = serde_json::to_vec(content).map_or(u64::MAX, |b| b.len() as u64);
    if bytes > limits.bytes {
        return Err(SlotInvalid::TooLarge { bytes });
    }
    let items: usize = content.blocks.iter().map(SlotBlock::items).sum();
    if items > limits.items {
        return Err(SlotInvalid::Rule(TOO_MANY_ITEMS));
    }
    content.blocks.iter().try_for_each(SlotBlock::check)
}

impl SlotBlock {
    fn items(&self) -> usize {
        match self {
            Self::Rows { rows } => rows.len(),
            Self::List { items } => items.len(),
            Self::Tree { nodes } => nodes.len(),
            Self::Topology { nodes, flows, .. } => nodes.len() + flows.len(),
            Self::Line { .. }
            | Self::Gauge { .. }
            | Self::Spark { .. }
            | Self::NextColumn
            | Self::Fill { .. }
            | Self::Unknown => 1,
        }
    }

    fn check(&self) -> Result<(), SlotInvalid> {
        let fails = |broken: bool, rule| {
            if broken {
                Err(SlotInvalid::Rule(rule))
            } else {
                Ok(())
            }
        };
        match self {
            Self::Tree { nodes } => fails(!depths_step_down(nodes), TREE_DEPTH),
            Self::Topology { nodes, flows, .. } => fails(
                !topology_holds_together(nodes) || !flows_land(nodes, flows),
                TOPOLOGY_KEYS,
            ),
            Self::Gauge { value, max, .. } => fails(*max == 0 || value > max, GAUGE_RANGE),
            Self::Spark { values, max, .. } => fails(
                *max == 0 || values.len() > MAX_SPARK_VALUES || values.iter().any(|v| v > max),
                SPARK_RANGE,
            ),
            Self::Fill { fill } => fails(
                fill.cached.is_some_and(|cached| cached > 100) || repeats_a_part(&fill.parts),
                FILL_RANGE,
            ),
            Self::Line { .. }
            | Self::Rows { .. }
            | Self::List { .. }
            | Self::NextColumn
            | Self::Unknown => Ok(()),
        }
    }
}

fn depths_step_down(nodes: &[SlotNode]) -> bool {
    let mut deepest_allowed = 0u8;
    nodes.iter().all(|node| {
        let fits = node.depth <= deepest_allowed && node.depth <= MAX_DEPTH;
        deepest_allowed = node.depth.saturating_add(1);
        fits
    })
}

fn topology_holds_together(nodes: &[SlotTopologyNode]) -> bool {
    let mut seen = BTreeSet::new();
    nodes.iter().all(|node| {
        let parent_known = node.parent.as_ref().is_none_or(|p| seen.contains(p));
        parent_known && seen.insert(&node.key)
    })
}

fn flows_land(nodes: &[SlotTopologyNode], flows: &[SlotFlow]) -> bool {
    let known =
        |end: &Option<String>| (end.as_ref()).is_none_or(|k| nodes.iter().any(|n| &n.key == k));
    flows
        .iter()
        .all(|flow| known(&flow.from) && known(&flow.to))
}

fn repeats_a_part(parts: &[PartTokens]) -> bool {
    FillPart::ORDER
        .iter()
        .any(|part| parts.iter().filter(|p| p.part == *part).count() > 1)
}

fn is_false(b: &bool) -> bool {
    !*b
}

#[cfg(test)]
mod tests;
