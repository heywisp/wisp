use std::io::{BufReader, Write};
use std::net::TcpListener;
use std::sync::Arc;
use std::time::Duration;

use futures::stream::BoxStream;
use serde_json::json;
use tokio::sync::mpsc;
use wisp_core::agent::{Agent, Connection, LogContext, default_session};
use wisp_core::provider::anthropic::{
    AnthropicProvider, Credential as Key, PER_MESSAGE_EFFORT_BETA,
};
use wisp_core::provider::{ModelCapabilities, Provider, ProviderError, ProviderEvent, Request};
use wisp_core::session::Redactions;
use wisp_protocol::log::CredentialSource;
use wisp_protocol::{
    AgentEvent, BlockKind, CacheEffect, CacheTtl, ClientCommand, ContentBlock, Effort, Feature,
    NoSearch, RebuildReason, RequestId, SearchForm, TurnOutcome, TurnStats,
};
use wisp_render::chrome::{Effect, Endpoint, EndpointFacts};
use wisp_render::theme::glyphs::{GlyphSet, Glyphs};
use wisp_tui::app::picker::{Picked, Picker, switch_to};
use wisp_tui::app::settings::Tracker;
use wisp_tui::app::{Credential, SessionFacts};

use super::{Chosen, facts};
use crate::server::read_request;
use crate::turn_lines::with_turn_lines;

const OPUS: &str = "claude-opus-5-5";
const OPUS_4_8: &str = "claude-opus-4-8";
const SONNET: &str = "claude-sonnet-5-5";
const UNI: GlyphSet = GlyphSet::Unicode;
const SSE_HEAD: &str =
    "HTTP/1.1 200 OK\r\ncontent-type: text/event-stream\r\nconnection: close\r\n\r\n";
const SUMMARY: &str = "So far: two files and one failing test.";
const WAIT: Duration = Duration::from_secs(10);

type Respond = dyn Fn(&str, usize) -> String + Send + Sync;

fn event(name: &str, data: serde_json::Value) -> String {
    format!("event: {name}\ndata: {data}\n\n")
}

fn answer(model: &str, n: usize) -> String {
    filled_answer(model, n, 9_100)
}

fn filled_answer(model: &str, n: usize, fill: u64) -> String {
    let usage = json!({
        "input_tokens": 100,
        "output_tokens": 1,
        "cache_read_input_tokens": fill - 9_100,
        "cache_creation_input_tokens": 9_000,
        "cache_creation": {"ephemeral_5m_input_tokens": 0, "ephemeral_1h_input_tokens": 9_000},
    });
    let mut body = event(
        "message_start",
        json!({"type": "message_start", "message": {"id": format!("msg_{n}"), "model": model, "usage": usage}}),
    );
    body += &event(
        "content_block_start",
        json!({"type": "content_block_start", "index": 0, "content_block": {"type": "text", "text": ""}}),
    );
    body += &event(
        "content_block_delta",
        json!({"type": "content_block_delta", "index": 0, "delta": {"type": "text_delta", "text": format!("answer {n}")}}),
    );
    body += &event(
        "content_block_stop",
        json!({"type": "content_block_stop", "index": 0}),
    );
    body += &event(
        "message_delta",
        json!({"type": "message_delta", "delta": {"stop_reason": "end_turn"}, "usage": {"output_tokens": 3}}),
    );
    body += &event("message_stop", json!({"type": "message_stop"}));
    format!("{SSE_HEAD}{body}")
}

fn thinking(model: &str, n: usize, finished: bool) -> String {
    let usage = json!({"input_tokens": 100, "output_tokens": 1, "cache_read_input_tokens": 0, "cache_creation_input_tokens": 9_000});
    let mut body = event(
        "message_start",
        json!({"type": "message_start", "message": {"id": format!("msg_{n}"), "model": model, "usage": usage}}),
    );
    body += &event(
        "content_block_start",
        json!({"type": "content_block_start", "index": 0, "content_block": {"type": "thinking", "thinking": ""}}),
    );
    body += &event(
        "content_block_delta",
        json!({"type": "content_block_delta", "index": 0, "delta": {"type": "thinking_delta", "thinking": "Weighing it up."}}),
    );
    if !finished {
        return format!("stall:{SSE_HEAD}{body}");
    }
    body += &event(
        "content_block_delta",
        json!({"type": "content_block_delta", "index": 0, "delta": {"type": "signature_delta", "signature": "c2lnbmVk"}}),
    );
    body += &event(
        "content_block_stop",
        json!({"type": "content_block_stop", "index": 0}),
    );
    body += &event(
        "content_block_start",
        json!({"type": "content_block_start", "index": 1, "content_block": {"type": "text", "text": ""}}),
    );
    body += &event(
        "content_block_delta",
        json!({"type": "content_block_delta", "index": 1, "delta": {"type": "text_delta", "text": "done"}}),
    );
    body += &event(
        "content_block_stop",
        json!({"type": "content_block_stop", "index": 1}),
    );
    body += &event(
        "message_delta",
        json!({"type": "message_delta", "delta": {"stop_reason": "end_turn"}, "usage": {"output_tokens": 3}}),
    );
    body += &event("message_stop", json!({"type": "message_stop"}));
    format!("{SSE_HEAD}{body}")
}

fn rejection(message: &str) -> String {
    let body =
        json!({"type": "error", "error": {"type": "invalid_request_error", "message": message}})
            .to_string();
    format!(
        "HTTP/1.1 400 Bad Request\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{body}",
        body.len()
    )
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Reply {
    Compacts,
    Stalls,
    Refuses,
    Errors,
    Empty,
}

fn compaction(model: &str, reply: Reply) -> String {
    let usage = json!({"input_tokens": 400, "output_tokens": 1, "cache_read_input_tokens": 9_000, "cache_creation_input_tokens": 0});
    let mut body = event(
        "message_start",
        json!({"type": "message_start", "message": {"id": "msg_c", "model": model, "usage": usage}}),
    );
    match reply {
        Reply::Stalls => return format!("stall:{SSE_HEAD}{body}"),
        Reply::Errors => return rejection("the mock refuses"),
        Reply::Empty => {
            body += &event(
                "message_delta",
                json!({"type": "message_delta", "delta": {"stop_reason": "end_turn"}, "usage": {"output_tokens": 3}}),
            );
        }
        Reply::Refuses => {
            body += &event(
                "message_delta",
                json!({"type": "message_delta", "delta": {"stop_reason": "refusal"}, "usage": {"output_tokens": 3}}),
            );
        }
        Reply::Compacts => {
            body += &event(
                "content_block_start",
                json!({"type": "content_block_start", "index": 0, "content_block": {"type": "compaction", "content": SUMMARY}}),
            );
            body += &event(
                "content_block_stop",
                json!({"type": "content_block_stop", "index": 0}),
            );
            body += &event(
                "message_delta",
                json!({"type": "message_delta", "delta": {"stop_reason": "compaction"}, "usage": {"output_tokens": 300}}),
            );
        }
    }
    body += &event("message_stop", json!({"type": "message_stop"}));
    format!("{SSE_HEAD}{body}")
}

fn model_of(request: &str) -> String {
    let body = request.split("\r\n\r\n").nth(1).unwrap_or_default();
    serde_json::from_str::<serde_json::Value>(body)
        .ok()
        .and_then(|v| v["model"].as_str().map(str::to_owned))
        .unwrap_or_default()
}

fn gateway(respond: Arc<Respond>) -> String {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let addr = listener.local_addr().unwrap();
    std::thread::spawn(move || {
        for (n, sock) in listener.incoming().enumerate() {
            let Ok(mut sock) = sock else {
                return;
            };
            let respond = Arc::clone(&respond);
            std::thread::spawn(move || {
                let mut reader = BufReader::new(sock.try_clone().unwrap());
                let Some(seen) = read_request(&mut reader) else {
                    return;
                };
                let reply = respond(&format!("{}\r\n{}", seen.head, seen.body), n);
                if let Some(rest) = reply.strip_prefix("delay:") {
                    std::thread::sleep(Duration::from_millis(150));
                    sock.write_all(rest.as_bytes()).ok();
                } else if let Some(rest) = reply.strip_prefix("stall:") {
                    sock.write_all(rest.as_bytes()).ok();
                    sock.flush().ok();
                    std::thread::sleep(Duration::from_secs(10));
                } else {
                    sock.write_all(reply.as_bytes()).ok();
                }
            });
        }
    });
    format!("http://{addr}")
}

struct Documented(AnthropicProvider);

impl Provider for Documented {
    fn stream(
        &self,
        request: &Request<'_>,
    ) -> BoxStream<'static, Result<ProviderEvent, ProviderError>> {
        self.0.stream(request)
    }

    fn echoed<'a>(
        &self,
        content: &'a [ContentBlock],
        earlier: &wisp_core::provider::ServerCalls,
    ) -> Vec<&'a ContentBlock> {
        self.0.echoed(content, earlier)
    }

    fn search_signal(&self, block: &ContentBlock) -> Option<wisp_core::provider::SearchSignal> {
        self.0.search_signal(block)
    }

    fn pause_limit(&self) -> u32 {
        self.0.pause_limit()
    }

    fn capabilities(&self, model: &str) -> Option<ModelCapabilities> {
        self.0.capabilities(model)
    }

    fn features_documented(&self) -> bool {
        true
    }
    fn account_bound_thinking(&self, model: &str) -> bool {
        self.0.account_bound_thinking(model)
    }

    fn cache_prefix(&self, request: &Request<'_>) -> Option<String> {
        self.0.cache_prefix(request)
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Row {
    Model,
    Effort,
}

struct Harness {
    agent: Agent,
    tracker: Tracker,
    facts: SessionFacts,
    checked: usize,
}

fn provider(endpoint: Endpoint, respond: Arc<Respond>) -> (Arc<dyn Provider>, String) {
    let base = gateway(respond);
    let inner = AnthropicProvider::new(&Key::ApiKey("sk-test".into()), &base).unwrap();
    let host = base.trim_start_matches("http://").to_owned();
    let provider: Arc<dyn Provider> = match endpoint {
        Endpoint::Official => Arc::new(Documented(inner)),
        Endpoint::Custom => Arc::new(inner),
    };
    (provider, host)
}

const UNAVAILABLE: &str = "work";

impl Harness {
    fn new(
        endpoint: Endpoint,
        model: &str,
        effort: Effort,
        respond: impl Fn(&str, usize) -> String + Send + Sync + 'static,
    ) -> Harness {
        let (provider, _) = provider(endpoint, Arc::new(respond));
        Harness::build(provider, endpoint, model, effort, |a| a)
    }

    fn build(
        provider: Arc<dyn Provider>,
        endpoint: Endpoint,
        model: &str,
        effort: Effort,
        connect: impl FnOnce(Agent) -> Agent,
    ) -> Harness {
        let mut session = default_session();
        session.model = model.to_owned();
        session.effort = effort;
        let facts = facts(
            Chosen {
                model,
                effort: effort.as_str(),
                cache_ttl: CacheTtl::OneHour,
                endpoint,
                max_tokens: session.max_tokens,
                claude_usage: false,
            },
            None,
            None,
            None,
        );
        Harness {
            agent: connect(with_turn_lines(Agent::new(provider, session))),
            tracker: Tracker::new(&facts),
            facts,
            checked: 0,
        }
    }

    fn feed(&mut self, events: &[AgentEvent]) {
        for e in events {
            self.tracker.apply(e, &self.facts, Duration::ZERO, UNI);
        }
        self.tracker.show(&mut self.facts);
    }

    fn endpoints(
        sides: &[(&str, Endpoint)],
        start: &str,
        model: &str,
        effort: Effort,
        respond: impl Fn(&str, usize) -> String + Send + Sync + 'static,
    ) -> Harness {
        let respond: Arc<Respond> = Arc::new(respond);
        let mut connections = vec![];
        let mut listed = vec![];
        for (name, reach) in sides {
            let (provider, host) = provider(*reach, Arc::clone(&respond));
            connections.push(Connection {
                name: (*name).to_owned(),
                provider,
                context: LogContext {
                    endpoint: format!("http://{host}"),
                    endpoint_name: (*name).to_owned(),
                    credential_source: CredentialSource::Environment,
                    redactions: Redactions::default(),
                },
                priced: true,
            });
            listed.push(EndpointFacts {
                models: Vec::new(),
                mark: Some(1),
                name: (*name).to_owned(),
                provider: "anthropic".to_owned(),
                credential: Some(Credential::ApiKey),
                reach: Some(*reach),
                host: (*reach == Endpoint::Custom).then_some(host),
                unavailable: None,
                priced: true,
                expired: false,
                key_var: None,
                complete: false,
                default_model: None,
            });
        }
        listed.push(EndpointFacts {
            models: Vec::new(),
            mark: Some(2),
            name: UNAVAILABLE.to_owned(),
            provider: "anthropic".to_owned(),
            credential: Some(Credential::ApiKey),
            reach: Some(Endpoint::Official),
            host: None,
            unavailable: Some("WORK_KEY is not set".to_owned()),
            priced: true,
            expired: false,
            key_var: None,
            complete: false,
            default_model: None,
        });
        listed.sort_by(|a, b| a.name.cmp(&b.name));
        let first = connections
            .iter()
            .find(|c| c.name == start)
            .map(|c| c.provider.clone())
            .unwrap();
        let reach = sides.iter().find(|(n, _)| *n == start).unwrap().1;
        let unavailable = vec![(UNAVAILABLE.to_owned(), "WORK_KEY is not set".to_owned())];
        let mut h = Harness::build(first, reach, model, effort, |agent| {
            agent.with_connections(connections, unavailable, start)
        });
        h.facts.endpoint_name = Some(start.to_owned());
        h.facts.endpoints = listed;
        h.tracker = Tracker::new(&h.facts);
        h
    }

    fn searching(self, sides: &[(&str, Endpoint)], config: Option<&str>) -> Harness {
        let Harness {
            agent,
            tracker,
            mut facts,
            checked,
        } = self;
        let listed: Vec<_> = sides
            .iter()
            .map(|(name, _)| {
                (
                    (*name).to_owned(),
                    wisp_core::endpoints::ProviderKind::Anthropic,
                )
            })
            .collect();
        let ids: Vec<String> = facts.models.iter().map(|m| m.id.clone()).collect();
        let loaded = crate::context::load_text(config);
        facts.search = crate::context::search_forms(&loaded, &listed, &ids, true, true, None, &[]);
        Harness {
            agent: agent
                .with_model_settings(loaded.settings)
                .with_web_search(true),
            tracker,
            facts,
            checked,
        }
    }

    fn search_agrees(&mut self, label: &str) {
        let endpoints: Vec<String> = self
            .facts
            .endpoints
            .iter()
            .filter(|e| e.available())
            .map(|e| e.name.clone())
            .collect();
        let mut ids: Vec<String> = self.facts.models.iter().map(|m| m.id.clone()).collect();
        ids.push("my-model".to_owned());
        for endpoint in &endpoints {
            for id in &ids {
                let kernel = crate::context::shown_form(self.agent.search_form(endpoint, id));
                let shown = self.tracker.search_form(&self.facts, endpoint, id);
                assert_eq!(shown, kernel, "{label}: {endpoint} {id}");
                self.checked += 1;
            }
        }
    }

    async fn ask(&mut self, model: Option<&str>, effort: Option<Effort>) -> Vec<AgentEvent> {
        self.ask_on(None, model, effort).await
    }

    async fn ask_on(
        &mut self,
        endpoint: Option<&str>,
        model: Option<&str>,
        effort: Option<Effort>,
    ) -> Vec<AgentEvent> {
        let (tx, mut rx) = mpsc::channel(64);
        self.agent
            .change_settings(
                endpoint.map(str::to_owned),
                model.map(str::to_owned),
                effort,
                &tx,
            )
            .await;
        drop(tx);
        let mut events = vec![];
        while let Some(e) = rx.recv().await {
            events.push(e);
        }
        events
    }

    async fn change(&mut self, model: Option<&str>, effort: Option<Effort>) -> CacheEffect {
        let events = self.ask(model, effort).await;
        self.feed(&events);
        cache_of(&events).unwrap()
    }

    async fn turn(&mut self, on_step: Vec<ClientCommand>) -> Vec<AgentEvent> {
        let (cmd_tx, mut cmd_rx) = mpsc::channel(8);
        let (ev_tx, mut ev_rx) = mpsc::channel(1024);
        let collector = tokio::spawn(async move {
            let mut seen = vec![];
            let mut pending = Some(on_step);
            while let Some(e) = ev_rx.recv().await {
                if matches!(e, AgentEvent::StepStarted { .. })
                    && let Some(commands) = pending.take()
                {
                    for c in commands {
                        cmd_tx.send(c).await.unwrap();
                    }
                }
                seen.push(e);
            }
            seen
        });
        self.agent
            .run_turn("go".to_owned(), &mut cmd_rx, &ev_tx)
            .await
            .unwrap();
        drop(ev_tx);
        let events = collector.await.unwrap();
        self.feed(&events);
        events
    }

    async fn whole_turn(&mut self, stop_on_thinking: bool) -> Vec<AgentEvent> {
        let (cmd_tx, mut cmd_rx) = mpsc::channel(8);
        let (ev_tx, mut ev_rx) = mpsc::channel(1024);
        let collector = tokio::spawn(async move {
            let mut seen = vec![];
            while let Some(e) = ev_rx.recv().await {
                let thinking = matches!(
                    e,
                    AgentEvent::BlockStarted {
                        kind: BlockKind::Thinking,
                        ..
                    }
                );
                if stop_on_thinking && thinking {
                    cmd_tx.send(ClientCommand::Cancel).await.unwrap();
                }
                seen.push(e);
            }
            seen
        });
        let outcome = self
            .agent
            .run_turn("go".to_owned(), &mut cmd_rx, &ev_tx)
            .await
            .unwrap();
        drop(ev_tx);
        let mut events = collector.await.unwrap();
        events.push(AgentEvent::TurnEnded {
            outcome,
            stats: TurnStats::default(),
        });
        self.feed(&events);
        events
    }

    async fn switch(&mut self, endpoint: &str) -> CacheEffect {
        let events = self.ask_on(Some(endpoint), None, None).await;
        self.feed(&events);
        cache_of(&events).unwrap()
    }

    async fn probe(&mut self, model: Option<&str>, effort: Option<Effort>) -> Option<CacheEffect> {
        self.probe_on(None, model, effort).await
    }

    async fn probe_on(
        &mut self,
        endpoint: Option<&str>,
        model: Option<&str>,
        effort: Option<Effort>,
    ) -> Option<CacheEffect> {
        let before = self.tracker.next().clone();
        let events = self.ask_on(endpoint, model, effort).await;
        let back = self
            .ask_on(Some(&before.endpoint), Some(&before.model), before.effort)
            .await;
        assert!(cache_of(&back).is_some(), "{back:?}");
        cache_of(&events)
    }

    fn expected(&self, effect: Effect, target: &str, row: Row) -> CacheEffect {
        let switch = target != self.tracker.session().model;
        match effect {
            Effect::WithEndpointSwitch => CacheEffect::Rebuilt {
                reason: RebuildReason::EndpointSwitch,
            },
            Effect::Kept => CacheEffect::Kept,
            Effect::KeptIfPassedOn => CacheEffect::KeptIfPassedOn,
            Effect::WithSwitch => CacheEffect::Rebuilt {
                reason: RebuildReason::ModelSwitch,
            },
            Effect::Rebuild(_) | Effect::Switch(..) => CacheEffect::Rebuilt {
                reason: if switch {
                    RebuildReason::ModelSwitch
                } else {
                    RebuildReason::EffortChange
                },
            },
            Effect::Current | Effect::Unsupported | Effect::Unavailable => {
                panic!("{row:?} {target}: no request to compare")
            }
        }
    }

    async fn endpoint_rows_agree(&mut self, label: &str) {
        let wanted = self.tracker.next().clone();
        let names: Vec<String> = self
            .facts
            .endpoints
            .iter()
            .map(|e| e.name.clone())
            .collect();
        for name in names {
            let predicted = self
                .tracker
                .endpoint_effect(&self.facts, &name, Duration::ZERO);
            let kernel = match predicted {
                Effect::Current => continue,
                Effect::Unavailable => {
                    let kernel = self.probe_on(Some(&name), None, None).await;
                    assert_eq!(kernel, None, "{label}: {name} should be refused");
                    continue;
                }
                _ => self.probe_on(Some(&name), None, None).await,
            };
            let session = self.tracker.session().clone();
            let want = match predicted {
                Effect::Kept => CacheEffect::Kept,
                Effect::KeptIfPassedOn => CacheEffect::KeptIfPassedOn,
                Effect::Rebuild(_) | Effect::Switch(..) if name != session.endpoint => {
                    CacheEffect::Rebuilt {
                        reason: RebuildReason::EndpointSwitch,
                    }
                }
                Effect::Rebuild(_) if wanted.model != session.model => CacheEffect::Rebuilt {
                    reason: RebuildReason::ModelSwitch,
                },
                Effect::Rebuild(_) => CacheEffect::Rebuilt {
                    reason: RebuildReason::EffortChange,
                },
                other => panic!("{label}: the {name} row predicts {other:?}"),
            };
            assert_eq!(kernel, Some(want), "{label}: the {name} endpoint row");
            self.checked += 1;
        }
    }

    async fn agrees(&mut self, label: &str) {
        self.endpoint_rows_agree(label).await;
        let wanted = self.tracker.next().clone();
        let ids: Vec<String> = self.facts.models.iter().map(|m| m.id.clone()).collect();
        for id in ids {
            let predicted = self.tracker.model_effect(&self.facts, &id, Duration::ZERO);
            match predicted {
                Effect::Current => continue,
                Effect::Unsupported => {
                    let kernel = self.probe(Some(&id), None).await;
                    assert_eq!(kernel, None, "{label}: {id} should be refused");
                    continue;
                }
                _ => {}
            }
            let (model, effort) = match switch_to(&id, &self.facts, wanted.effort, None) {
                Picked::Settings { model, effort, .. } => (model, effort),
                Picked::Effort {
                    model, preselect, ..
                } => (Some(model), preselect),
            };
            let kernel = self.probe(model.as_deref(), effort).await;
            let want = self.expected(predicted, &id, Row::Model);
            assert_eq!(kernel, Some(want), "{label}: the {id} row");
            self.checked += 1;
        }
        let caps = self
            .facts
            .model_facts(&wanted.model)
            .and_then(|m| m.caps.clone());
        let levels = caps
            .as_ref()
            .map_or(Effort::ALL.to_vec(), |c| c.efforts.clone());
        for level in &levels {
            if Some(*level) == wanted.effort {
                continue;
            }
            let predicted =
                self.tracker
                    .effort_effect(&self.facts, &wanted.model, *level, Duration::ZERO);
            let kernel = self.probe(None, Some(*level)).await;
            let want = self.expected(predicted, &wanted.model, Row::Effort);
            assert_eq!(
                kernel,
                Some(want),
                "{label}: the {} effort row",
                level.as_str()
            );
            self.checked += 1;
        }
        let at = wanted
            .effort
            .and_then(|e| levels.iter().position(|l| *l == e))
            .unwrap_or(0);
        let next = if levels.is_empty() {
            Effort::Medium
        } else {
            levels[(at + 1) % levels.len()]
        };
        match self.tracker.cycle(&self.facts, Duration::ZERO, UNI) {
            Ok((level, hint)) => {
                assert_eq!(level, next, "{label}");
                let predicted =
                    self.tracker
                        .effort_effect(&self.facts, &wanted.model, level, Duration::ZERO);
                assert!(
                    matches!(
                        predicted,
                        Effect::Kept | Effect::WithSwitch | Effect::WithEndpointSwitch
                    ),
                    "{label}: {hint}"
                );
                assert!(hint.ends_with(&predicted.text(UNI)), "{label}: {hint}");
                let kernel = self.probe(None, Some(level)).await;
                let want = self.expected(predicted, &wanted.model, Row::Effort);
                assert_eq!(kernel, Some(want), "{label}: shift-tab");
            }
            Err(why) => {
                let kernel = self.probe(None, Some(next)).await;
                assert_ne!(
                    kernel,
                    Some(CacheEffect::Kept),
                    "{label}: shift-tab refused ({why}) though the cache is kept"
                );
            }
        }
        self.checked += 1;
    }
}

fn cache_of(events: &[AgentEvent]) -> Option<CacheEffect> {
    events.iter().find_map(|e| match e {
        AgentEvent::SettingsChanged { cache, .. } => Some(cache.clone()),
        _ => None,
    })
}

#[tokio::test]
async fn documented_rows_and_shift_tab_match_the_kernel() {
    let mut h = Harness::new(Endpoint::Official, OPUS, Effort::Medium, |r, n| {
        answer(&model_of(r), n)
    });
    h.agrees("documented, before any turn").await;
    h.turn(vec![]).await;
    h.agrees("documented, after a turn").await;
    h.change(None, Some(Effort::High)).await;
    h.turn(vec![]).await;
    h.agrees("documented, after an effort change").await;
    assert!(h.checked > 20, "{}", h.checked);
}

#[tokio::test]
async fn switching_back_to_the_running_model_matches_the_kernel() {
    let mut h = Harness::new(Endpoint::Official, OPUS_4_8, Effort::Medium, |r, n| {
        answer(&model_of(r), n)
    });
    h.turn(vec![]).await;
    assert_eq!(
        h.change(None, Some(Effort::High)).await,
        CacheEffect::Rebuilt {
            reason: RebuildReason::EffortChange
        }
    );
    h.change(Some(SONNET), None).await;
    h.agrees("official, back to opus-4-8").await;
    let mut h = Harness::new(Endpoint::Custom, OPUS, Effort::Medium, |r, n| {
        answer(&model_of(r), n)
    });
    h.turn(vec![]).await;
    h.change(None, Some(Effort::High)).await;
    h.change(Some(SONNET), None).await;
    h.agrees("gateway, back to opus-5-5").await;
    let mut h = Harness::new(
        Endpoint::Custom,
        "claude-sonnet-4-6",
        Effort::High,
        |r, n| answer(&model_of(r), n),
    );
    h.turn(vec![]).await;
    h.change(Some(OPUS), Some(Effort::Xhigh)).await;
    h.agrees("gateway, back to sonnet-4-6 through the effort picker")
        .await;
}

#[tokio::test]
async fn a_proven_gateway_matches_the_kernel() {
    let mut h = Harness::new(Endpoint::Custom, OPUS, Effort::Medium, |r, n| {
        answer(&model_of(r), n)
    });
    h.agrees("unknown").await;
    h.turn(vec![]).await;
    h.agrees("unknown after a turn without a marker").await;
    assert_eq!(
        h.change(None, Some(Effort::Low)).await,
        CacheEffect::KeptIfPassedOn
    );
    let events = h.turn(vec![]).await;
    assert!(
        events
            .iter()
            .any(|e| matches!(e, AgentEvent::FeatureStateChanged { .. })),
        "{events:?}"
    );
    h.agrees("proven").await;
    assert!(h.tracker.cycle(&h.facts, Duration::ZERO, UNI).is_ok());
}

#[tokio::test]
async fn a_silent_first_request_rejection_matches_the_kernel() {
    let mut h = Harness::new(Endpoint::Custom, OPUS, Effort::Medium, |r, n| {
        if r.to_ascii_lowercase().contains(PER_MESSAGE_EFFORT_BETA) {
            return rejection(&format!(
                "Unexpected value(s) `{PER_MESSAGE_EFFORT_BETA}` for the `anthropic-beta` header."
            ));
        }
        answer(&model_of(r), n)
    });
    let first = h.turn(vec![]).await;
    assert!(cache_of(&first).is_none(), "{first:?}");
    assert_eq!(
        h.change(None, Some(Effort::Low)).await,
        CacheEffect::Rebuilt {
            reason: RebuildReason::EffortChange
        }
    );
    h.turn(vec![]).await;
    h.agrees("silently fallen back").await;
    assert!(h.tracker.cycle(&h.facts, Duration::ZERO, UNI).is_err());
}

#[tokio::test]
async fn a_fallback_under_a_pending_switch_matches_the_kernel() {
    let mut h = Harness::new(Endpoint::Custom, SONNET, Effort::Medium, |r, n| {
        if n == 0 {
            return format!(
                "delay:{}",
                rejection("messages.3.output_config: Extra inputs are not permitted")
            );
        }
        answer(&model_of(r), n)
    });
    h.change(None, Some(Effort::Low)).await;
    let switch = vec![ClientCommand::shortcut(
        RequestId(1),
        None,
        Some(OPUS.to_owned()),
        None,
    )];
    let events = h.turn(switch).await;
    assert!(
        !events.iter().any(|e| matches!(
            e,
            AgentEvent::SettingsChanged {
                cache: CacheEffect::Rebuilt {
                    reason: RebuildReason::PerMessageEffortRejected
                },
                ..
            }
        )),
        "{events:?}"
    );
    h.agrees("fallen back with a switch pending").await;
    h.change(Some(SONNET), None).await;
    h.agrees("back on the fallen-back model").await;
}

#[tokio::test]
async fn endpoint_rows_and_switches_match_the_kernel() {
    let sides = [("direct", Endpoint::Official), ("gw", Endpoint::Custom)];
    let mut h = Harness::endpoints(&sides, "direct", OPUS, Effort::Medium, |r, n| {
        answer(&model_of(r), n)
    });
    h.agrees("direct, before any turn").await;
    h.turn(vec![]).await;
    h.agrees("direct, after a turn").await;
    assert_eq!(
        h.switch("gw").await,
        CacheEffect::Rebuilt {
            reason: RebuildReason::EndpointSwitch
        }
    );
    h.agrees("a switch to the gateway pending").await;
    h.change(None, Some(Effort::Low)).await;
    h.agrees("a switch and an effort change pending").await;
    let events = h.turn(vec![]).await;
    assert_eq!(h.tracker.session().endpoint, "gw");
    assert!(
        events.iter().any(|e| matches!(
            e,
            AgentEvent::FeatureStateChanged { endpoint, .. } if endpoint == "gw"
        )),
        "{events:?}"
    );
    h.agrees("proven on the gateway").await;
    assert_eq!(h.change(None, Some(Effort::High)).await, CacheEffect::Kept);
    h.turn(vec![]).await;
    h.switch("direct").await;
    h.agrees("back to direct, pending").await;
    h.turn(vec![]).await;
    h.agrees("back on direct").await;
    h.switch("gw").await;
    h.change(Some(SONNET), None).await;
    h.agrees("a switch and a model change pending").await;
    h.switch("direct").await;
    h.agrees("switched back to the running endpoint with a model pending")
        .await;
    assert!(h.checked > 40, "{}", h.checked);
}

#[tokio::test]
async fn a_proven_gateway_stays_proven_across_a_round_trip() {
    let sides = [("a", Endpoint::Custom), ("b", Endpoint::Custom)];
    let mut h = Harness::endpoints(&sides, "a", OPUS, Effort::Medium, |r, n| {
        answer(&model_of(r), n)
    });
    h.turn(vec![]).await;
    h.change(None, Some(Effort::Low)).await;
    h.turn(vec![]).await;
    h.agrees("proven on a").await;
    h.switch("b").await;
    h.agrees("unknown on b, pending").await;
    assert!(h.tracker.cycle(&h.facts, Duration::ZERO, UNI).is_err());
    h.turn(vec![]).await;
    h.agrees("on b after its first turn").await;
    h.switch("a").await;
    h.turn(vec![]).await;
    h.agrees("proven again on a").await;
    assert!(h.tracker.cycle(&h.facts, Duration::ZERO, UNI).is_ok());
}

fn photo(width: u32, height: u32) -> Vec<u8> {
    use image::codecs::png::PngEncoder;
    use image::{ExtendedColorType, ImageEncoder, Rgb, RgbImage};
    let pixels = RgbImage::from_fn(width, height, |x, y| {
        Rgb([(x % 256) as u8, (y % 256) as u8, ((x + y) % 256) as u8])
    });
    let mut out = vec![];
    PngEncoder::new(&mut out)
        .write_image(pixels.as_raw(), width, height, ExtendedColorType::Rgb8)
        .unwrap();
    out
}

fn sent_image(request: &str) -> (u32, u32) {
    let body = request.split("\r\n\r\n").nth(1).unwrap();
    let json: serde_json::Value = serde_json::from_str(body).unwrap();
    let content = json["messages"].as_array().unwrap().last().unwrap()["content"]
        .as_array()
        .unwrap()
        .clone();
    let image = content.iter().find(|b| b["type"] == "image").unwrap();
    let data = image["source"]["data"].as_str().unwrap();
    let bytes = wisp_media::decode_base64(data).unwrap();
    let decoded = image::load_from_memory(&bytes).unwrap();
    (decoded.width(), decoded.height())
}

async fn kernel_sends(
    id: &str,
    row: &wisp_core::provider::ModelCapabilities,
    data: &Arc<str>,
) -> (u32, u32) {
    use std::sync::Mutex;

    use wisp_protocol::ImageData;

    let seen: Arc<Mutex<Vec<String>>> = Arc::default();
    let kept = Arc::clone(&seen);
    let model = id.to_owned();
    let effort = row.default_effort.unwrap_or(Effort::High);
    let (kernel, _) = provider(
        Endpoint::Official,
        Arc::new(move |request: &str, n: usize| {
            kept.lock().unwrap().push(request.to_owned());
            answer(&model, n)
        }),
    );
    assert_eq!(kernel.capabilities(id), Some(*row), "{id}");
    let mut h = Harness::build(kernel, Endpoint::Official, id, effort, |a| a);
    let (_commands, mut received) = mpsc::channel(8);
    let (events, mut emitted) = mpsc::channel(1024);
    let drain = tokio::spawn(async move { while emitted.recv().await.is_some() {} });
    h.agent
        .run_user_turn(
            String::new(),
            vec![ImageData {
                data: Arc::clone(data),
            }],
            &mut received,
            &events,
        )
        .await
        .unwrap();
    drop(events);
    drain.await.unwrap();
    let requests = seen.lock().unwrap().clone();
    assert_eq!(requests.len(), 1, "{id}");
    sent_image(&requests[0])
}

#[tokio::test]
async fn the_tuis_image_estimate_equals_what_the_kernel_sends_for_every_capability_row() {
    use wisp_core::endpoints::ProviderKind;
    use wisp_core::provider::shipped_capabilities;
    use wisp_render::chrome::ImageTier;
    use wisp_tui::term::clipboard::reader::prepare;

    let probe = photo(1600, 200);
    let sources = [
        ((2200, 900), photo(2200, 900)),
        ((1920, 1080), photo(1920, 1080)),
        ((2000, 2000), photo(2000, 2000)),
    ];
    let edge_bound = |tier: ImageTier| match tier.max_edge {
        1568 => (1568, 196),
        _ => (1600, 200),
    };
    let token_bound =
        |tier: ImageTier, size: (u32, u32)| match ((tier.max_edge, tier.max_tokens), size) {
            ((1568, 1568), (1920, 1080)) => Some((1456, 819)),
            ((2576, 4784), (2000, 2000)) => Some((1932, 1932)),
            _ => None,
        };
    let listed = super::models(&[]);
    let mut tiers = std::collections::BTreeSet::new();
    let mut capped = std::collections::BTreeSet::new();
    let rows = wisp_core::pricing::model_ids().filter_map(|id| match shipped_capabilities(id) {
        Some((ProviderKind::Anthropic, row)) => Some((id, row)),
        _ => None,
    });
    for (id, row) in rows {
        let tier = super::image_tier_of(&row);
        let facts = listed
            .iter()
            .find(|m| m.id == *id)
            .unwrap_or_else(|| panic!("{id} has a capability row but isn't listed"));
        assert_eq!(facts.image_tier, tier, "{id}");
        let Some(tier) = tier else {
            panic!("{id} has no image tier");
        };
        let tui = prepare(&probe, tier).unwrap();
        assert_eq!((tui.width, tui.height), edge_bound(tier), "{id} probe");
        let sent = kernel_sends(id, &row, &tui.data).await;
        assert_eq!((tui.width, tui.height), sent, "{id} probe");
        assert_eq!(tui.tokens, wisp_media::tokens(sent.0, sent.1), "{id} probe");
        if !tiers.insert((tier.max_edge, tier.max_tokens)) {
            continue;
        }
        for (size, source) in &sources {
            let tui = prepare(source, tier).unwrap();
            if let Some(bound) = token_bound(tier, *size) {
                assert_eq!((tui.width, tui.height), bound, "{id} {size:?}");
                capped.insert((tier.max_edge, tier.max_tokens));
            }
            let (width, height) = kernel_sends(id, &row, &tui.data).await;
            assert_eq!((tui.width, tui.height), (width, height), "{id} {size:?}");
            assert_eq!(
                tui.tokens,
                wisp_media::tokens(width, height),
                "{id} {size:?}"
            );
        }
    }
    assert_eq!(capped, tiers, "the token cap must bind for every tier");
    assert!(tiers.len() >= 2, "{tiers:?}");
}

#[test]
fn the_documented_wrapper_forwards_the_prefix_digest() {
    let inner =
        AnthropicProvider::new(&Key::ApiKey("sk-test".into()), "http://127.0.0.1:9").unwrap();
    let session = default_session();
    let messages = [wisp_protocol::Message::user_text("hi")];
    let request = Request {
        session: &session,
        tools: &[],
        deferred: &[],
        by_value: &[],
        server_tools: &[],
        messages: &messages,
        previous_response_id: None,
        features: wisp_core::provider::Features::default(),
        options: Default::default(),
        rendering: &wisp_core::provider::UNRENDERED,
        compaction: None,
    };
    let expected = wisp_core::provider::anthropic::sent_prefix(&request);
    assert!(expected.is_some());
    assert_eq!(Documented(inner).cache_prefix(&request), expected);
}

const GW_OFF: &str = "[endpoints.gw]\nnative_web_search = false\n";

#[tokio::test]
async fn a_gateway_sends_search_by_default_and_its_refusal_reads_as_this_endpoints() {
    let sides = [("direct", Endpoint::Official), ("gw", Endpoint::Custom)];
    let mut h = Harness::endpoints(&sides, "gw", OPUS, Effort::Medium, |r, n| {
        if r.contains("web_search_20250305") {
            rejection("web search is not enabled for this organization")
        } else {
            answer(&model_of(r), n)
        }
    })
    .searching(&sides, None);
    h.search_agrees("before any turn");
    assert_eq!(
        h.tracker.search_form(&h.facts, "gw", OPUS),
        Some(SearchForm::Native)
    );
    h.turn(vec![]).await;
    h.search_agrees("after the gateway's refusal");
    assert_eq!(
        h.tracker.search_form(&h.facts, "gw", OPUS),
        Some(SearchForm::None(NoSearch::Refused))
    );
    assert_eq!(
        h.tracker.search_form(&h.facts, "direct", OPUS),
        Some(SearchForm::Native)
    );
    assert_eq!(
        h.facts.search_line(&Glyphs::of(UNI)).as_deref(),
        Some("web search: off (not on this account) \u{b7} chat only")
    );
}

#[tokio::test]
async fn the_search_forms_the_tui_shows_match_the_kernel_across_a_refusal() {
    let sides = [("direct", Endpoint::Official), ("gw", Endpoint::Custom)];
    let mut h = Harness::endpoints(&sides, "direct", OPUS, Effort::Medium, |r, n| {
        if r.contains("web_search_20250305") {
            rejection("web search is not enabled for this organization")
        } else {
            answer(&model_of(r), n)
        }
    })
    .searching(&sides, Some(GW_OFF));
    h.search_agrees("before any turn");
    assert_eq!(
        h.tracker.search_form(&h.facts, "direct", OPUS),
        Some(SearchForm::Native)
    );
    assert_eq!(
        h.tracker.search_form(&h.facts, "gw", OPUS),
        Some(SearchForm::None(NoSearch::TurnedOff))
    );
    assert_eq!(h.facts.search_line(&Glyphs::of(UNI)), None);
    let events = h.turn(vec![]).await;
    assert!(
        events.iter().any(|e| matches!(
            e,
            AgentEvent::FeatureStateChanged {
                feature: Feature::NativeWebSearch,
                ..
            }
        )),
        "{events:?}"
    );
    h.search_agrees("after the refusal");
    assert_eq!(
        h.tracker.search_form(&h.facts, "direct", SONNET),
        Some(SearchForm::None(NoSearch::Refused))
    );
    assert_eq!(
        h.facts.search_line(&Glyphs::of(UNI)).as_deref(),
        Some("web search: off (not on this account) \u{b7} chat only")
    );
    h.switch("gw").await;
    h.search_agrees("a switch to the gateway pending");
    h.turn(vec![]).await;
    h.search_agrees("on the gateway");
    assert_eq!(
        h.facts.search_line(&Glyphs::of(UNI)).as_deref(),
        Some("web search: off (off for this account) \u{b7} chat only")
    );
    assert!(h.checked > 40, "{}", h.checked);
}

#[tokio::test]
async fn a_reply_stopped_while_thinking_leaves_no_reasoning_for_a_switch_to_drop() {
    let sides = [("direct", Endpoint::Official), ("gw", Endpoint::Custom)];
    let mut h = Harness::endpoints(&sides, "direct", OPUS, Effort::High, |r, n| {
        thinking(&model_of(r), n, n > 0)
    })
    .searching(&sides, Some(GW_OFF));
    let drops = "switching web search on or off drops earlier reasoning";
    let events = h.whole_turn(true).await;
    let stepped = events
        .iter()
        .position(|e| matches!(e, AgentEvent::StepEnded { .. }));
    let ended = events.iter().position(|e| {
        matches!(
            e,
            AgentEvent::TurnEnded {
                outcome: TurnOutcome::Cancelled {
                    committed_steps: 0,
                    ..
                },
                ..
            }
        )
    });
    assert!(stepped.zip(ended).is_some_and(|(s, e)| s < e), "{events:?}");
    let menu = Picker::endpoints()
        .queried("gw")
        .menu(&h.facts, &h.tracker, Duration::ZERO, UNI);
    let under = menu.shape.under.clone().unwrap_or_default();
    assert!(
        under
            .parts
            .iter()
            .any(|(part, _)| part.ends_with("web search: on \u{2192} off")),
        "{under:?}"
    );
    assert!(under.after.iter().all(|n| n != drops), "{menu:?}");
    let events = h.whole_turn(false).await;
    assert!(
        events.iter().any(|e| matches!(
            e,
            AgentEvent::TurnEnded {
                outcome: TurnOutcome::Completed {
                    committed: true,
                    ..
                },
                ..
            }
        )),
        "{events:?}"
    );
    let menu = Picker::endpoints()
        .queried("gw")
        .menu(&h.facts, &h.tracker, Duration::ZERO, UNI);
    let under = menu.shape.under.clone().unwrap_or_default();
    assert!(under.after.iter().any(|n| n == drops), "{menu:?}");
}

mod compact;
mod resume;
mod retry;
mod switch;

#[test]
fn the_tuis_provider_titles_are_the_kernels() {
    use wisp_core::endpoints::ProviderKind;
    for word in wisp_protocol::settings::PROVIDERS {
        let title = ProviderKind::parse(word).map(ProviderKind::title);
        assert_eq!(
            wisp_tui::app::settings::provider_title(word),
            title,
            "{word}"
        );
    }
}

#[test]
fn the_tuis_account_words_are_the_kernels() {
    use wisp_tui::app::config_screen::SWITCH_FIRST;
    assert_eq!(SWITCH_FIRST, wisp_core::agent::SWITCH_FIRST);
}

#[test]
fn the_tui_job_limits_are_the_kernels() {
    use wisp_tui::app::processes;
    assert_eq!(processes::MAX_PASTE, wisp_core::agent::MAX_ATTACH_INPUT);
    assert_eq!(processes::LINE_LIMIT, wisp_core::jobs::write::LINE_LIMIT);
    assert_eq!(processes::MIN_COLS, wisp_tool_text::terminal::MIN_COLS);
    assert_eq!(processes::MIN_ROWS, wisp_tool_text::terminal::MIN_ROWS);
}

#[tokio::test]
async fn gemini_call_signatures_replay_without_transcript_rows() {
    use std::sync::Mutex;
    use wisp_core::endpoints::{
        EndpointName, PricingMode, ProviderKind, ResolvedEndpoint, UrlSource,
    };
    use wisp_core::provider::credential::{Credential, Source};
    use wisp_core::provider::gemini::GeminiProvider;
    use wisp_render::markdown::{Layout, Renderer};
    use wisp_render::transcript::{Config, Detail, Transcript};

    let requests = Arc::new(Mutex::new(Vec::new()));
    let captured = requests.clone();
    let base_url = gateway(Arc::new(move |request, n| {
        let body =
            serde_json::from_str::<serde_json::Value>(request.split("\r\n\r\n").nth(1).unwrap())
                .unwrap();
        captured.lock().unwrap().push(body);
        let part = if n == 0 {
            json!({
                "functionCall": {"id": "call_canary", "name": "missing_tool", "args": {}},
                "thoughtSignature": "CANARY_function_signature"
            })
        } else {
            json!({"text": "Visible answer"})
        };
        let reply = json!({
            "candidates": [{
                "content": {"role": "model", "parts": [part]},
                "finishReason": "STOP"
            }],
            "usageMetadata": {"promptTokenCount": 10, "candidatesTokenCount": 2}
        });
        format!("{SSE_HEAD}data: {reply}\n\n")
    }));
    let endpoint = ResolvedEndpoint {
        client_version: None,
        name: EndpointName::parse("gemini-test").unwrap(),
        provider: ProviderKind::Gemini,
        credential: Credential::ApiKey("CANARY_GEMINI_KEY".into()),
        base_url,
        source: Source::Environment,
        key_env: None,
        url_source: UrlSource::Official,
        pricing: PricingMode::List,
    };
    let provider = Arc::new(GeminiProvider::from_endpoint(&endpoint).unwrap());
    let mut h = Harness::build(
        provider,
        Endpoint::Custom,
        "gemini-4-carbon",
        Effort::High,
        |a| a,
    );
    let events = h.turn(Vec::new()).await;
    let history = h.agent.history();
    let marker = history
        .iter()
        .flat_map(|message| &message.content)
        .find_map(|block| match block {
            ContentBlock::Opaque {
                provider,
                raw,
                deltas,
            } if raw["type"] == "call_signatures" => Some((provider, raw, deltas)),
            _ => None,
        })
        .unwrap();
    assert_eq!(marker.0, "gemini");
    assert_eq!(
        marker.1["signatures"],
        json!({"call_canary": "CANARY_function_signature"})
    );
    assert!(
        marker.1["identity"]
            .as_str()
            .is_some_and(|id| !id.is_empty())
    );
    assert!(marker.2.is_empty());
    let requests = requests.lock().unwrap();
    assert_eq!(requests.len(), 2);
    let replay = requests[1]["contents"]
        .as_array()
        .unwrap()
        .iter()
        .flat_map(|content| content["parts"].as_array().unwrap())
        .find(|part| part.get("functionCall").is_some())
        .unwrap();
    assert_eq!(replay["thoughtSignature"], "CANARY_function_signature");
    assert!(!events.iter().any(|event| matches!(
        event,
        AgentEvent::BlockStarted { kind: BlockKind::Other { name }, .. }
            if name == "call_signatures"
    )));
    let mut transcript = Transcript::new(Config {
        provider: "Gemini".into(),
        cache_ttl: CacheTtl::OneHour,
        logging: true,
    });
    for event in &events {
        transcript.apply(event, Duration::ZERO);
    }
    let renderer = Renderer::new(Layout::new(80, UNI));
    for detail in [Detail::Scrollback, Detail::Full] {
        let shown: String = transcript
            .rows(&renderer, detail)
            .iter()
            .flat_map(|row| row.spans.iter().map(|span| span.text.as_str()))
            .collect();
        assert!(!shown.contains("call_signatures"), "{shown}");
        assert!(shown.contains("Visible answer"), "{shown}");
        assert!(shown.contains("missing_tool"), "{shown}");
    }
}

#[test]
fn fallback_display_and_renderer_brace_corroboration_stay_in_agreement() {
    use wisp_protocol::ToolOutcome;
    use wisp_render::markdown::{Layout, Renderer};
    use wisp_render::transcript::{Config, Detail, Transcript};

    let render_fallback = |name: &str, input: serde_json::Value| {
        let display = wisp_core::tool::fallback_display(name, &input);
        let mut transcript = Transcript::new(Config {
            provider: "Anthropic".into(),
            cache_ttl: CacheTtl::OneHour,
            logging: false,
        });
        transcript.apply(&AgentEvent::turn_started(1), Duration::ZERO);
        transcript.apply(
            &AgentEvent::ToolCallRequested {
                step: 1,
                block: 0,
                call_id: "call_1".to_owned(),
                tool: name.to_owned(),
                input,
                display: display.clone(),
                refused: true,
            },
            Duration::from_millis(1),
        );
        transcript.apply(
            &AgentEvent::call_finished(
                "call_1",
                ToolOutcome::InvalidInput {
                    message: "bad input".to_owned(),
                },
            ),
            Duration::from_millis(2),
        );
        let renderer = Renderer::new(Layout::new(120, UNI));
        let rendered: Vec<String> = [Detail::Scrollback, Detail::Full]
            .into_iter()
            .map(|detail| {
                transcript
                    .rows(&renderer, detail)
                    .into_iter()
                    .map(|row| {
                        row.spans
                            .into_iter()
                            .map(|span| span.text)
                            .collect::<String>()
                    })
                    .collect::<Vec<_>>()
                    .join("\n")
            })
            .collect();
        (display, rendered)
    };

    let (display, rendered) = render_fallback("ordinary", json!({"b": 1, "a": 2}));
    assert_eq!(display.target, "{a, b}");
    for rows in &rendered {
        assert!(rows.contains("Used ordinary {a, b}"), "{rows}");
    }

    let (display, rendered) = render_fallback("lf_case", json!({"line1\nline2": 1}));
    assert_eq!(display.target, "{line1 line2}");
    for rows in &rendered {
        assert!(rows.contains("Used lf_case {line1 line2}"), "{rows}");
    }

    let (display, rendered) = render_fallback("crlf_case", json!({"line1\r\nline2": 1}));
    assert_eq!(display.target, "{line1 line2}");
    for rows in &rendered {
        assert!(rows.contains("Used crlf_case {line1 line2}"), "{rows}");
    }

    let long_input =
        serde_json::Value::Object((0..40).map(|n| (format!("k{n:02}"), json!(1))).collect());
    let (display, rendered) = render_fallback("long_clip", long_input);
    assert!(display.target.ends_with('\u{2026}'), "{display:?}");
    for rows in &rendered {
        assert!(rows.contains("Used long_clip {}"), "{rows}");
        assert!(!rows.contains("k39"), "{rows}");
    }

    let mut span_unsafe = serde_json::Map::new();
    for n in 0..14 {
        span_unsafe.insert(format!("k{n:02}"), json!(1));
    }
    span_unsafe.insert("z.sk-ant-CANARY0123456789012345".to_owned(), json!(1));
    let (display, rendered) =
        render_fallback("span_unsafe", serde_json::Value::Object(span_unsafe));
    assert_eq!(display.target, "{}");
    for rows in &rendered {
        assert!(rows.contains("Used span_unsafe {}"), "{rows}");
        for forbidden in ["k00", "sk-ant-", "CANARY"] {
            assert!(!rows.contains(forbidden), "{forbidden}: {rows}");
        }
    }

    let (_, rendered) = render_fallback("tab_case", json!({"hello\tworld": 1}));
    for rows in &rendered {
        assert!(rows.contains("Used tab_case {}"), "{rows}");
        assert!(!rows.contains("hello world"), "{rows}");
    }

    let (_, rendered) = render_fallback(
        "zw_case",
        json!({"sk-ant-\u{200B}CANARY0123456789012345": 1}),
    );
    for rows in &rendered {
        assert!(rows.contains("Used zw_case {}"), "{rows}");
        for forbidden in ["sk-ant-", "CANARY"] {
            assert!(!rows.contains(forbidden), "{forbidden}: {rows}");
        }
    }

    let (_, rendered) = render_fallback("bom_case", json!({"user\u{FEFF}@example.invalid": 1}));
    for rows in &rendered {
        assert!(rows.contains("Used bom_case {}"), "{rows}");
        for forbidden in ["user@", "example.invalid"] {
            assert!(!rows.contains(forbidden), "{forbidden}: {rows}");
        }
    }

    let mut clip_zw = serde_json::Map::new();
    for n in 0..14 {
        clip_zw.insert(format!("k{n:02}"), json!(1));
    }
    let mut clip_tab = clip_zw.clone();
    clip_zw.insert(
        "z_prefix.sk-ant-\u{200B}CANARY0123456789012345".to_owned(),
        json!(1),
    );
    clip_tab.insert("z_tab_prefix_secret\ttail".to_owned(), json!(1));

    let (display, rendered) = render_fallback("clip_zw", serde_json::Value::Object(clip_zw));
    assert_eq!(display.target, "{}", "{display:?}");
    for rows in &rendered {
        assert!(rows.contains("Used clip_zw {}"), "{rows}");
        for forbidden in ["k00", "z_prefix", "sk-ant-", "CANARY"] {
            assert!(!rows.contains(forbidden), "{forbidden}: {rows}");
        }
    }

    let (display, rendered) = render_fallback("clip_tab", serde_json::Value::Object(clip_tab));
    assert!(display.target.ends_with('\u{2026}'), "{display:?}");
    for rows in &rendered {
        assert!(rows.contains("Used clip_tab {}"), "{rows}");
        for forbidden in ["k00", "z_tab_pr", "secret"] {
            assert!(!rows.contains(forbidden), "{forbidden}: {rows}");
        }
    }

    let (display, rendered) =
        render_fallback("user\u{200B}@example.invalid", serde_json::Value::Null);
    assert_eq!(
        display.action,
        wisp_protocol::ToolAction::Other {
            name: "tool".to_owned(),
        }
    );
    assert_eq!(display.target, "{}");
    for rows in &rendered {
        assert!(rows.contains("Used tool {}"), "{rows}");
        for forbidden in ["user@", "example.invalid"] {
            assert!(!rows.contains(forbidden), "{forbidden}: {rows}");
        }
    }

    let render_raw = |name: &str, input: serde_json::Value, display: wisp_protocol::ToolDisplay| {
        let mut transcript = Transcript::new(Config {
            provider: "Anthropic".into(),
            cache_ttl: CacheTtl::OneHour,
            logging: false,
        });
        transcript.apply(&AgentEvent::turn_started(1), Duration::ZERO);
        transcript.apply(
            &AgentEvent::ToolCallRequested {
                step: 1,
                block: 0,
                call_id: "call_raw".to_owned(),
                tool: name.to_owned(),
                input,
                display,
                refused: true,
            },
            Duration::from_millis(1),
        );
        transcript.apply(
            &AgentEvent::call_finished(
                "call_raw",
                ToolOutcome::InvalidInput {
                    message: "bad input".to_owned(),
                },
            ),
            Duration::from_millis(2),
        );
        let renderer = Renderer::new(Layout::new(120, UNI));
        [Detail::Scrollback, Detail::Full]
            .into_iter()
            .map(|detail| {
                transcript
                    .rows(&renderer, detail)
                    .into_iter()
                    .map(|row| {
                        row.spans
                            .into_iter()
                            .map(|span| span.text)
                            .collect::<String>()
                    })
                    .collect::<Vec<_>>()
                    .join("\n")
            })
            .collect::<Vec<_>>()
    };

    let raw_display = |name: &str, target: &str| wisp_protocol::ToolDisplay {
        action: wisp_protocol::ToolAction::Other {
            name: name.to_owned(),
        },
        verbs: None,
        target: target.to_owned(),
        output: wisp_protocol::OutputKind::Text,
        view: None,
        title: None,
        purpose: wisp_protocol::ToolPurpose::Run,
    };

    let mut old_intact = serde_json::Map::new();
    for n in 0..14 {
        old_intact.insert(format!("k{n:02}"), json!(1));
    }
    old_intact.insert("z.sk-ant-CANARY0123456789012345".to_owned(), json!(1));
    let old_prefix =
        "{k00, k01, k02, k03, k04, k05, k06, k07, k08, k09, k10, k11, k12, k13, z.sk-ant\u{2026}";
    for rows in render_raw(
        "old_clip_intact",
        serde_json::Value::Object(old_intact),
        raw_display("old_clip_intact", old_prefix),
    ) {
        assert!(rows.contains("Used old_clip_intact {}"), "{rows}");
        for forbidden in ["k00", "sk-ant-", "CANARY"] {
            assert!(!rows.contains(forbidden), "{forbidden}: {rows}");
        }
    }

    for rows in render_raw(
        "escaped_legacy",
        json!({"ab_safe": 1}),
        raw_display("escaped_legacy", "{\"a\\u0062_safe\":1}"),
    ) {
        assert!(rows.contains("Used escaped_legacy {}"), "{rows}");
        assert!(!rows.contains("ab_safe"), "{rows}");
    }

    for rows in render_raw(
        "malformed_legacy",
        json!({"a": 1, "b": "CANARY-val"}),
        raw_display("malformed_legacy", "{\"a\":1, bad\u{2026}"),
    ) {
        assert!(rows.contains("Used malformed_legacy {}"), "{rows}");
        assert!(!rows.contains("CANARY"), "{rows}");
    }

    for (name, prefix) in [
        ("bom_brace", "\u{FEFF}{secret: CANARY-val}"),
        ("zwsp_brace", "\u{200B}{secret: CANARY-val}"),
    ] {
        for rows in render_raw(
            name,
            json!({"secret": "CANARY-val"}),
            raw_display(name, prefix),
        ) {
            assert!(rows.contains(&format!("Used {name} {{}}")), "{rows}");
            assert!(!rows.contains("CANARY"), "{rows}");
        }
    }

    for rows in render_raw(
        "user\u{200B}@example.invalid",
        serde_json::Value::Null,
        raw_display("user\u{200B}@example.invalid", "{}"),
    ) {
        assert!(rows.contains("Used tool {}"), "{rows}");
        for forbidden in ["user@", "example.invalid"] {
            assert!(!rows.contains(forbidden), "{forbidden}: {rows}");
        }
    }
}
