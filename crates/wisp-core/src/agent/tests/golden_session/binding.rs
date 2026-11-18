use std::collections::{BTreeMap, BTreeSet};

use super::*;
use crate::agent::search::WHOLE;
use crate::provider::RequestOptions;

const OPUS: &str = "claude-opus-5-5";
const SONNET: &str = "claude-sonnet-5-5";
const REPLAY_MINUTES: i64 = 30;
const MINUTES_PER_REQUEST: i64 = 8;
const SEARCH_OFF: &str = "web search is not enabled for this organization";
const UNREADABLE: &str =
    "messages.1.content.0: Invalid `encrypted_content` in `search_result` block";
const BINDING_REFUSED: &str = "thinking.block_binding: Extra inputs are not permitted";
const TTL_ORDER: &str =
    "a ttl='1h' cache_control block must not come after a ttl='5m' cache_control block";
const EDITED: &str = "messages: a thinking block in the conversation was modified";
const BOUND_ELSEWHERE: &str = "Invalid `signature` in `thinking` block. The block is bound to a different conversation. Remove the block, or set `thinking.block_binding.prefix_mismatch_behavior` to `drop_block` with the thinking-binding-controls-2026-08-01 beta header.";
const GATEWAY_KEY: &str = "sk-gateway";
const BASE_TOOLS: &[&str] = &["ask_user", "bash", "view_file"];
const MORE_TOOLS: &[&str] = &["ask_user", "bash", "show_diff", "view_file"];
const BAND: &str = "[models.\"claude-opus-5-5\"]\ncompact_at = 14500\n";
const BACKGROUND_TOKENS: u64 = 2_000;
const SUMMARY: &str = "## Goal and standing instructions\nLook around.\n\n## Decisions\nNone yet.\n\n## State of the work\nDone: a look around.\n\n## Files, branches and artefacts\nNone.\n\n## Commands, errors and fixes\nNone.\n\n## Open questions\nNone.";

fn compaction_content(request: u32) -> String {
    format!("{SUMMARY} ({request})")
}

#[derive(Debug, Clone)]
struct Served {
    body: String,
    status: u16,
    one_shot: bool,
    total: u64,
    read: u64,
    drops: Vec<String>,
    inserted: usize,
}

struct Produced {
    digest: String,
    model: String,
    before: Option<String>,
}

struct Source {
    content: Value,
    messages: Vec<Value>,
    chain: Option<String>,
}

struct Cached {
    key: Value,
    effort: Value,
    messages: Vec<Value>,
    total: u64,
    hour: bool,
}

#[derive(Default)]
struct Api {
    tools_per_turn: usize,
    refuse_search: bool,
    refuse_rendered: usize,
    search_once: bool,
    unreadable: bool,
    tampered: Option<(&'static str, BTreeSet<String>)>,
    refuse_next: VecDeque<Option<&'static str>>,
    refuse_binding_for: Option<&'static str>,
    fallback_to: Option<&'static str>,
    replay: Option<BTreeMap<String, (Vec<Value>, i64)>>,
    minutes: i64,
    produced: BTreeMap<String, Produced>,
    sources: Vec<Source>,
    previous: Option<Cached>,
    served: Vec<Served>,
    next: u32,
    cold: usize,
    iterations: bool,
    replay_for: Option<&'static str>,
    still: bool,
}

enum Shape {
    Search,
    Tool,
    Final,
    Summary,
    Compaction,
}

fn ttls(value: &Value, out: &mut Vec<String>) {
    match value {
        Value::Object(fields) => {
            if let Some(marker) = fields.get("cache_control") {
                out.push(marker["ttl"].as_str().unwrap_or("5m").to_owned());
            }
            for (key, field) in fields {
                if key != "cache_control" {
                    ttls(field, out);
                }
            }
        }
        Value::Array(items) => items.iter().for_each(|item| ttls(item, out)),
        _ => {}
    }
}

fn longer_after_shorter(request: &Value) -> bool {
    let mut order = Vec::new();
    for part in ["tools", "system", "messages"] {
        ttls(&request[part], &mut order);
    }
    order
        .iter()
        .skip_while(|ttl| *ttl != "5m")
        .any(|ttl| ttl == "1h")
}

fn unmarked(value: &Value) -> Value {
    match value {
        Value::Object(fields) => Value::Object(
            fields
                .iter()
                .filter(|(k, _)| *k != "cache_control")
                .map(|(k, v)| (k.clone(), unmarked(v)))
                .collect(),
        ),
        Value::Array(items) => Value::Array(items.iter().map(unmarked).collect()),
        other => other.clone(),
    }
}

fn thought(block: &Value) -> bool {
    matches!(
        block["type"].as_str(),
        Some("thinking" | "redacted_thinking")
    )
}

fn unthought(blocks: &[Value]) -> Vec<Value> {
    blocks.iter().filter(|b| !thought(b)).cloned().collect()
}

fn sends_thinking(body: &str) -> bool {
    let request: Value = serde_json::from_str(body).unwrap();
    request["messages"]
        .as_array()
        .into_iter()
        .flatten()
        .flat_map(content)
        .any(|b| thought(&b))
}

fn content(message: &Value) -> Vec<Value> {
    message["content"].as_array().cloned().unwrap_or_default()
}

fn prefix_digest(request: &Value, messages: &[Value], before: &[Value]) -> String {
    let messages: Vec<Value> = messages
        .iter()
        .map(|m| json!({"role": m["role"], "content": unthought(&content(m))}))
        .collect();
    let bytes = json!([
        request["system"],
        request["tools"],
        messages,
        unthought(before)
    ]);
    crate::session::sha256_hex(&bytes.to_string())
}

fn size(value: &Value) -> u64 {
    u64::try_from(value.to_string().len()).unwrap()
}

fn cache_betas(header: &str) -> Vec<&str> {
    header
        .split(',')
        .map(str::trim)
        .filter(|b| !b.is_empty() && crate::provider::anthropic::keys_cache(b))
        .collect()
}

fn header<'a>(head: &'a str, name: &str) -> &'a str {
    head.lines()
        .find_map(|line| {
            let (key, value) = line.split_once(':')?;
            key.trim()
                .eq_ignore_ascii_case(name)
                .then_some(value.trim())
        })
        .unwrap_or_default()
}

fn human(message: &Value) -> bool {
    message["role"] == "user" && content(message).iter().any(|b| b["type"] != "tool_result")
}

fn rendered_with_thinking(messages: &[Value]) -> bool {
    messages
        .iter()
        .filter(|m| m["role"] == "assistant")
        .map(content)
        .any(|blocks| {
            blocks.iter().any(thought)
                && blocks.iter().any(|b| {
                    b["type"] == "text"
                        && b["text"]
                            .as_str()
                            .is_some_and(|t| t.starts_with("[web search"))
                })
        })
}

fn summary_asked(messages: &[Value]) -> bool {
    messages.last().is_some_and(|m| {
        content(m).iter().any(|b| {
            b["text"]
                .as_str()
                .is_some_and(|t| t.starts_with("Write a summary"))
        })
    })
}

const SEARCH_RESULTS: u64 = 500;

fn searched_usage(total: u64, read: u64) -> Value {
    json!({
        "input_tokens": total - read + SEARCH_RESULTS,
        "cache_creation_input_tokens": 0,
        "cache_read_input_tokens": read + total,
        "output_tokens": 2,
        "iterations": [
            {"type": "message", "input_tokens": total - read, "cache_creation_input_tokens": 0, "cache_read_input_tokens": read, "output_tokens": 1},
            {"type": "message", "input_tokens": SEARCH_RESULTS, "cache_creation_input_tokens": 0, "cache_read_input_tokens": total, "output_tokens": 1}
        ]
    })
}

fn sse(index: usize, block: &Value, deltas: &[Value]) -> String {
    let mut out = block_start(index, block);
    for delta in deltas {
        out.push_str(&block_delta(index, delta));
    }
    out + &block_stop(index)
}

impl Api {
    fn answer(&mut self, head: &str, body: &str) -> String {
        let marked: Value = serde_json::from_str(body).unwrap();
        if longer_after_shorter(&marked) {
            return self.refuse(body, TTL_ORDER);
        }
        let mut request = unmarked(&marked);
        let one_shot = !body.contains("cache_control") || request.get("compaction").is_some();
        if !self.still {
            self.minutes += MINUTES_PER_REQUEST;
        }
        let model = request["model"].as_str().unwrap().to_owned();
        let binding = request["thinking"].get("block_binding").is_some();
        if binding
            && self
                .refuse_binding_for
                .is_some_and(|key| header(head, "x-api-key") == key)
        {
            return self.refuse(body, BINDING_REFUSED);
        }
        if self.refuse_rendered > 0
            && rendered_with_thinking(request["messages"].as_array().map_or(&[], Vec::as_slice))
        {
            self.refuse_rendered -= 1;
            return self.refuse(body, EDITED);
        }
        let searches = request["tools"]
            .as_array()
            .is_some_and(|t| t.iter().any(|t| t["name"] == "web_search"));
        if self.refuse_search && searches {
            return self.refuse(body, SEARCH_OFF);
        }
        if let Some(message) = self.refuse_next.pop_front().flatten() {
            return self.refuse(body, message);
        }
        if self.unreadable && body.contains("\"web_search_tool_result\"") {
            return self.refuse(body, UNREADABLE);
        }
        if let Some((message, stale)) = &self.tampered {
            let sent = request["messages"].as_array().into_iter().flatten();
            if sent
                .flat_map(content)
                .any(|b| thought(&b) && b["signature"].as_str().is_some_and(|s| stale.contains(s)))
            {
                let message = *message;
                return self.refuse(body, message);
            }
        }
        let staged = self
            .replay_for
            .is_none_or(|key| header(head, "x-api-key") == key);
        let inserted = if staged { self.replay(&mut request) } else { 0 };
        let messages = request["messages"].as_array().cloned().unwrap_or_default();
        let (drops, broken, chain) = match self.check(&request, &messages, &model, binding) {
            Ok(checked) => checked,
            Err(message) => return self.refuse(body, message),
        };
        if broken && !binding {
            let path = drops
                .iter()
                .find(|(_, reason)| reason == "prefix_binding_mismatch")
                .map_or("messages", |(path, _)| path.as_str());
            return self.refuse(body, &format!("{path}: {BOUND_ELSEWHERE}"));
        }
        let key = json!([
            model,
            request["system"],
            request["tools"],
            request["thinking"],
            cache_betas(header(head, "anthropic-beta"))
        ]);
        let effort = request["output_config"]["effort"].clone();
        let hour = marked["cache_control"]["ttl"] == "1h";
        let prefix = size(&request["system"]) + size(&request["tools"]);
        let total = prefix + size(&Value::Array(messages.clone()));
        let read = match &self.previous {
            _ if one_shot => 0,
            _ if self.cold > 0 => {
                self.cold -= 1;
                0
            }
            Some(c)
                if c.key == key
                    && c.effort == effort
                    && messages.starts_with(&c.messages)
                    && (c.hour || !hour) =>
            {
                c.total
            }
            Some(c) if c.key == key => prefix,
            _ => 0,
        };
        if !one_shot {
            self.previous = Some(Cached {
                key,
                effort,
                messages: messages.clone(),
                total,
                hour,
            });
        }
        let shape = if request.get("compaction").is_some() {
            Shape::Compaction
        } else if summary_asked(&messages) {
            Shape::Summary
        } else {
            let rounds = messages
                .iter()
                .rev()
                .take_while(|m| !human(m))
                .filter(|m| m["role"] == "assistant")
                .count();
            if rounds < self.tools_per_turn && std::mem::take(&mut self.search_once) {
                Shape::Search
            } else if rounds < self.tools_per_turn {
                Shape::Tool
            } else {
                Shape::Final
            }
        };
        let n = self.next;
        self.next += 1;
        let signature = format!("sig-{n}");
        let thinking = json!({"type": "thinking", "thinking": format!("Thought {n}."), "signature": signature});
        let replied = self.fallback_to.take().unwrap_or(model.as_str()).to_owned();
        if matches!(shape, Shape::Compaction) {
            self.sources.push(Source {
                content: json!(compaction_content(n)),
                messages: messages.clone(),
                chain,
            });
        } else {
            self.produced.insert(
                signature.clone(),
                Produced {
                    digest: prefix_digest(&request, &messages, &[]),
                    model: replied.clone(),
                    before: chain,
                },
            );
        }
        let tool_id = format!("toolu_{n:02}");
        let search_id = format!("srvtoolu_{n:02}");
        let after = format!("{signature}-after");
        let search_call = json!({"type": "server_tool_use", "id": search_id, "name": "web_search", "input": {"query": "rust"}});
        let search_result = json!({"type": "web_search_tool_result", "tool_use_id": search_id, "content": [{"type": "web_search_result", "title": "Rust", "url": "https://www.rust-lang.org"}]});
        let mut searched = vec![search_call];
        if self.iterations {
            searched.push(search_result.clone());
        }
        if matches!(shape, Shape::Search) {
            self.produced.insert(
                after.clone(),
                Produced {
                    digest: prefix_digest(&request, &messages, &searched),
                    model: replied.clone(),
                    before: Some(signature.clone()),
                },
            );
        }
        let stored = match shape {
            Shape::Tool => Some(vec![thinking.clone()]),
            Shape::Search => Some(vec![
                thinking.clone(),
                json!({"type": "thinking", "thinking": format!("Thought {n}, after."), "signature": after.clone()}),
            ]),
            _ => None,
        };
        if let (Some(stored), Some(replay), true) = (stored, self.replay.as_mut(), staged) {
            replay.insert(tool_id.clone(), (stored, self.minutes + REPLAY_MINUTES));
        }
        let listed: Vec<Value> = drops
            .iter()
            .map(|(path, reason)| json!({"type": "thinking_dropped", "path": path, "reason": reason}))
            .collect();
        self.served.push(Served {
            body: body.to_owned(),
            status: 200,
            one_shot,
            total,
            read,
            drops: drops.iter().map(|(p, r)| format!("{p} {r}")).collect(),
            inserted,
        });
        let mut start = json!({"type": "message_start", "message": {
            "id": format!("msg_{n:02}"), "type": "message", "role": "assistant", "model": model,
            "content": [], "stop_reason": null, "stop_sequence": null,
            "usage": {"input_tokens": total - read, "cache_creation_input_tokens": 0, "cache_read_input_tokens": read, "output_tokens": 1}
        }});
        if !listed.is_empty() {
            start["message"]["input_transformations"] = Value::Array(listed.clone());
        }
        if self.iterations && matches!(shape, Shape::Search) {
            start["message"]["usage"] = searched_usage(total, read);
        }
        let mut out = event("message_start", &start);
        let mut index = 0;
        if replied != model {
            out.push_str(&sse(index, &fallback_marker(&model, &replied), &[]));
            index += 1;
        }
        let think = |index: usize| {
            sse(
                index,
                &json!({"type": "thinking", "thinking": "", "signature": ""}),
                &[
                    json!({"type": "thinking_delta", "thinking": format!("Thought {n}.")}),
                    json!({"type": "signature_delta", "signature": signature}),
                ],
            )
        };
        let stop = match shape {
            Shape::Compaction => {
                out.push_str(&sse(
                    index,
                    &json!({"type": "compaction", "content": compaction_content(n)}),
                    &[],
                ));
                "compaction"
            }
            Shape::Summary => {
                out.push_str(&sse(
                    index,
                    &json!({"type": "text", "text": ""}),
                    &[json!({"type": "text_delta", "text": SUMMARY})],
                ));
                "end_turn"
            }
            Shape::Search => {
                let search_command = if self.iterations {
                    "true"
                } else {
                    "exec sleep 30"
                };
                out.push_str(&think(index));
                out.push_str(&sse(
                    index + 1,
                    &json!({"type": "server_tool_use", "id": search_id, "name": "web_search", "input": {}}),
                    &[json!({"type": "input_json_delta", "partial_json": "{\"query\":\"rust\"}"})],
                ));
                if self.iterations {
                    index += 1;
                    out.push_str(&sse(index + 1, &search_result, &[]));
                }
                out.push_str(&sse(
                    index + 2,
                    &json!({"type": "thinking", "thinking": "", "signature": ""}),
                    &[
                        json!({"type": "thinking_delta", "thinking": format!("Thought {n}, after.")}),
                        json!({"type": "signature_delta", "signature": after}),
                    ],
                ));
                out.push_str(&sse(
                    index + 3,
                    &json!({"type": "tool_use", "id": tool_id, "name": "bash", "input": {}}),
                    &[json!({"type": "input_json_delta", "partial_json": json!({"command": search_command}).to_string()})],
                ));
                "tool_use"
            }
            Shape::Tool => {
                out.push_str(&think(index));
                out.push_str(&sse(
                    index + 1,
                    &json!({"type": "tool_use", "id": tool_id, "name": "bash", "input": {}}),
                    &[json!({"type": "input_json_delta", "partial_json": "{\"command\":\"true\"}"})],
                ));
                "tool_use"
            }
            Shape::Final => {
                out.push_str(&think(index));
                out.push_str(&sse(
                    index + 1,
                    &json!({"type": "text", "text": ""}),
                    &[json!({"type": "text_delta", "text": format!("Done {n}.")})],
                ));
                "end_turn"
            }
        };
        let mut delta = json!({"type": "message_delta", "delta": {"stop_reason": stop, "stop_sequence": null}, "usage": {"output_tokens": 20}});
        if replied != model && !listed.is_empty() {
            delta["delta"]["input_transformations"] = Value::Array(listed);
        }
        out.push_str(&event("message_delta", &delta));
        out.push_str(&event("message_stop", json!({"type": "message_stop"})));
        http::sse(&out)
    }

    fn tamper(&mut self, message: &'static str) {
        self.tampered = Some((message, self.produced.keys().cloned().collect()));
    }

    fn refuse(&mut self, body: &str, message: &str) -> String {
        self.served.push(Served {
            body: body.to_owned(),
            status: 400,
            one_shot: false,
            total: 0,
            read: 0,
            drops: Vec::new(),
            inserted: 0,
        });
        let error = json!({"type": "error", "error": {"type": "invalid_request_error", "message": message}});
        http::json("400 Bad Request", &error.to_string())
    }

    fn replay(&mut self, request: &mut Value) -> usize {
        let Some(replay) = &self.replay else {
            return 0;
        };
        let mut inserted = 0;
        for message in request["messages"].as_array_mut().into_iter().flatten() {
            let blocks = content(message);
            let first_call = blocks
                .iter()
                .find(|b| b["type"] == "tool_use")
                .and_then(|b| b["id"].as_str());
            let Some(id) = first_call.filter(|_| !blocks.iter().any(thought)) else {
                continue;
            };
            if let Some((stored, until)) = replay.get(id).filter(|(_, until)| self.minutes < *until)
            {
                let _ = until;
                let mut filled = stored.clone();
                filled.extend(blocks.iter().cloned());
                inserted += stored.len();
                message["content"] = Value::Array(filled);
            }
        }
        inserted
    }

    #[allow(
        clippy::type_complexity,
        reason = "the drops, whether any broke, and the chain's last block"
    )]
    fn check(
        &self,
        request: &Value,
        messages: &[Value],
        model: &str,
        binding: bool,
    ) -> Result<(Vec<(String, String)>, bool, Option<String>), &'static str> {
        let mut drops = Vec::new();
        let mut broken = false;
        let mut chain: Option<String> = None;
        for (m, message) in messages.iter().enumerate() {
            let blocks = content(message);
            for (b, block) in blocks.iter().enumerate() {
                if !thought(block) {
                    continue;
                }
                let signature = block["signature"].as_str().unwrap_or_default();
                let Some(produced) = self.produced.get(signature) else {
                    return Err("messages: a thinking block's signature is not valid");
                };
                let path = format!("messages.{m}.content.{b}");
                if produced.model != model {
                    if binding {
                        drops.push((path, "model_binding_mismatch".to_owned()));
                    }
                    continue;
                }
                let digest = prefix_digest(request, &messages[..m], &blocks[..b]);
                let direct = digest == produced.digest && produced.before == chain;
                if direct || self.swapped(request, messages, (m, b), produced, chain.as_ref()) {
                    chain = Some(signature.to_owned());
                } else {
                    broken = true;
                    drops.push((path, "prefix_binding_mismatch".to_owned()));
                }
            }
        }
        Ok((drops, broken, chain))
    }

    fn swapped(
        &self,
        request: &Value,
        messages: &[Value],
        (m, b): (usize, usize),
        produced: &Produced,
        chain: Option<&String>,
    ) -> bool {
        let found = messages[..=m]
            .iter()
            .enumerate()
            .rev()
            .find_map(|(i, message)| {
                let blocks = content(message);
                let end = if i == m { b } else { blocks.len() };
                blocks[..end]
                    .iter()
                    .rposition(|block| block["type"] == "compaction")
                    .map(|k| (i, k, blocks))
            });
        let Some((c, k, swapped)) = found else {
            return false;
        };
        let rest = &swapped[k + 1..];
        let blocks = content(&messages[m]);
        self.sources
            .iter()
            .filter(|source| source.content == swapped[k]["content"])
            .any(|source| {
                let mut prefix = source.messages.clone();
                let before = if c == m {
                    &rest[..b - k - 1]
                } else {
                    if !rest.is_empty() {
                        prefix.push(json!({"role": messages[c]["role"], "content": rest}));
                    }
                    prefix.extend_from_slice(&messages[c + 1..m]);
                    &blocks[..b]
                };
                prefix_digest(request, &prefix, before) == produced.digest
                    && produced.before.as_ref() == chain.or(source.chain.as_ref())
            })
    }
}

#[derive(Clone, Copy)]
struct Build {
    tools: &'static [&'static str],
    search: bool,
    system: Option<&'static str>,
    before_binding: bool,
    models: Option<&'static str>,
    gateway: bool,
    band: bool,
    model: &'static str,
    instructions: Option<&'static str>,
    extra: Option<&'static str>,
    flags: &'static [(&'static str, &'static str, &'static str)],
    options: RequestOptions,
    stepped: bool,
}

impl Default for Build {
    fn default() -> Self {
        Self {
            tools: BASE_TOOLS,
            search: false,
            system: None,
            before_binding: false,
            models: None,
            gateway: false,
            band: false,
            model: OPUS,
            instructions: None,
            extra: None,
            flags: &[],
            options: RequestOptions {
                refusal_fallback: true,
                debug_cache: false,
                message_threads: true,
            },
            stepped: false,
        }
    }
}

fn flag_layers(flags: &[(&str, &str, &str)]) -> Vec<crate::settings::Given> {
    use wisp_protocol::settings::{SettingValue, Target, find};
    flags
        .iter()
        .map(|(origin, name, value)| crate::settings::Given {
            target: Target::Name(find(name).unwrap()),
            value: match *value {
                "true" => SettingValue::Bool(true),
                "false" => SettingValue::Bool(false),
                text => SettingValue::Text(text.to_owned()),
            },
            origin: (*origin).to_owned(),
        })
        .collect()
}

struct Bench {
    case: String,
    api: Arc<Mutex<Api>>,
    server: Server,
    env: Env,
    store: MemoryStore,
    steps: Arc<ShiftedClock>,
}

fn toolbox_of(names: &[&str], search: bool) -> crate::tool::Toolbox {
    use crate::kit::BuildTools;
    use futures::FutureExt;
    let mut on: BTreeSet<String> = names.iter().map(|n| (*n).to_owned()).collect();
    if search {
        on.insert(crate::search::WEB_SEARCH.to_owned());
    }
    let mut toolbox = crate::plugin::Tools::new(registry(Tools::Default), on)
        .start(
            crate::kit::ROOT,
            Arc::new(crate::tool::testkit::fake_capture_host()),
        )
        .now_or_never()
        .unwrap()
        .toolbox;
    toolbox.drop_plugin("web-search");
    toolbox
}

impl Bench {
    async fn new(case: &str, stage: bool) -> Self {
        let api = Arc::new(Mutex::new(Api {
            tools_per_turn: 2,
            replay: stage.then(BTreeMap::new),
            ..Api::default()
        }));
        let answering = api.clone();
        let server = Server::answering(Box::new(move |head: &str, body: &str| {
            answering.lock().unwrap().answer(head, body)
        }))
        .await;
        let case = format!("{case}-{}", if stage { "gateway" } else { "direct" });
        Self {
            env: Env::new(&case),
            case,
            api,
            server,
            store: MemoryStore::default(),
            steps: Arc::new(step_clock()),
        }
    }

    fn connection(&self, name: &str, gateway: bool) -> Connection {
        let provider: Arc<dyn Provider> = if gateway {
            Arc::new(
                AnthropicProvider::new(&Credential::ApiKey(GATEWAY_KEY.into()), &self.server.base)
                    .unwrap(),
            )
        } else {
            self.server.provider()
        };
        Connection {
            name: name.into(),
            provider,
            context: LogContext {
                endpoint_name: name.into(),
                ..log_context()
            },
            priced: true,
        }
    }

    fn agent(&self, log: SessionLog, build: Build, connections: Vec<Connection>) -> Agent {
        let mut session = default_session();
        session.model = build.model.into();
        if let Some(system) = build.system {
            session.system = Some(system.into());
        }
        let first = connections[0].clone();
        let tools = toolbox_of(build.tools, build.search);
        let web_search = tools.web_search();
        let models = build
            .models
            .or(build.band.then_some(BAND))
            .map_or_else(ModelSettings::shipped_only, models);
        let clock: Arc<dyn Clock> = if build.stepped {
            self.steps.clone()
        } else {
            Arc::new(fixed_clock())
        };
        let agent = Agent::new(first.provider.clone(), session)
            .with_clock(clock)
            .with_request_options(build.options)
            .with_connections(connections, Vec::new(), &first.name)
            .with_model_settings(models)
            .with_retry_policy(crate::agent::retry::quick())
            .with_count_tokens(false)
            .with_auto_compact(build.band)
            .with_start_layers({
                let mut layers = if build.band {
                    background_layers(BACKGROUND_TOKENS)
                } else {
                    crate::settings::host::StartLayers::default()
                };
                layers.flags = flag_layers(build.flags);
                layers
            })
            .with_web_search(web_search)
            .with_tools(tools, self.env.workspace())
            .with_platform("Linux")
            .with_resources(None);
        let agent = match build.instructions {
            Some(source) => agent.with_test_instructions(
                crate::agent::InstructionParts {
                    source: wisp_protocol::InstructionsSource::parse(source).unwrap(),
                    warn_tokens: crate::instructions::DEFAULT_WARN_TOKENS,
                    extra: build.extra.and_then(crate::instructions::Extra::named),
                    preset: None,
                },
                Some(&self.env.root.join("config")),
            ),
            None => agent,
        }
        .with_log(log, first.context.clone());
        if build.before_binding {
            agent.with_binding_controls(false)
        } else {
            agent
        }
    }

    fn connections(&self, build: Build) -> Vec<Connection> {
        if build.gateway {
            vec![self.connection("gw", true), self.connection("env", false)]
        } else {
            vec![self.connection("env", false)]
        }
    }

    fn start(&self, build: Build) -> Live {
        let log = SessionLog::new(Box::new(self.store.clone()), "s1");
        let agent = self.agent(log, build, self.connections(build));
        Live::on(agent, &self.server, self.steps.clone())
    }

    fn start_with(&self, build: Build, connections: Vec<Connection>) -> Live {
        let log = SessionLog::new(Box::new(self.store.clone()), "s1");
        Live::on(
            self.agent(log, build, connections),
            &self.server,
            self.steps.clone(),
        )
    }

    fn resume(&self, build: Build) -> Live {
        self.resume_with(build, self.connections(build))
    }

    fn resume_with(&self, build: Build, connections: Vec<Connection>) -> Live {
        let records = records(&self.store);
        let seq = u64::try_from(records.len()).unwrap();
        let log = SessionLog::continuing(Box::new(self.store.clone()), "s1", seq);
        let agent = self
            .agent(log, build, connections)
            .resume(&records, 0)
            .unwrap();
        Live::on(agent, &self.server, self.steps.clone())
    }

    fn both(&self) -> Vec<Connection> {
        vec![self.connection("env", false), self.connection("gw", true)]
    }

    fn served(&self) -> Vec<Served> {
        self.api.lock().unwrap().served.clone()
    }

    fn settled(&self) -> usize {
        settled(&self.served()).len()
    }

    fn cuts(&self) -> Vec<(BlockPosition, usize, Option<String>)> {
        pick!(
            log_events(&self.store),
            LogEvent::ThinkingCut {
                from,
                through,
                tools_sha256,
            } => (from, through, tools_sha256)
        )
    }
}

fn settled(served: &[Served]) -> Vec<Served> {
    served
        .iter()
        .filter(|s| s.status == 200 && !s.one_shot)
        .cloned()
        .collect()
}

fn prefix_parts(body: &str) -> (Value, Vec<Value>) {
    let request = unmarked(&serde_json::from_str::<Value>(body).unwrap());
    let messages = request["messages"].as_array().cloned().unwrap_or_default();
    let key = json!([
        request["model"],
        request["system"],
        request["tools"],
        request["thinking"]
    ]);
    (key, messages)
}

pub(super) fn extends(before: &str, after: &str) -> bool {
    let (key_before, sent_before) = prefix_parts(before);
    let (key_after, sent_after) = prefix_parts(after);
    key_before == key_after && sent_after.starts_with(&sent_before)
}

fn assert_settles(bench: &Bench, changes: &[usize]) {
    let served = bench.served();
    let ok = settled(&served);
    let case = &bench.case;
    assert!(ok.len() >= 2, "{case}: only {} requests", ok.len());
    for (at, pair) in ok.windows(2).enumerate().map(|(i, p)| (i + 1, p)) {
        if changes.contains(&at) {
            continue;
        }
        assert!(
            pair[1].read >= pair[0].total,
            "{case}: request {at} read {} of the previous prompt's {}",
            pair[1].read,
            pair[0].total
        );
        assert!(
            extends(&pair[0].body, &pair[1].body),
            "{case}: request {at} doesn't extend the one before it"
        );
    }
    let mut starts: Vec<usize> = changes.to_vec();
    starts.insert(0, 0);
    starts.push(ok.len());
    for span in starts.windows(2) {
        let first = &ok[span[0]].drops;
        for (at, served) in ok.iter().enumerate().take(span[1]).skip(span[0]) {
            assert_eq!(
                &served.drops, first,
                "{case}: request {at}'s drop list moved"
            );
        }
    }
    let inserted: usize = served.iter().map(|s| s.inserted).sum();
    assert_eq!(inserted, 0, "{case}: the gateway re-inserted thinking");
    if let Some(last) = changes.last() {
        let after = ok.len() - last;
        assert!(after >= 4, "{case}: only {after} requests after the change");
    }
}

fn assert_signed(case: &str, agent: &Agent) {
    for message in &*agent.history() {
        for block in &message.content {
            if let ContentBlock::Thinking { signature, .. } = block {
                assert!(!signature.is_empty(), "{case}: an unsigned thinking block");
            }
        }
    }
}

fn binds(body: &str) -> bool {
    serde_json::from_str::<Value>(body).unwrap()["thinking"]
        .get("block_binding")
        .is_some()
}

async fn two_turns(live: &mut Live, from: usize) {
    live.completed(&format!("u{from}"), "Look around.").await;
    live.completed(&format!("u{}", from + 1), "And again.")
        .await;
}

async fn finish(bench: &Bench, live: Live) -> Agent {
    finish_seen(bench, live).await.0
}

async fn finish_seen(bench: &Bench, live: Live) -> (Agent, Vec<AgentEvent>) {
    let (agent, seen) = live.close().await;
    assert_signed(&bench.case, &agent);
    (agent, seen)
}

fn adaptations(seen: &[AgentEvent]) -> Vec<wisp_protocol::HistoryChange> {
    pick!(seen, AgentEvent::HistoryAdapted { change, .. } => change.clone())
}

async fn switch_model(live: &mut Live, request: u64, model: &str) {
    live.send(ClientCommand::shortcut(
        RequestId(request),
        None,
        Some(model.to_owned()),
        None,
    ))
    .await;
    live.until(|e| {
        matches!(e, AgentEvent::SettingsChanged { answers: Some(RequestId(r)), .. } if *r == request)
    })
    .await;
}

async fn switch_endpoint(live: &mut Live, request: u64, endpoint: &str) {
    live.send(ClientCommand::shortcut(
        RequestId(request),
        Some(endpoint.to_owned()),
        None,
        None,
    ))
    .await;
    live.until(|e| {
        matches!(e, AgentEvent::SettingsChanged { answers: Some(RequestId(r)), .. } if *r == request)
    })
    .await;
}

async fn resumed_with(case: &str, stage: bool, later: Build) -> Agent {
    resumed_from(case, stage, Build::default(), later).await
}

async fn resumed_from(case: &str, stage: bool, first: Build, later: Build) -> Agent {
    let bench = Bench::new(case, stage).await;
    let mut live = bench.start(first);
    two_turns(&mut live, 1).await;
    finish(&bench, live).await;
    let change = bench.settled();
    let mut live = bench.resume(later);
    two_turns(&mut live, 3).await;
    live.completed("u5", "Once more.").await;
    let agent = finish(&bench, live).await;
    assert!(
        bench.cuts().is_empty(),
        "{}: {:?}",
        bench.case,
        bench.cuts()
    );
    assert_settles(&bench, &[change]);
    let ok = settled(&bench.served());
    assert!(ok.iter().all(|s| binds(&s.body)), "{}", bench.case);
    assert!(!ok[change].drops.is_empty(), "{}", bench.case);
    agent
}

fn notice(agent: &Agent) -> &str {
    agent.resume_notice_text().unwrap_or_default()
}

#[tokio::test]
async fn a_resume_that_adds_a_tool_misses_once_then_hits() {
    for stage in [false, true] {
        let later = Build {
            tools: MORE_TOOLS,
            ..Build::default()
        };
        let agent = resumed_with("binding-tool-added", stage, later).await;
        assert!(
            notice(&agent).contains(
                " This run's tools differ from the session's: added show_diff. Wisp's prompt or tools changed since this session: the cache is rebuilt"
            ),
            "{}",
            notice(&agent)
        );
    }
}

#[tokio::test]
async fn a_root_from_before_the_agent_and_checklist_tools_misses_once_on_resume_then_hits() {
    for stage in [false, true] {
        let first = Build {
            tools: &["ask_user", "bash", "show_diff", "view_file"],
            ..Build::default()
        };
        let later = Build {
            tools: &[
                "ask_user",
                "bash",
                "show_diff",
                "spawn_agent",
                "view_file",
                "write_todos",
            ],
            ..Build::default()
        };
        let bench = Bench::new("binding-agent-tools-on", stage).await;
        let mut live = bench.start(first);
        two_turns(&mut live, 1).await;
        finish(&bench, live).await;
        let change = bench.settled();
        let mut live = bench.resume(later);
        two_turns(&mut live, 3).await;
        live.completed("u5", "Once more.").await;
        let agent = finish(&bench, live).await;
        assert!(bench.cuts().is_empty(), "{:?}", bench.cuts());
        assert_settles(&bench, &[change]);
        let ok = settled(&bench.served());
        assert!(
            ok[change].read < ok[change].total,
            "{}: the changed request read {} of {}",
            bench.case,
            ok[change].read,
            ok[change].total
        );
        assert!(
            notice(&agent).contains(
                " This run's tools differ from the session's: added list_agents, rename_agent, send_message, spawn_agent, stop_agent, write_todos. Wisp's prompt or tools changed since this session: the cache is rebuilt"
            ),
            "{}",
            notice(&agent)
        );
    }
}

#[tokio::test]
async fn a_resume_that_removes_a_tool_misses_once_then_hits() {
    for stage in [false, true] {
        let first = Build {
            tools: MORE_TOOLS,
            ..Build::default()
        };
        let agent = resumed_from("binding-tool-removed", stage, first, Build::default()).await;
        assert!(
            notice(&agent).contains(
                " This run's tools differ from the session's: removed show_diff. Wisp's prompt or tools changed since this session"
            ),
            "{}",
            notice(&agent)
        );
    }
}

#[tokio::test]
async fn a_resume_with_tools_added_and_removed_names_both() {
    let first = Build {
        tools: &["ask_user", "bash"],
        ..Build::default()
    };
    let later = Build {
        tools: &["bash", "show_diff"],
        ..Build::default()
    };
    let agent = resumed_from("binding-tool-swapped", false, first, later).await;
    assert!(
        notice(&agent).contains(
            " This run's tools differ from the session's: added show_diff; removed ask_user."
        ),
        "{}",
        notice(&agent)
    );
}

#[tokio::test]
async fn a_resume_with_a_changed_system_prompt_misses_once_then_hits() {
    for stage in [false, true] {
        let later = Build {
            system: Some("You are Wisp, a careful assistant."),
            ..Build::default()
        };
        let agent = resumed_with("binding-system", stage, later).await;
        assert!(
            !notice(&agent).contains("tools differ"),
            "{}",
            notice(&agent)
        );
    }
}

#[tokio::test]
async fn a_plain_resume_never_misses() {
    for stage in [false, true] {
        let bench = Bench::new("binding-plain", stage).await;
        let mut live = bench.start(Build::default());
        two_turns(&mut live, 1).await;
        finish(&bench, live).await;
        let mut live = bench.resume(Build::default());
        two_turns(&mut live, 3).await;
        finish(&bench, live).await;
        assert_settles(&bench, &[]);
        assert!(settled(&bench.served()).iter().all(|s| s.drops.is_empty()));
    }
}

#[tokio::test]
async fn a_model_switch_there_and_back_misses_once_at_each_switch() {
    for stage in [false, true] {
        let bench = Bench::new("binding-model", stage).await;
        let mut live = bench.start(Build::default());
        two_turns(&mut live, 1).await;
        let there = bench.settled();
        switch_model(&mut live, 1, SONNET).await;
        two_turns(&mut live, 3).await;
        let back = bench.settled();
        switch_model(&mut live, 2, OPUS).await;
        two_turns(&mut live, 5).await;
        let (_, seen) = finish_seen(&bench, live).await;
        assert!(
            bench.cuts().is_empty(),
            "{}: {:?}",
            bench.case,
            bench.cuts()
        );
        assert_settles(&bench, &[there, back]);
        let ok = settled(&bench.served());
        assert!(
            ok[there]
                .drops
                .iter()
                .all(|d| d.ends_with("model_binding_mismatch"))
        );
        assert!(
            ok[back]
                .drops
                .iter()
                .all(|d| d.ends_with("model_binding_mismatch"))
        );
        let dropped = |at: usize| wisp_protocol::HistoryChange::ThinkingDropped {
            count: ok[at].drops.len(),
            reason: Some("model_binding_mismatch".into()),
        };
        assert_eq!(
            adaptations(&seen),
            vec![dropped(there), dropped(back)],
            "{}",
            bench.case
        );
    }
}

async fn refused_mid_session(bench: &Bench, build: Build) -> (Live, usize) {
    let mut live = bench.start(build);
    two_turns(&mut live, 1).await;
    let ok = settled(&bench.served());
    assert!(
        ok.iter().all(|s| s.body.contains("\"web_search\"")),
        "{}: the session never sent web search",
        bench.case
    );
    bench.api.lock().unwrap().refuse_search = true;
    let change = bench.settled();
    (live, change)
}

#[tokio::test]
async fn a_search_refused_mid_session_misses_once_then_hits() {
    for stage in [false, true] {
        let bench = Bench::new("binding-refused", stage).await;
        let build = Build {
            search: true,
            ..Build::default()
        };
        let (mut live, change) = refused_mid_session(&bench, build).await;
        two_turns(&mut live, 3).await;
        finish(&bench, live).await;
        assert!(
            bench.cuts().is_empty(),
            "{}: {:?}",
            bench.case,
            bench.cuts()
        );
        assert_settles(&bench, &[change]);
    }
}

#[tokio::test]
async fn a_break_in_the_last_turn_costs_no_miss_on_resume() {
    for (stage, before_binding) in [(false, false), (true, false), (false, true), (true, true)] {
        let bench = Bench::new(
            if before_binding {
                "binding-last-turn-old"
            } else {
                "binding-last-turn"
            },
            stage,
        )
        .await;
        let build = Build {
            search: true,
            before_binding,
            ..Build::default()
        };
        let (mut live, change) = refused_mid_session(&bench, build).await;
        live.completed("u3", "One more.").await;
        finish(&bench, live).await;
        let mut live = bench.resume(Build {
            before_binding: false,
            ..build
        });
        two_turns(&mut live, 4).await;
        finish(&bench, live).await;
        assert!(
            bench.cuts().is_empty(),
            "{}: {:?}",
            bench.case,
            bench.cuts()
        );
        assert_settles(&bench, &[change]);
    }
}

#[tokio::test]
async fn a_session_logged_before_binding_controls_turns_them_on_at_its_one_break() {
    for stage in [false, true] {
        let bench = Bench::new("binding-old-break", stage).await;
        let old = Build {
            before_binding: true,
            ..Build::default()
        };
        let mut live = bench.start(old);
        two_turns(&mut live, 1).await;
        finish(&bench, live).await;
        let change = bench.settled();
        let mut live = bench.resume(Build {
            tools: MORE_TOOLS,
            ..Build::default()
        });
        two_turns(&mut live, 3).await;
        finish(&bench, live).await;
        assert!(
            bench.cuts().is_empty(),
            "{}: {:?}",
            bench.case,
            bench.cuts()
        );
        assert_settles(&bench, &[change]);
        let ok = settled(&bench.served());
        assert!(ok[..change].iter().all(|s| !binds(&s.body)));
        assert!(ok[change..].iter().all(|s| binds(&s.body)));
    }
}

#[tokio::test]
async fn resuming_a_session_logged_before_binding_controls_keeps_them_off_and_hits() {
    for stage in [false, true] {
        let bench = Bench::new("binding-old-plain", stage).await;
        let old = Build {
            before_binding: true,
            ..Build::default()
        };
        let mut live = bench.start(old);
        two_turns(&mut live, 1).await;
        finish(&bench, live).await;
        let mut live = bench.resume(Build::default());
        two_turns(&mut live, 3).await;
        let (agent, _) = live.close().await;
        assert!(!agent.settings.binding_controls);
        assert_settles(&bench, &[]);
        assert!(settled(&bench.served()).iter().all(|s| !binds(&s.body)));
    }
}

async fn stopped_in_a_search(bench: &Bench, live: &mut Live, id: &str) {
    bench.api.lock().unwrap().search_once = true;
    live.send(ClientCommand::send(id, "Search, then wait."))
        .await;
    live.until(|e| matches!(e, AgentEvent::ToolCallStarted { .. }))
        .await;
    live.send(ClientCommand::Cancel).await;
    let outcome = live.ended().await;
    assert!(
        matches!(outcome, TurnOutcome::Cancelled { .. }),
        "{}: {outcome:?}",
        bench.case
    );
}

#[tokio::test]
async fn a_stop_in_the_last_turn_of_an_older_session_resumes_with_the_same_bytes() {
    for stage in [false, true] {
        let mut sent = Vec::new();
        for quit in [false, true] {
            let bench = Bench::new(
                if quit {
                    "binding-old-stop-resumed"
                } else {
                    "binding-old-stop-live"
                },
                stage,
            )
            .await;
            let build = Build {
                search: true,
                ..Build::default()
            };
            let mut live = bench.start(Build {
                before_binding: true,
                ..build
            });
            two_turns(&mut live, 1).await;
            finish(&bench, live).await;
            let mut live = bench.resume(build);
            live.completed("u3", "Look around.").await;
            let change = bench.settled() + 1;
            stopped_in_a_search(&bench, &mut live, "u4").await;
            if quit {
                finish(&bench, live).await;
                live = bench.resume(build);
            }
            two_turns(&mut live, 5).await;
            finish(&bench, live).await;
            let kinds: Vec<String> = log_events(&bench.store)
                .iter()
                .map(|e| serde_json::to_value(e).unwrap()["type"].to_string())
                .collect();
            assert!(
                kinds.iter().any(|k| k == "\"tail_closed\""),
                "{}: {kinds:?}",
                bench.case
            );
            assert!(
                bench.cuts().is_empty(),
                "{}: {:?}",
                bench.case,
                bench.cuts()
            );
            assert_settles(&bench, &[change]);
            let ok = settled(&bench.served());
            assert!(
                ok[change..].iter().all(|s| binds(&s.body)),
                "{}",
                bench.case
            );
            sent.push(ok[change].body.clone());
        }
        assert_eq!(
            sent[0], sent[1],
            "stage {stage}: the resumed request differs"
        );
    }
}

#[tokio::test]
async fn a_form_escalated_in_an_earlier_turn_of_an_older_session_keeps_binding_off_at_a_plain_resume()
 {
    let bench = Bench::new("binding-old-form", false).await;
    let old = Build {
        before_binding: true,
        ..Build::default()
    };
    let mut live = bench.start(old);
    two_turns(&mut live, 1).await;
    finish(&bench, live).await;
    bench.store.edit("s1", |records| {
        let at = records
            .iter()
            .position(|r| matches!(r.event, LogEvent::TurnEnded { .. }))
            .unwrap();
        let mut form = records[at].clone();
        form.event = LogEvent::RenderingForm {
            form: wisp_protocol::RenderingForm::WithoutThinking,
        };
        records.insert(at + 1, form);
        let base = records[0].seq;
        for (seq, record) in (base..).zip(records.iter_mut()) {
            record.seq = seq;
        }
    });
    let mut live = bench.resume(Build::default());
    two_turns(&mut live, 3).await;
    let (agent, _) = live.close().await;
    assert!(!agent.settings.binding_controls);
    assert_settles(&bench, &[]);
    assert!(settled(&bench.served()).iter().all(|s| !binds(&s.body)));
}

fn compactions(ok: &[Served]) -> Vec<usize> {
    let sizes: Vec<usize> = ok.iter().map(ok_messages).collect();
    (1..ok.len()).filter(|i| sizes[*i] < sizes[i - 1]).collect()
}

#[tokio::test]
async fn a_break_under_a_leading_native_head_misses_once_then_hits() {
    for stage in [false, true] {
        let bench = Bench::new("binding-native-head", stage).await;
        let banded = Build {
            band: true,
            ..Build::default()
        };
        let mut live = bench.start(banded);
        two_turns(&mut live, 1).await;
        live.completed("u3", "After the summary.").await;
        let agent = finish(&bench, live).await;
        assert!(
            agent.compaction.head.as_ref().is_some_and(|h| h.leading()),
            "{}",
            bench.case
        );
        let change = bench.settled();
        let mut live = bench.resume(Build {
            tools: MORE_TOOLS,
            ..Build::default()
        });
        two_turns(&mut live, 4).await;
        finish(&bench, live).await;
        assert!(
            bench.cuts().is_empty(),
            "{}: {:?}",
            bench.case,
            bench.cuts()
        );
        let mut changes = compactions(&settled(&bench.served()));
        changes.push(change);
        changes.sort_unstable();
        changes.dedup();
        assert_settles(&bench, &changes);
    }
}

#[tokio::test]
async fn a_refused_binding_trial_cuts_at_a_break_as_before() {
    let bench = Bench::new("binding-trial-refused", false).await;
    bench.api.lock().unwrap().refuse_binding_for = Some(GATEWAY_KEY);
    let gateway = Build {
        gateway: true,
        ..Build::default()
    };
    let mut live = bench.start(gateway);
    two_turns(&mut live, 1).await;
    finish(&bench, live).await;
    let refused = bench.served();
    assert_eq!(refused[0].status, 400, "{}", bench.case);
    assert!(binds(&refused[0].body));
    assert!(settled(&refused).iter().all(|s| !binds(&s.body)));
    let mut live = bench.resume(Build {
        tools: MORE_TOOLS,
        ..gateway
    });
    two_turns(&mut live, 3).await;
    finish(&bench, live).await;
    let cuts = bench.cuts();
    assert_eq!(cuts.len(), 1, "{}: {cuts:?}", bench.case);
    assert!(cuts[0].2.is_some());
    let ok = settled(&bench.served());
    assert!(ok.iter().all(|s| !binds(&s.body)));
}

#[tokio::test]
async fn a_kept_break_cuts_once_on_a_pair_that_refuses_binding_and_the_cut_stays() {
    let bench = Bench::new("binding-refusing-pair", false).await;
    bench.api.lock().unwrap().refuse_binding_for = Some(GATEWAY_KEY);
    let mut live = bench.start_with(Build::default(), bench.both());
    two_turns(&mut live, 1).await;
    finish(&bench, live).await;
    let kept = bench.settled();
    let later = Build {
        tools: MORE_TOOLS,
        ..Build::default()
    };
    let mut live = bench.resume_with(later, bench.both());
    live.completed("u3", "Keep going.").await;
    assert!(bench.cuts().is_empty());
    let away = bench.settled();
    switch_endpoint(&mut live, 1, "gw").await;
    two_turns(&mut live, 4).await;
    let cuts = bench.cuts();
    assert_eq!(cuts.len(), 1, "{}: {cuts:?}", bench.case);
    assert_eq!(
        cuts[0].0,
        BlockPosition {
            message: 0,
            block: 0
        }
    );
    assert_eq!(cuts[0].2, None);
    let home = bench.settled();
    switch_endpoint(&mut live, 2, "env").await;
    two_turns(&mut live, 6).await;
    let agent = finish(&bench, live).await;
    assert_eq!(bench.cuts().len(), 1, "{}", bench.case);
    assert_eq!(agent.search.kept_break, None);
    assert_settles(&bench, &[kept, away, home]);
    let ok = settled(&bench.served());
    let first_home: Value = serde_json::from_str(&ok[home].body).unwrap();
    let before_away = first_home["messages"].as_array().unwrap()[..ok_messages(&ok[away])]
        .iter()
        .flat_map(content)
        .filter(thought)
        .count();
    assert_eq!(before_away, 0, "{}: the cut came back", bench.case);
}

#[tokio::test]
async fn a_rewind_across_the_kept_cut_keeps_it_live_and_after_a_resume() {
    let bench = Bench::new("binding-rewind-cut", false).await;
    bench.api.lock().unwrap().refuse_binding_for = Some(GATEWAY_KEY);
    let mut live = bench.start_with(Build::default(), bench.both());
    two_turns(&mut live, 1).await;
    finish(&bench, live).await;
    let later = Build {
        tools: MORE_TOOLS,
        ..Build::default()
    };
    let mut live = bench.resume_with(later, bench.both());
    live.completed("u3", "Keep going.").await;
    switch_endpoint(&mut live, 1, "gw").await;
    live.completed("u4", "Over there.").await;
    assert_eq!(bench.cuts().len(), 1, "{}", bench.case);
    live.send(ClientCommand::Rewind {
        to: wisp_protocol::RewindPoint::Turn { turn: 4 },
        request: RequestId(2),
    })
    .await;
    let done = live
        .until(|e| {
            matches!(e, AgentEvent::Rewound { .. }) || matches!(e, AgentEvent::Rejected { .. })
        })
        .await;
    assert!(matches!(done, AgentEvent::Rewound { .. }), "{done:?}");
    live.completed("u5", "Over there, again.").await;
    let agent = finish(&bench, live).await;
    assert_eq!(
        agent
            .search
            .cuts
            .iter()
            .filter(|c| c.from.message == 0)
            .count(),
        1
    );
    let mut live = bench.resume_with(later, bench.both());
    live.completed("u6", "Still there.").await;
    let agent = finish(&bench, live).await;
    assert!(
        agent.search.cuts.iter().any(|c| c.from == WHOLE),
        "{}: the resumed session lost the cut of the whole history",
        bench.case
    );
    let served = bench.served();
    assert!(
        served.iter().all(|s| s.status == 200 || binds(&s.body)),
        "{}: a request was refused for a broken thinking block",
        bench.case
    );
    let ok = settled(&served);
    let last = ok.len() - 1;
    assert!(ok[last].read >= ok[last - 1].total, "{}", bench.case);
}

fn ok_messages(served: &Served) -> usize {
    serde_json::from_str::<Value>(&served.body).unwrap()["messages"]
        .as_array()
        .map_or(0, Vec::len)
}

#[tokio::test]
async fn compactions_after_a_kept_break_are_followed_by_hits() {
    for (stage, native) in [(false, true), (true, true), (false, false), (true, false)] {
        let bench = Bench::new(
            if native {
                "binding-compact-native"
            } else {
                "binding-compact-wisp"
            },
            stage,
        )
        .await;
        let models =
            (!native).then_some("[models.\"claude-opus-5-5\"]\nnative_compaction = false\n");
        let build = Build {
            models,
            ..Build::default()
        };
        let mut live = bench.start(build);
        two_turns(&mut live, 1).await;
        finish(&bench, live).await;
        let change = bench.settled();
        let mut live = bench.resume(Build {
            tools: MORE_TOOLS,
            ..build
        });
        live.completed("u3", "Keep going.").await;
        live.compacted(1).await;
        let compacted = bench.settled();
        two_turns(&mut live, 4).await;
        finish(&bench, live).await;
        assert!(
            bench.cuts().is_empty(),
            "{}: {:?}",
            bench.case,
            bench.cuts()
        );
        assert_settles(&bench, &[change, compacted]);
    }
}

#[tokio::test]
async fn a_rewind_after_a_kept_break_is_followed_by_hits() {
    for stage in [false, true] {
        let bench = Bench::new("binding-rewind", stage).await;
        let mut live = bench.start(Build::default());
        two_turns(&mut live, 1).await;
        finish(&bench, live).await;
        let change = bench.settled();
        let mut live = bench.resume(Build {
            tools: MORE_TOOLS,
            ..Build::default()
        });
        two_turns(&mut live, 3).await;
        live.send(ClientCommand::Rewind {
            to: wisp_protocol::RewindPoint::Turn { turn: 4 },
            request: RequestId(1),
        })
        .await;
        let done = live
            .until(|e| {
                matches!(e, AgentEvent::Rewound { .. }) || matches!(e, AgentEvent::Rejected { .. })
            })
            .await;
        assert!(matches!(done, AgentEvent::Rewound { .. }), "{done:?}");
        let rewound = bench.settled();
        two_turns(&mut live, 5).await;
        finish(&bench, live).await;
        assert!(
            bench.cuts().is_empty(),
            "{}: {:?}",
            bench.case,
            bench.cuts()
        );
        assert_settles(&bench, &[change, rewound]);
    }
}

#[tokio::test]
async fn a_refusal_fallback_listing_drops_in_its_message_delta_writes_no_cut() {
    for stage in [false, true] {
        let bench = Bench::new("binding-fallback", stage).await;
        let mut live = bench.start(Build::default());
        two_turns(&mut live, 1).await;
        finish(&bench, live).await;
        let change = bench.settled();
        let mut live = bench.resume(Build {
            tools: MORE_TOOLS,
            ..Build::default()
        });
        bench.api.lock().unwrap().fallback_to = Some(SONNET);
        two_turns(&mut live, 3).await;
        finish(&bench, live).await;
        assert!(
            bench.cuts().is_empty(),
            "{}: {:?}",
            bench.case,
            bench.cuts()
        );
        let ok = settled(&bench.served());
        assert!(!ok[change].drops.is_empty());
        let later = &ok[change + 1..];
        for (at, pair) in later.windows(2).enumerate() {
            assert!(
                pair[1].read >= pair[0].total,
                "{}: request {at}",
                bench.case
            );
        }
    }
}

mod breaks;
mod flags;
mod forms;
mod misses;
mod refused;

fn paid_compactions(bench: &Bench) -> usize {
    bench
        .served()
        .iter()
        .filter(|s| s.status == 200)
        .filter(|s| {
            serde_json::from_str::<Value>(&s.body)
                .unwrap()
                .get("compaction")
                .is_some()
        })
        .count()
}

#[tokio::test]
async fn a_compaction_that_cant_get_under_the_point_holds_until_the_point_is_raised() {
    let bench = Bench::new("auto-held", false).await;
    let mut live = bench.start(Build {
        band: true,
        tools: MORE_TOOLS,
        ..Build::default()
    });
    two_turns(&mut live, 1).await;
    live.completed("u3", "Keep going.").await;
    assert_eq!(paid_compactions(&bench), 1);
    let first = held(&live.seen);
    assert_eq!(first.len(), 1, "{first:?}");
    assert!(first[0].0 >= 14_500 && first[0].1 == 14_500, "{first:?}");
    live.send(ClientCommand::ChangeSettings {
        request: RequestId(1),
        changes: vec![
            wisp_protocol::settings::SettingChange::named(
                "compact_at",
                wisp_protocol::settings::SettingValue::Tokens(14_800),
            )
            .unwrap(),
        ],
        save: false,
    })
    .await;
    live.until(|e| {
        matches!(
            e,
            AgentEvent::SettingsChanged {
                answers: Some(RequestId(1)),
                ..
            }
        )
    })
    .await;
    two_turns(&mut live, 4).await;
    assert_eq!(paid_compactions(&bench), 2);
    let notices = held(&live.seen);
    assert_eq!(notices.len(), 2, "{notices:?}");
    assert_eq!(notices[1].1, 14_800);
    finish(&bench, live).await;
}

fn marks(bench: &Bench) -> Vec<String> {
    pick!(log_events(&bench.store), LogEvent::RebuildMarked { reason, .. } => reason)
}
