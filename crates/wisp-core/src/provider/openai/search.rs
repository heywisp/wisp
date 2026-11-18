use serde::Deserialize;
use serde_json::Value;
use wisp_protocol::{CitedSource, SearchAction, SearchFailure, SearchHit, SearchOutcome};

use crate::provider::SearchSignal;

pub(crate) const CALL: &str = "web_search_call";

fn action(item: &Value) -> SearchAction {
    item.pointer("/action/type")
        .and_then(|kind| SearchAction::deserialize(kind).ok())
        .unwrap_or(SearchAction::Other)
}

fn field<'a>(item: &'a Value, key: &str) -> Option<&'a str> {
    item.get("action")?.get(key)?.as_str()
}

pub(crate) fn subject(item: &Value) -> Option<String> {
    let queries = || {
        let queries = item.pointer("/action/queries")?.as_array()?;
        let joined: Vec<&str> = queries.iter().filter_map(Value::as_str).collect();
        Some(joined.join(", "))
    };
    match action(item) {
        SearchAction::OpenPage => field(item, "url").map(str::to_owned),
        SearchAction::FindInPage => field(item, "pattern").map(str::to_owned),
        SearchAction::Search | SearchAction::Other => {
            field(item, "query").map(str::to_owned).or_else(queries)
        }
    }
    .filter(|subject| !subject.is_empty())
}

fn hit(url: &str, title: &str) -> SearchHit {
    SearchHit {
        title: title.to_owned(),
        url: url.to_owned(),
        snippet: None,
        page_age: None,
    }
}

fn hits(item: &Value) -> Vec<SearchHit> {
    match action(item) {
        SearchAction::OpenPage => Vec::new(),
        SearchAction::FindInPage => field(item, "url")
            .map(|url| hit(url, ""))
            .into_iter()
            .collect(),
        SearchAction::Search | SearchAction::Other => item
            .pointer("/action/sources")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
            .filter_map(|source| {
                let text = |key| source.get(key).and_then(Value::as_str).unwrap_or_default();
                let (url, name) = (text("url"), text("name"));
                (!url.is_empty() || !name.is_empty()).then(|| hit(url, name))
            })
            .collect(),
    }
}

pub(crate) fn signals(item: &Value) -> Option<[SearchSignal; 2]> {
    if item.get("type").and_then(Value::as_str) != Some(CALL) {
        return None;
    }
    let call_id = item.get("id")?.as_str()?.to_owned();
    let outcome = if item.get("status").and_then(Value::as_str) == Some("failed") {
        SearchOutcome::Failed {
            failure: SearchFailure::Failed,
            code: None,
        }
    } else {
        SearchOutcome::Results { hits: hits(item) }
    };
    Some([
        SearchSignal::Started {
            call_id: call_id.clone(),
            query: subject(item).unwrap_or_default(),
            action: action(item),
        },
        SearchSignal::Ended { call_id, outcome },
    ])
}

pub(crate) fn cited(annotation: &Value) -> Option<CitedSource> {
    if annotation.get("type").and_then(Value::as_str) != Some("url_citation") {
        return None;
    }
    Some(CitedSource {
        url: annotation.get("url")?.as_str()?.to_owned(),
        title: annotation
            .get("title")
            .and_then(Value::as_str)
            .map(str::to_owned),
    })
}

pub(crate) fn requests<'a>(response: &Value, finalized: impl Iterator<Item = &'a Value>) -> u64 {
    if let Some(counted) = response
        .pointer("/tool_usage/web_search/num_requests")
        .and_then(Value::as_u64)
    {
        return counted;
    }
    let searched = finalized
        .filter(|item| item.get("type").and_then(Value::as_str) == Some(CALL))
        .filter(|item| item.get("status").and_then(Value::as_str) == Some("completed"))
        .filter(|item| action(item).is_search())
        .count();
    u64::try_from(searched).unwrap_or(u64::MAX)
}
