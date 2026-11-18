use std::collections::BTreeSet;

use serde_json::Value;
use wisp_protocol::{
    CitedSource, ContentBlock, SEARCH_CITATION, SEARCH_RESULT, SERVER_TOOL_USE, SearchAction,
    SearchFailure, SearchHit, SearchOutcome,
};

use super::ours;
use crate::provider::{SearchSignal, forms};
use crate::search::{WEB_SEARCH, hit_list};

const RESULT_ERROR: &str = "web_search_tool_result_error";
const HIT: &str = "web_search_result";
const MAX_CODE: usize = 64;

fn raw_of(block: &ContentBlock) -> Option<&Value> {
    forms::raw(ours(block)?)
}

fn kind(raw: &Value) -> Option<&str> {
    raw.get("type").and_then(Value::as_str)
}

fn text(raw: &Value, key: &str) -> Option<String> {
    raw.get(key).and_then(Value::as_str).map(str::to_owned)
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SearchCall {
    pub id: String,
    pub query: String,
}

pub fn search_call(block: &ContentBlock) -> Option<SearchCall> {
    let raw = raw_of(block)?;
    if kind(raw) != Some(SERVER_TOOL_USE)
        || raw.get("name").and_then(Value::as_str) != Some(WEB_SEARCH)
    {
        return None;
    }
    Some(SearchCall {
        id: text(raw, "id")?,
        query: raw
            .pointer("/input/query")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_owned(),
    })
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SearchResult {
    pub call_id: String,
    pub outcome: SearchOutcome,
}

pub fn search_result(block: &ContentBlock) -> Option<SearchResult> {
    let raw = raw_of(block)?;
    if kind(raw) != Some(SEARCH_RESULT) {
        return None;
    }
    Some(SearchResult {
        call_id: text(raw, "tool_use_id").filter(|id| !id.is_empty())?,
        outcome: outcome(raw.get("content")),
    })
}

fn unknown_shape() -> SearchOutcome {
    SearchOutcome::Failed {
        failure: SearchFailure::Failed,
        code: None,
    }
}

fn outcome(content: Option<&Value>) -> SearchOutcome {
    match content {
        Some(Value::Array(items)) => items
            .iter()
            .map(hit)
            .collect::<Option<Vec<SearchHit>>>()
            .map_or_else(unknown_shape, |hits| SearchOutcome::Results { hits }),
        Some(error) if kind(error) == Some(RESULT_ERROR) => {
            let code = error.get("error_code").and_then(Value::as_str);
            failed(code)
        }
        _ => unknown_shape(),
    }
}

fn hit(item: &Value) -> Option<SearchHit> {
    if kind(item) != Some(HIT) {
        return None;
    }
    Some(SearchHit {
        url: text(item, "url")?,
        title: text(item, "title").unwrap_or_default(),
        snippet: None,
        page_age: text(item, "page_age"),
    })
}

pub fn failure_of(code: &str) -> SearchFailure {
    match code {
        "too_many_requests" => SearchFailure::RateLimited,
        "invalid_tool_input" | "query_too_long" => SearchFailure::InvalidQuery,
        "unavailable" => SearchFailure::Unavailable,
        _ => SearchFailure::Failed,
    }
}

fn bounded(code: &str) -> bool {
    !code.is_empty()
        && code.len() <= MAX_CODE
        && code
            .chars()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_')
}

fn failed(code: Option<&str>) -> SearchOutcome {
    let Some(code) = code else {
        return unknown_shape();
    };
    SearchOutcome::Failed {
        failure: failure_of(code),
        code: bounded(code).then(|| code.to_owned()),
    }
}

pub fn cited_sources(block: &ContentBlock) -> Vec<CitedSource> {
    let Some(raw) = raw_of(block).filter(|raw| kind(raw) == Some("text")) else {
        return Vec::new();
    };
    let Some(Value::Array(citations)) = raw.get("citations") else {
        return Vec::new();
    };
    let mut sources: Vec<CitedSource> = Vec::new();
    for citation in citations
        .iter()
        .filter(|c| kind(c) == Some(SEARCH_CITATION))
    {
        let Some(url) = text(citation, "url") else {
            continue;
        };
        if sources.iter().all(|s| s.url != url) {
            sources.push(CitedSource {
                url,
                title: text(citation, "title"),
            });
        }
    }
    sources
}

pub fn signal(block: &ContentBlock) -> Option<SearchSignal> {
    if let Some(call) = search_call(block) {
        return Some(SearchSignal::Started {
            call_id: call.id,
            query: call.query,
            action: SearchAction::Search,
        });
    }
    if let Some(result) = search_result(block) {
        return Some(SearchSignal::Ended {
            call_id: result.call_id,
            outcome: result.outcome,
        });
    }
    let sources = cited_sources(block);
    (!sources.is_empty()).then_some(SearchSignal::Cited { sources })
}

pub fn closed_text(block: &ContentBlock, closed: &BTreeSet<String>) -> Option<String> {
    let raw = raw_of(block).filter(|raw| kind(raw) == Some(SERVER_TOOL_USE))?;
    let id = raw.get("id").and_then(Value::as_str)?;
    if !closed.contains(id) {
        return None;
    }
    let name = raw.get("name").and_then(Value::as_str).unwrap_or_default();
    Some(match search_call(block) {
        Some(call) => format!(
            "[web search for \"{}\": the turn ended before its result arrived]",
            call.query
        ),
        None => format!("[{name} call: the turn ended before its result arrived]"),
    })
}

pub fn rule_r(block: &ContentBlock) -> Option<Option<String>> {
    if let Some(call) = search_call(block) {
        return Some(Some(crate::search::call_line(&call.query)));
    }
    if let Some(result) = search_result(block) {
        return Some(Some(hit_list(&result.outcome)));
    }
    let raw = raw_of(block).filter(|raw| kind(raw) == Some("text"))?;
    let cited = raw
        .get("citations")
        .and_then(Value::as_array)
        .is_some_and(|c| c.iter().any(|c| kind(c) == Some(SEARCH_CITATION)));
    if !cited {
        return None;
    }
    let text = raw.get("text").and_then(Value::as_str).unwrap_or_default();
    Some((!text.trim().is_empty()).then(|| text.to_owned()))
}
