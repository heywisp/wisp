use std::collections::{BTreeMap, BTreeSet};
use std::time::Duration;

use futures::StreamExt;
use futures::stream::{self, BoxStream};
use reqwest::Client;
use reqwest::header::LOCATION;
use serde_json::{Value, json};
use tokio::time::{Instant, timeout_at};
use wisp_protocol::SearchOutcome;
use wisp_tool_text::search::{QUERY, REPORT, schema};

use super::super::sse::Item;
use super::super::{ProviderEvent, SearchSignal};
use crate::search::WEB_SEARCH;

pub(super) const CALL: &str = "search_call";
pub(super) const RESULT: &str = "search_result";
pub(super) const REDIRECTS: &str =
    "https://vertexaisearch.cloud.google.com/grounding-api-redirect/";
pub(super) const FOLLOW_WAIT: Duration = Duration::from_secs(3);
const FOLLOWS_AT_ONCE: usize = 8;
const MAX_ADDRESS: usize = 2048;

pub(super) fn declaration() -> Value {
    json!({
        "name": WEB_SEARCH,
        "description": format!("Search the web with Google Search. {REPORT}"),
        "parametersJsonSchema": serde_json::from_str::<Value>(&schema(QUERY)).unwrap_or_default(),
    })
}

pub(super) fn query_of(item: &Value) -> &str {
    item.pointer("/part/functionCall/args/query")
        .and_then(Value::as_str)
        .unwrap_or_default()
}

#[cfg(test)]
pub(super) fn call_item(call_id: &str, part: &Value) -> Value {
    json!({"type": CALL, "call_id": call_id, "part": part})
}

pub(super) fn response_part(item: &Value) -> Option<Value> {
    (item["type"] == RESULT).then(|| {
        let mut response = json!({
            "name": item["name"],
            "response": {"output": item["output"], "is_error": item["is_error"]},
        });
        if !item["id"].is_null() {
            response["id"] = item["id"].clone();
        }
        json!({"functionResponse": response})
    })
}

pub(in crate::provider) fn as_text(item: &Value) -> Option<String> {
    match item["type"].as_str()? {
        CALL => Some(crate::search::call_line(query_of(item))),
        RESULT => item["output"].as_str().map(str::to_owned),
        _ => None,
    }
}

pub(super) fn real_addresses(
    events: BoxStream<'static, Item>,
    client: Client,
    redirects: String,
    wait: Duration,
) -> BoxStream<'static, Item> {
    let state = (events, client, redirects, BTreeMap::new());
    stream::unfold(
        state,
        move |(mut events, client, redirects, mut known)| async move {
            let mut item = events.next().await?;
            if let Ok(ProviderEvent::Search { signal, .. }) = &mut item {
                let texts: Vec<&mut String> = match signal {
                    SearchSignal::Ended {
                        outcome: SearchOutcome::Results { hits },
                        ..
                    } => hits
                        .iter_mut()
                        .flat_map(|hit| [&mut hit.url, &mut hit.title])
                        .collect(),
                    SearchSignal::Cited { sources } => sources
                        .iter_mut()
                        .flat_map(|source| {
                            std::iter::once(&mut source.url).chain(&mut source.title)
                        })
                        .collect(),
                    _ => Vec::new(),
                };
                swap(&client, &redirects, wait, texts, &mut known).await;
            }
            Some((item, (events, client, redirects, known)))
        },
    )
    .boxed()
}

async fn swap(
    client: &Client,
    redirects: &str,
    wait: Duration,
    texts: Vec<&mut String>,
    known: &mut BTreeMap<String, String>,
) {
    let fresh: BTreeSet<String> = texts
        .iter()
        .filter(|text| text.starts_with(redirects) && !known.contains_key(text.as_str()))
        .map(|text| String::clone(text))
        .collect();
    known.extend(fresh.iter().map(|url| (url.clone(), url.clone())));
    let deadline = Instant::now() + wait;
    let mut follows = stream::iter(fresh)
        .map(|url| async move { (follow(client, &url).await, url) })
        .buffer_unordered(FOLLOWS_AT_ONCE);
    while let Ok(Some((address, url))) = timeout_at(deadline, follows.next()).await {
        known.extend(address.map(|address| (url, address)));
    }
    for text in texts {
        if let Some(address) = known.get(text.as_str()) {
            address.clone_into(text);
        }
    }
}

async fn follow(client: &Client, url: &str) -> Option<String> {
    let response = client.head(url).send().await.ok()?;
    let location = response.headers().get(LOCATION)?.to_str().ok()?;
    (response.status().is_redirection() && page_address(location)).then(|| location.to_owned())
}

fn page_address(location: &str) -> bool {
    location.len() <= MAX_ADDRESS
        && !location
            .chars()
            .any(|c| c.is_control() || c.is_whitespace())
        && reqwest::Url::parse(location).is_ok_and(|url| {
            matches!(url.scheme(), "http" | "https")
                && url.has_host()
                && url.username().is_empty()
                && url.password().is_none()
        })
}
