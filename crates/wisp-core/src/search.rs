use std::sync::LazyLock;

use wisp_protocol::SearchOutcome;

use crate::context::{ListedRow, ModelSettings};
use crate::endpoints::ProviderKind;
use crate::provider::ServerToolSpec;
use crate::settings::Layer;

pub use wisp_protocol::{NoSearch, SearchForm, WEB_SEARCH};

static NATIVE: LazyLock<Vec<ServerToolSpec>> = LazyLock::new(|| {
    wisp_protocol::settings::WEB_SEARCH_VERSIONS
        .iter()
        .map(|version| ServerToolSpec {
            name: WEB_SEARCH.to_owned(),
            kind: (*version).to_owned(),
        })
        .collect()
});

static BY_CALL: LazyLock<[ServerToolSpec; 1]> = LazyLock::new(|| {
    [ServerToolSpec {
        name: WEB_SEARCH.to_owned(),
        kind: "call".to_owned(),
    }]
});

pub fn by_call(entries: &[ServerToolSpec]) -> bool {
    entries.iter().any(|entry| entry == &BY_CALL[0])
}

pub fn runs_by_call(entries: &[ServerToolSpec]) -> bool {
    by_call(entries)
        || entries.iter().any(|entry| {
            entry.name == WEB_SEARCH && entry.kind == wisp_protocol::settings::GEMINI_WEB_SEARCH
        })
}

pub fn sent(entries: &[ServerToolSpec]) -> &[ServerToolSpec] {
    if by_call(entries) { &[] } else { entries }
}

pub fn wire<'a>(
    specs: &[crate::tool::ToolSpec],
    entries: &'a [ServerToolSpec],
    runner: Option<&crate::tool::ToolSpec>,
) -> (Vec<crate::tool::ToolSpec>, &'a [ServerToolSpec]) {
    let mut specs = specs.to_vec();
    if by_call(entries)
        && let Some(runner) = runner
    {
        specs.push(runner.clone());
    }
    (specs, sent(entries))
}

pub fn side_model(provider: &dyn crate::provider::Provider, chat: &str) -> String {
    crate::models::ModelTable::shared()
        .provider(provider.kind())
        .and_then(|row| row.web_search_model.as_deref())
        .filter(|model| crate::agent::side::served_side_model(provider, model))
        .map(str::to_owned)
        .unwrap_or_else(|| crate::agent::side::one_shot_model(provider, chat))
}

pub fn listed_version(kind: ProviderKind) -> &'static str {
    match kind {
        ProviderKind::Anthropic => wisp_protocol::settings::CLAUDE_WEB_SEARCH,
        ProviderKind::OpenAi => wisp_protocol::settings::OPENAI_WEB_SEARCH,
        ProviderKind::Gemini => wisp_protocol::settings::GEMINI_WEB_SEARCH,
    }
}

pub fn tool(version: &str) -> Option<&'static ServerToolSpec> {
    NATIVE.iter().find(|spec| spec.kind == version)
}

fn speaks(wire: ProviderKind, version: &str) -> bool {
    match wire {
        ProviderKind::Anthropic => version
            .strip_prefix(WEB_SEARCH)
            .and_then(|rest| rest.strip_prefix('_'))
            .is_some_and(|date| date.len() == 8 && date.bytes().all(|b| b.is_ascii_digit())),
        ProviderKind::OpenAi => version == wisp_protocol::settings::OPENAI_WEB_SEARCH,
        ProviderKind::Gemini => version == wisp_protocol::settings::GEMINI_WEB_SEARCH,
    }
}

fn sendable(wire: ProviderKind, versions: &[String]) -> Option<&'static ServerToolSpec> {
    versions
        .iter()
        .filter_map(|version| tool(version))
        .find(|spec| speaks(wire, &spec.kind))
}

pub fn deciding<'a>(wire: ProviderKind, lists: impl IntoIterator<Item = &'a [String]>) -> usize {
    lists
        .into_iter()
        .position(|list| list.is_empty() || sendable(wire, list).is_some())
        .unwrap_or(0)
}

pub fn call_line(query: &str) -> String {
    format!("[web search: \"{query}\"]")
}

pub fn had_search(names: &[String]) -> bool {
    names.iter().any(|name| name == WEB_SEARCH)
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SearchFacts {
    pub by_call: bool,
    pub versions: Vec<String>,
    pub versions_from: Layer,
    pub wire: ProviderKind,
    pub setting: Option<bool>,
    pub setting_from: Layer,
}

impl SearchFacts {
    pub fn resolve(
        models: &ModelSettings,
        endpoint: &str,
        model: &str,
        wire: ProviderKind,
        listed: Option<&ListedRow>,
    ) -> Self {
        let layered = models.layered(endpoint, model);
        let own = ModelSettings::own_rows(&layered).into_iter().zip([
            Layer::Flag,
            Layer::Flag,
            Layer::EndpointModel,
            Layer::Model,
        ]);
        let places: Vec<(&[String], Layer)> = own
            .chain([
                (listed.map(|l| &l.row), Layer::Shipped),
                (layered.shipped, Layer::Shipped),
            ])
            .filter_map(|(row, layer)| Some((row?.web_search.as_deref()?, layer)))
            .collect();
        let at = deciding(wire, places.iter().map(|(list, _)| *list));
        let (versions, versions_from) = places
            .get(at)
            .map_or((Vec::new(), Layer::Default), |(list, layer)| {
                (list.to_vec(), *layer)
            });
        Self {
            by_call: crate::models::ModelTable::shared()
                .provider(wire)
                .is_some_and(|provider| provider.web_search_by_call),
            versions,
            versions_from,
            wire,
            setting: layered.native_web_search,
            setting_from: layered.native_web_search_from,
        }
    }

    pub fn turned_off(&self) -> bool {
        self.setting == Some(false)
    }

    pub fn form(&self, requested: bool, refused: bool) -> SearchForm {
        if !requested {
            return SearchForm::None(NoSearch::NotRequested);
        }
        if refused {
            return SearchForm::None(NoSearch::Refused);
        }
        if self.native().is_none() {
            return SearchForm::None(NoSearch::NotSupported);
        }
        if self.turned_off() {
            return SearchForm::None(NoSearch::TurnedOff);
        }
        SearchForm::Native
    }

    pub fn native(&self) -> Option<&'static ServerToolSpec> {
        sendable(self.wire, &self.versions)
    }

    pub fn server_tools(&self, form: SearchForm, runner: bool) -> &'static [ServerToolSpec] {
        match (form, self.native()) {
            (SearchForm::Native, Some(_)) if self.by_call && runner => &*BY_CALL,
            (SearchForm::Native, Some(spec)) => std::slice::from_ref(spec),
            _ => &[],
        }
    }

    pub fn opt_in_from(&self) -> Layer {
        self.setting_from
    }
}

pub fn hit_list(outcome: &SearchOutcome) -> String {
    match outcome {
        SearchOutcome::Results { hits } if hits.is_empty() => "no results".to_owned(),
        SearchOutcome::Results { hits } => wisp_tool_text::search::numbered(
            hits.iter()
                .map(|hit| (hit.title.as_str(), hit.url.as_str())),
        ),
        SearchOutcome::Failed { failure, code } => {
            format!("failed: {}", code.as_deref().unwrap_or(failure.as_str()))
        }
    }
}

#[cfg(test)]
mod tests;
