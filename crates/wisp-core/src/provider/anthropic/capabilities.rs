use wisp_media::Tier;

use crate::endpoints::ProviderKind;
use crate::models::list::ModelList;
use crate::models::{ModelTable, ProviderRow};
use crate::provider::{ImageCaps, ModelCapabilities};

const LISTED_ONLY: ModelCapabilities = ModelCapabilities {
    efforts: &[],
    default_effort: None,
    per_message_effort: false,
    effort_in_id: None,
    inline_tools: false,
    tools_after_results: false,
    deferred_tools: false,
    adaptive_thinking: false,
    thinking_budgets: None,
    max_output: None,
    images: Some(ImageCaps::new(Tier::STANDARD, 100)),
};

pub const ID_PREFIX: &str = "claude-";

fn anthropic_lists() -> Option<&'static ProviderRow> {
    ModelTable::shared().provider(ProviderKind::Anthropic)
}

pub fn account_bound_thinking(model: &str) -> bool {
    anthropic_lists().is_some_and(|lists| lists.account_bound_thinking.iter().any(|id| id == model))
}

pub fn message_threads(model: &str) -> bool {
    anthropic_lists().is_some_and(|lists| lists.message_threads.iter().any(|id| id == model))
}

pub fn row(model: &str) -> Option<&'static ModelCapabilities> {
    ModelTable::shared().capabilities(ProviderKind::Anthropic, model)
}

pub fn listed(list: Option<&ModelList>, model: &str) -> Option<ModelCapabilities> {
    let shipped = row(model).copied();
    list.and_then(|list| list.capabilities(ProviderKind::Anthropic, model, shipped, LISTED_ONLY))
        .or(shipped)
}

#[cfg(test)]
mod tests;
