use std::collections::BTreeSet;

use serde::Deserialize;
use wisp_protocol::{Effort, valid_model_id};

use super::{ListedModel, ModelList, Unusable, max_output, window};
use crate::models::ModelTable;
use crate::provider::anthropic::capabilities;

#[derive(Deserialize)]
struct Page {
    data: Vec<Entry>,
}

#[derive(Deserialize)]
struct Entry {
    id: String,
    lifecycle: String,
    line: Option<String>,
    max_input_tokens: Option<u64>,
    max_tokens: Option<u32>,
    capabilities: Option<Capabilities>,
}

#[derive(Deserialize)]
struct Capabilities {
    effort: Efforts,
    thinking: Thinking,
    image_input: Support,
    server_tools: ServerTools,
}

#[derive(Deserialize)]
struct Efforts {
    supported: bool,
    low: Support,
    medium: Support,
    high: Support,
    xhigh: Option<Support>,
    max: Support,
}

#[derive(Deserialize)]
struct Support {
    supported: bool,
}

#[derive(Deserialize)]
struct Thinking {
    types: ThinkingTypes,
}

#[derive(Deserialize)]
struct ThinkingTypes {
    adaptive: Support,
}

#[derive(Deserialize)]
struct ServerTools {
    web_search: Support,
}

pub fn parse(bytes: &[u8]) -> Result<ModelList, Unusable> {
    let page: Page = serde_json::from_slice(bytes).map_err(|_| Unusable::Invalid)?;
    let entries: Vec<Entry> = page
        .data
        .into_iter()
        .filter(|entry| valid_model_id(&entry.id) && entry.capabilities.is_some())
        .collect();
    let table = ModelTable::shared();
    let rank = |line: &str| {
        let of_line = entries.iter().filter(|e| e.line.as_deref() == Some(line));
        of_line.filter_map(|e| table.listed_rank(&e.id)).min()
    };
    let mut lines = BTreeSet::new();
    let mut models = Vec::new();
    for entry in &entries {
        let active = entry.lifecycle == "active";
        let known = active && capabilities::row(&entry.id).is_some();
        let rowless = active && !known;
        let line = entry.line.as_deref().filter(|_| known);
        let shown = line.filter(|line| lines.insert(*line));
        let order = match shown {
            Some(line) => (0, rank(line).map_or(u32::MAX, u32::from)),
            None if rowless => (1, 0),
            None => (2, 0),
        };
        let listed = entry.listed(shown.is_some() || rowless);
        models.extend(listed.map(|model| (order, model)));
    }
    models.sort_by_key(|(order, _)| *order);
    ModelList::new(models.into_iter().map(|(_, model)| model).collect())
}

impl Entry {
    fn listed(&self, shown: bool) -> Option<ListedModel> {
        let capabilities = self.capabilities.as_ref()?;
        Some(ListedModel {
            id: self.id.clone(),
            shown,
            window: window(&self.id, self.max_input_tokens),
            default_window: None,
            max_window: None,
            levels: capabilities.effort.levels(),
            default_level: None,
            max_output: max_output(&self.id, self.max_tokens),
            adaptive: Some(capabilities.thinking.types.adaptive.supported),
            images: capabilities.image_input.supported,
            search: capabilities.server_tools.web_search.supported,
        })
    }
}

impl Efforts {
    fn levels(&self) -> Vec<Effort> {
        let xhigh = self.xhigh.as_ref().is_some_and(|xhigh| xhigh.supported);
        let named = [
            self.low.supported,
            self.medium.supported,
            self.high.supported,
            xhigh,
            self.max.supported,
        ];
        Effort::ALL
            .into_iter()
            .zip(named)
            .filter_map(|(level, on)| (self.supported && on).then_some(level))
            .collect()
    }
}
