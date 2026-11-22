use std::collections::BTreeSet;
use std::sync::Arc;

use jiff::SignedDuration;
use serde_json::{Value, json};
use tokio::sync::mpsc;
use wisp_protocol::RequestId;
use wisp_protocol::context::ReportDetail;
use wisp_protocol::log::Cut;
use wisp_protocol::{
    AgentEvent, CacheTtl, ContentBlock, Effort, ErrorKind, Message, RenderingForm, StopReason,
    TurnOutcome, UnfinishedReason, Usage,
};

use super::*;
use crate::agent::forms::Trial;
use crate::agent::testing::{Caps, Fake, Knobs, Prefix, Script, Step, ended, text};
use crate::agent::tests::completed;
use crate::agent::{DEFAULT_MODEL, default_session};
use crate::context::{CompactionCostInput, compaction_cost, usd};
use crate::pricing;
use crate::provider::anthropic::{build_marked_body, sent_prefix};
use crate::provider::{Feature, ModelCapabilities, ProviderError, ProviderEvent, Request};

mod log;

const OTHER_MODEL: &str = "claude-sonnet-4-6";

static PER_MESSAGE: ModelCapabilities = ModelCapabilities {
    efforts: &[Effort::Low, Effort::Medium, Effort::High],
    default_effort: None,
    per_message_effort: true,
    effort_in_id: None,
    adaptive_thinking: true,
    thinking_budgets: None,
    max_output: None,
    images: None,
    inline_tools: false,
    tools_after_results: false,
    deferred_tools: false,
};

fn scripted(scripts: Vec<Script>) -> Arc<Fake> {
    with_row(scripts, None, false)
}

fn with_row(
    scripts: Vec<Script>,
    row: Option<&'static ModelCapabilities>,
    documented: bool,
) -> Arc<Fake> {
    Fake::with(
        Knobs {
            prefix: Prefix::Sent,
            caps: Some(Caps::Fixed(row)),
            documented: Some(documented),
            count: Some(1),
            ..Knobs::default()
        },
        scripts.into_iter().map(Step::from),
    )
}

fn usage(input: u64, output: u64) -> Usage {
    Usage {
        input_tokens: input,
        output_tokens: output,
        ..Usage::default()
    }
}

fn reply(blocks: Vec<ContentBlock>, input: u64, output: u64, stop: StopReason) -> Script {
    let mut script = vec![Ok(crate::agent::tests::start(input, 0))];
    for (index, block) in blocks.into_iter().enumerate() {
        match block {
            ContentBlock::Text { text: words } => script.extend(text(index, &words)),
            other => script.extend([
                Ok(ProviderEvent::BlockStart {
                    index,
                    block: other,
                }),
                Ok(ProviderEvent::BlockStop { index }),
            ]),
        }
    }
    script.extend(ended(stop, usage(input, output), vec![]));
    script
}

fn answer(words: &str, input: u64) -> Script {
    reply(
        vec![ContentBlock::Text { text: words.into() }],
        input,
        40,
        StopReason::EndTurn,
    )
}

fn calls_tool(input: u64) -> Script {
    reply(
        vec![
            ContentBlock::Text {
                text: "Looking.".into(),
            },
            ContentBlock::ToolUse {
                id: "t1".into(),
                name: "missing".into(),
                input: json!({}),
            },
        ],
        input,
        30,
        StopReason::ToolUse,
    )
}

fn empty_reply(input: u64) -> Script {
    reply(
        vec![ContentBlock::Text {
            text: String::new(),
        }],
        input,
        5,
        StopReason::MaxTokens,
    )
}

fn refused(kind: ErrorKind, rejected: Vec<Feature>) -> Script {
    let mut error = ProviderError::new(kind, "refused");
    error.rejected = rejected;
    vec![Err(error)]
}

struct Rig {
    agent: Agent,
    commands: mpsc::Receiver<wisp_protocol::ClientCommand>,
    _keep: mpsc::Sender<wisp_protocol::ClientCommand>,
    events: mpsc::Sender<AgentEvent>,
}

impl Rig {
    fn new(provider: Arc<Fake>) -> Self {
        Self::of(Agent::new(provider, default_session()))
    }

    fn of(agent: Agent) -> Self {
        let (keep, commands) = mpsc::channel(8);
        let (events, mut sink) = mpsc::channel(1_024);
        tokio::spawn(async move { while sink.recv().await.is_some() {} });
        Self {
            agent,
            commands,
            _keep: keep,
            events,
        }
    }

    async fn turn(&mut self, text: &str) -> TurnOutcome {
        self.agent
            .run_turn(text.into(), &mut self.commands, &self.events)
            .await
            .unwrap()
    }

    async fn change(
        &mut self,
        endpoint: Option<&str>,
        model: Option<&str>,
        effort: Option<Effort>,
    ) {
        self.agent
            .change_settings(
                endpoint.map(str::to_owned),
                model.map(str::to_owned),
                effort,
                &self.events,
            )
            .await;
    }

    fn entries(&self) -> &[Entry] {
        &self.agent.epoch.entries
    }

    fn mark(&self) -> usize {
        self.agent.epoch.mark
    }

    fn marked_body(&self) -> Value {
        let (session, features) = self.agent.request_parts();
        let rendering = self.agent.rendering(self.agent.history.len());
        let request = Request {
            session: &session,
            tools: &self.agent.sent,
            deferred: &[],
            by_value: &[],
            server_tools: self.agent.server_tools(),
            messages: &self.agent.history,
            previous_response_id: None,
            features,
            options: Default::default(),
            rendering: &rendering,
            compaction: None,
        };
        let tail = self.agent.tail_marker();
        let body = build_marked_body(&request, tail).unwrap();
        serde_json::from_slice(&body).unwrap()
    }
}

fn entry(view_len: usize, ttl: CacheTtl, started_at: jiff::Timestamp) -> Entry {
    Entry {
        turn: 1,
        seq: None,
        view_len,
        prompt_tokens: 1_000,
        used: 1_000,
        output_tokens: 40,
        started_at,
        ttl,
        model: DEFAULT_MODEL.into(),
        endpoint: 0,
        prefix: Some(format!("sent {view_len}")),
        refreshed_at: None,
    }
}

fn at(minutes: i64) -> jiff::Timestamp {
    jiff::Timestamp::UNIX_EPOCH
        .checked_add(SignedDuration::from_mins(minutes))
        .unwrap()
}

#[derive(Default)]
struct BreakpointCache {
    written: BTreeSet<String>,
}

#[derive(Debug, Default, PartialEq, Eq)]
struct Traffic {
    read: Vec<usize>,
    written: Vec<usize>,
}

fn unmarked(block: &Value) -> Value {
    let mut block = block.clone();
    if let Some(map) = block.as_object_mut() {
        map.remove("cache_control");
    }
    block
}

fn cacheable(block: &Value) -> bool {
    match block["type"].as_str() {
        Some("thinking" | "redacted_thinking") | None => false,
        Some("text") => block["text"].as_str().is_some_and(|t| !t.trim().is_empty()),
        Some(_) => true,
    }
}

impl BreakpointCache {
    fn send(&mut self, body: &Value) -> Traffic {
        let mut prompt: Vec<(Value, bool)> = Vec::new();
        for key in ["tools", "system"] {
            for item in body[key].as_array().into_iter().flatten() {
                prompt.push((
                    json!([key, unmarked(item)]),
                    item.get("cache_control").is_some(),
                ));
            }
        }
        for message in body["messages"].as_array().unwrap() {
            let role = &message["role"];
            let config = &message["output_config"];
            if message["content"].as_array().unwrap().is_empty() {
                prompt.push((json!([role, config]), false));
            }
            for block in message["content"].as_array().unwrap() {
                let marked = block.get("cache_control").is_some();
                prompt.push((json!([role, unmarked(block)]), marked));
            }
        }
        if body.get("cache_control").is_some()
            && let Some(last) = prompt.iter().rposition(|(item, _)| cacheable(&item[1]))
        {
            prompt[last].1 = true;
        }
        let mut traffic = Traffic::default();
        for end in (0..prompt.len()).filter(|i| prompt[*i].1) {
            let key = serde_json::to_string(
                &prompt[..=end]
                    .iter()
                    .map(|(item, _)| item)
                    .collect::<Vec<_>>(),
            )
            .unwrap();
            if self.written.contains(&key) {
                traffic.read.push(end);
            } else {
                traffic.written.push(end);
                self.written.insert(key);
            }
        }
        traffic
    }
}

#[test]
fn only_the_last_entry_after_the_mark_on_the_same_pair_anchors() {
    let mut epoch = Epoch::default();
    assert_eq!(epoch.tail_marker(0, DEFAULT_MODEL, at(0)), None);
    epoch.push(entry(1, CacheTtl::OneHour, at(0)));
    epoch.push(entry(3, CacheTtl::FiveMinutes, at(1)));
    let marker = |view_len, ttl| {
        Some(TailMarker {
            view_len,
            ttl,
            prefix: format!("sent {view_len}"),
            last_blocks: None,
        })
    };
    assert_eq!(
        epoch.tail_marker(0, DEFAULT_MODEL, at(2)),
        marker(3, CacheTtl::FiveMinutes)
    );
    assert_eq!(epoch.tail_marker(1, DEFAULT_MODEL, at(2)), None);
    assert_eq!(epoch.tail_marker(0, OTHER_MODEL, at(2)), None);
    epoch.rebuilt(MarkReason::ModelSwitch);
    assert_eq!(epoch.entries.len(), 2);
    assert_eq!(epoch.anchor(0, DEFAULT_MODEL), None);
    assert_eq!(epoch.tail_marker(0, DEFAULT_MODEL, at(2)), None);
    epoch.push(entry(5, CacheTtl::OneHour, at(3)));
    assert_eq!(
        epoch.tail_marker(0, DEFAULT_MODEL, at(4)),
        marker(5, CacheTtl::OneHour)
    );
}

#[test]
fn no_tail_marker_once_the_anchor_is_past_its_own_ttl() {
    let mut epoch = Epoch::default();
    epoch.push(entry(1, CacheTtl::FiveMinutes, at(0)));
    assert!(epoch.tail_marker(0, DEFAULT_MODEL, at(4)).is_some());
    assert!(epoch.tail_marker(0, DEFAULT_MODEL, at(5)).is_none());
    epoch.push(entry(3, CacheTtl::OneHour, at(10)));
    assert!(epoch.tail_marker(0, DEFAULT_MODEL, at(69)).is_some());
    assert!(epoch.tail_marker(0, DEFAULT_MODEL, at(70)).is_none());
}

#[test]
fn a_change_inside_an_entrys_view_sets_the_mark_and_one_after_it_does_not() {
    let mut epoch = Epoch::default();
    epoch.push(entry(1, CacheTtl::OneHour, at(0)));
    epoch.push(entry(3, CacheTtl::OneHour, at(1)));
    epoch.changed_from(3, MarkReason::TailClosed);
    assert_eq!(epoch.mark, 0);
    epoch.changed_from(2, MarkReason::TailClosed);
    assert_eq!(epoch.mark, 2);
    epoch.push(entry(5, CacheTtl::OneHour, at(2)));
    epoch.changed_from(4, MarkReason::TailClosed);
    assert_eq!(epoch.mark, 3);
}

#[tokio::test]
async fn every_committed_reply_adds_an_entry_and_nothing_else_does() {
    let provider = scripted(vec![
        calls_tool(1_000),
        answer("done", 1_200),
        refused(ErrorKind::InvalidRequest, vec![]),
        empty_reply(1_300),
        answer("again", 1_400),
        reply(Vec::new(), 1_500, 5, StopReason::EndTurn),
    ]);
    let mut rig = Rig::new(provider);
    assert!(completed(&rig.turn("first").await));
    let views: Vec<usize> = rig.entries().iter().map(|e| e.view_len).collect();
    assert_eq!(views, [1, 3]);
    assert_eq!(rig.entries()[1].used, 1_200);
    assert_eq!(rig.entries()[1].output_tokens, 40);
    assert!(!completed(&rig.turn("fails").await));
    assert_eq!(rig.entries().len(), 2);
    rig.turn("empty").await;
    assert_eq!(rig.entries().len(), 2);
    assert!(completed(&rig.turn("works").await));
    let views: Vec<usize> = rig.entries().iter().map(|e| e.view_len).collect();
    assert_eq!(views, [1, 3, 5]);
    assert!(completed(&rig.turn("quiet").await));
    let views: Vec<usize> = rig.entries().iter().map(|e| e.view_len).collect();
    assert_eq!(views, [1, 3, 5, 7]);
    assert_eq!(rig.mark(), 0);
}

#[tokio::test]
async fn a_cancelled_step_adds_no_entry() {
    let provider = scripted(vec![answer("hi", 1_000), vec![Ok(ProviderEvent::Sending)]]);
    let mut rig = Rig::new(provider);
    rig.turn("first").await;
    let (send, mut commands) = mpsc::channel(8);
    send.try_send(wisp_protocol::ClientCommand::Cancel).unwrap();
    let outcome = rig
        .agent
        .run_turn("stop".into(), &mut commands, &rig.events)
        .await
        .unwrap();
    assert!(
        matches!(outcome, TurnOutcome::Cancelled { .. }),
        "{outcome:?}"
    );
    assert_eq!(rig.entries().len(), 1);
}

#[tokio::test]
async fn a_model_switch_sets_the_mark_and_keeps_the_entries() {
    let provider = scripted(vec![
        answer("hi", 1_000),
        refused(ErrorKind::InvalidRequest, vec![]),
        answer("there", 1_100),
    ]);
    let mut rig = Rig::new(provider);
    rig.turn("first").await;
    rig.change(None, Some(OTHER_MODEL), None).await;
    assert_eq!(rig.mark(), 0);
    rig.turn("fails on the new model").await;
    assert_eq!((rig.entries().len(), rig.mark()), (1, 1));
    assert_eq!(rig.agent.tail_marker(), None);
    rig.turn("works").await;
    assert_eq!((rig.entries().len(), rig.mark()), (2, 1));
    assert_eq!(rig.entries()[1].model, OTHER_MODEL);
    assert!(rig.agent.tail_marker().is_some());
}

#[tokio::test]
async fn an_endpoint_switch_sets_the_mark() {
    let provider = scripted(vec![answer("hi", 1_000), answer("there", 1_100)]);
    let agent = Agent::new(provider, default_session());
    let first = agent.connections[0].clone();
    let mut second = first.clone();
    second.name = "other".into();
    let mut rig = Rig::of(agent.with_connections(
        vec![first, second],
        vec![],
        crate::agent::IMPLICIT_ENDPOINT,
    ));
    rig.turn("first").await;
    rig.change(Some("other"), None, None).await;
    rig.turn("second").await;
    assert_eq!((rig.entries().len(), rig.mark()), (2, 1));
    assert_eq!(rig.entries()[1].endpoint, 1);
}

#[tokio::test]
async fn a_top_level_effort_change_sets_the_mark_and_a_per_message_one_does_not() {
    let provider = scripted(vec![answer("hi", 1_000), answer("there", 1_100)]);
    let mut rig = Rig::new(provider);
    rig.turn("first").await;
    rig.change(None, None, Some(Effort::Low)).await;
    rig.turn("second").await;
    assert_eq!((rig.entries().len(), rig.mark()), (2, 1));

    let provider = with_row(
        vec![answer("hi", 1_000), answer("there", 1_100)],
        Some(&PER_MESSAGE),
        true,
    );
    let mut rig = Rig::new(provider.clone());
    rig.turn("first").await;
    rig.change(None, None, Some(Effort::Low)).await;
    rig.turn("second").await;
    assert_eq!((rig.entries().len(), rig.mark()), (2, 0));
    let sent = provider.body(1);
    let last = sent["messages"].as_array().unwrap().last().unwrap().clone();
    assert_eq!(last["output_config"]["effort"], "low");
}

#[tokio::test]
async fn a_per_message_effort_fallback_sets_the_mark() {
    let provider = with_row(
        vec![
            answer("hi", 1_000),
            refused(ErrorKind::InvalidRequest, vec![Feature::PerMessageEffort]),
            answer("there", 1_100),
        ],
        Some(&PER_MESSAGE),
        false,
    );
    let mut rig = Rig::new(provider);
    rig.turn("first").await;
    rig.change(None, None, Some(Effort::Low)).await;
    assert!(completed(&rig.turn("second").await));
    assert_eq!((rig.entries().len(), rig.mark()), (2, 1));
}

#[tokio::test]
async fn a_cache_ttl_change_sets_no_mark_and_each_entry_keeps_its_ttl() {
    let provider = scripted(vec![answer("hi", 1_000), answer("there", 1_100)]);
    let mut rig = Rig::new(provider);
    rig.turn("first").await;
    rig.agent.session.cache_ttl = CacheTtl::FiveMinutes;
    let marker = rig.agent.tail_marker().unwrap();
    assert_eq!(marker.ttl, CacheTtl::OneHour);
    let body = rig.marked_body();
    let first = body["messages"][0]["content"].as_array().unwrap();
    assert_eq!(first.last().unwrap()["cache_control"]["ttl"], "1h");
    rig.turn("second").await;
    assert_eq!(rig.mark(), 0);
    let ttls: Vec<CacheTtl> = rig.entries().iter().map(|e| e.ttl).collect();
    assert_eq!(ttls, [CacheTtl::OneHour, CacheTtl::FiveMinutes]);
    assert_eq!(rig.agent.tail_marker().unwrap().ttl, CacheTtl::FiveMinutes);
}

#[tokio::test]
async fn a_kept_rendering_form_sets_the_mark() {
    let provider = scripted(vec![answer("hi", 1_000)]);
    let mut rig = Rig::new(provider);
    rig.turn("first").await;
    rig.agent.keep_form(&rig.events).await;
    assert_eq!(rig.mark(), 0);
    rig.agent.search.trial = Some(Trial {
        form: RenderingForm::WithoutThinking,
        cut: Some(Cut {
            from: wisp_protocol::log::BlockPosition {
                message: 1,
                block: 0,
            },
            through: 1,
            tools: false,
        }),
    });
    rig.agent.keep_form(&rig.events).await;
    assert_eq!(rig.mark(), 1);
}

fn server_call(id: &str) -> ContentBlock {
    ContentBlock::opaque(
        "anthropic",
        json!({"type": "server_tool_use", "id": id, "name": "web_search", "input": {"query": "q"}}),
    )
}

#[tokio::test]
async fn closing_a_tail_inside_the_anchors_view_sets_the_mark() {
    for (view_len, mark) in [(1, 0), (2, 1)] {
        let mut rig = Rig::new(scripted(vec![]));
        rig.agent.history = vec![
            Message::user_text("search"),
            Message {
                role: wisp_protocol::Role::Assistant,
                content: vec![server_call("s1")],
            },
        ];
        rig.agent.sync_history();
        rig.agent
            .epoch
            .push(entry(view_len, CacheTtl::OneHour, at(0)));
        assert_eq!(rig.agent.open_calls(), ["s1"]);
        rig.agent
            .close_tail(UnfinishedReason::Failed, &BTreeSet::new(), &rig.events)
            .await;
        assert_eq!(rig.mark(), mark, "view_len {view_len}");
    }
}

#[tokio::test]
async fn the_marked_request_reads_the_anchors_cached_prefix_and_writes_nothing() {
    let provider = scripted(vec![calls_tool(1_000), answer("done", 1_200)]);
    let mut rig = Rig::new(provider.clone());
    rig.turn("list").await;
    let mut cache = BreakpointCache::default();
    cache.send(&provider.body(0));
    let anchor = cache.send(&provider.body(1));
    let marked = rig.marked_body();
    assert!(marked.get("cache_control").is_none());
    let blocks: Vec<(usize, usize)> = marked["messages"]
        .as_array()
        .unwrap()
        .iter()
        .enumerate()
        .flat_map(|(m, message)| {
            message["content"]
                .as_array()
                .unwrap()
                .iter()
                .enumerate()
                .filter(|(_, b)| b.get("cache_control").is_some())
                .map(move |(b, _)| (m, b))
        })
        .collect();
    assert_eq!(blocks, [(2, 0)]);
    let traffic = cache.send(&marked);
    assert!(traffic.written.is_empty(), "{traffic:?}");
    assert_eq!(traffic.read.last(), anchor.written.last());
}

#[tokio::test]
async fn an_anchor_ending_in_an_effort_marker_reads_the_prefix_before_it() {
    let provider = with_row(
        vec![answer("hi", 1_000), answer("there", 1_100)],
        Some(&PER_MESSAGE),
        true,
    );
    let mut rig = Rig::new(provider.clone());
    rig.turn("first").await;
    rig.change(None, None, Some(Effort::Low)).await;
    rig.turn("second").await;
    let anchored = provider.body(1);
    let sent = anchored["messages"].as_array().unwrap();
    assert_eq!(sent.last().unwrap()["role"], "system");
    let mut cache = BreakpointCache::default();
    cache.send(&provider.body(0));
    let anchor = cache.send(&anchored);
    let marked = rig.marked_body();
    let user = &marked["messages"][2];
    assert_eq!(user["role"], "user");
    let last = user["content"].as_array().unwrap().last().unwrap();
    assert_eq!(last["cache_control"]["ttl"], "1h");
    let traffic = cache.send(&marked);
    assert!(traffic.written.is_empty(), "{traffic:?}");
    assert_eq!(traffic.read.last(), anchor.written.last());
}

#[tokio::test]
async fn a_marker_past_the_anchor_would_write_so_the_mock_tells_them_apart() {
    let provider = scripted(vec![calls_tool(1_000), answer("done", 1_200)]);
    let mut rig = Rig::new(provider.clone());
    rig.turn("list").await;
    let mut cache = BreakpointCache::default();
    cache.send(&provider.body(1));
    let mut wrong = rig.marked_body();
    let block = wrong["messages"][2]["content"][0].as_object_mut().unwrap();
    let marker = block.remove("cache_control").unwrap();
    wrong["messages"][3]["content"][0]["cache_control"] = marker;
    assert_eq!(cache.send(&wrong).written.len(), 1);
}

fn prefix(report: &wisp_protocol::context::ContextReport) -> u64 {
    report
        .categories
        .iter()
        .filter(|c| {
            matches!(
                c.category,
                wisp_protocol::context::Category::SystemPrompt
                    | wisp_protocol::context::Category::Tools
                    | wisp_protocol::context::Category::Instructions
            )
        })
        .map(|c| c.tokens)
        .sum()
}

fn priced(
    agent: &Agent,
    model: &str,
    prefix_tokens: u64,
    warm_tokens: u64,
    after_anchor_tokens: u64,
    warm: bool,
) -> Option<f64> {
    let cost = compaction_cost(
        &CompactionCostInput {
            prefix_tokens,
            warm_tokens,
            after_anchor_tokens,
            summary_tokens: crate::context::SUMMARY_TOKENS_ESTIMATE,
            head_tokens: agent.compaction_head_tokens(),
            head_ttl: CacheTtl::OneHour,
            prefix_warm: warm,
            tail_warm: warm,
        },
        pricing::model_info(model).unwrap(),
    );
    Some(usd(cost.estimate_micros))
}

#[tokio::test]
async fn the_compaction_figure_prices_from_the_anchor() {
    let provider = scripted(vec![
        answer("hi", 60_000),
        refused(ErrorKind::InvalidRequest, vec![]),
        refused(ErrorKind::InvalidRequest, vec![]),
        answer("there", 70_000),
    ]);
    let mut rig = Rig::new(provider);
    rig.turn("first").await;
    rig.turn("fails").await;
    let instruction = rig.agent.instruction_estimate();
    assert_eq!(
        instruction,
        crate::context::text_tokens(&wisp_plugin_compaction::summary_instruction(None))
    );
    let report = rig.agent.report(ReportDetail::Summary, RequestId(1));
    let p = prefix(&report);
    assert_eq!(
        report.compact_now_cost_usd,
        priced(
            &rig.agent,
            DEFAULT_MODEL,
            p,
            60_000 - p,
            40 + instruction,
            true
        )
    );

    rig.change(None, Some(OTHER_MODEL), None).await;
    rig.turn("fails on the new model").await;
    let report = rig.agent.report(ReportDetail::Summary, RequestId(2));
    let p = prefix(&report);
    assert_eq!(
        report.compact_now_cost_usd,
        priced(
            &rig.agent,
            OTHER_MODEL,
            p,
            report.next - p,
            instruction,
            false
        )
    );

    rig.turn("works").await;
    let report = rig.agent.report(ReportDetail::Summary, RequestId(3));
    let p = prefix(&report);
    assert_eq!(
        report.compact_now_cost_usd,
        priced(
            &rig.agent,
            OTHER_MODEL,
            p,
            70_000 - p,
            40 + instruction,
            true
        )
    );
}

#[tokio::test]
async fn after_a_top_level_effort_change_the_figure_is_cold_until_a_step_commits() {
    let provider = scripted(vec![
        answer("hi", 60_000),
        refused(ErrorKind::InvalidRequest, vec![]),
    ]);
    let mut rig = Rig::new(provider);
    rig.turn("first").await;
    let warm = rig.agent.report(ReportDetail::Summary, RequestId(1));
    rig.change(None, None, Some(Effort::Low)).await;
    rig.turn("fails").await;
    let cold = rig.agent.report(ReportDetail::Summary, RequestId(2));
    let p = prefix(&cold);
    assert_eq!(
        cold.compact_now_cost_usd,
        priced(
            &rig.agent,
            DEFAULT_MODEL,
            p,
            cold.next - p,
            rig.agent.instruction_estimate(),
            false
        )
    );
    assert!(cold.compact_now_cost_usd > warm.compact_now_cost_usd);
}

#[test]
fn an_entry_without_a_sent_prefix_never_anchors_a_marker() {
    let mut epoch = Epoch::default();
    let mut unknown = entry(1, CacheTtl::OneHour, at(0));
    unknown.prefix = None;
    epoch.push(unknown);
    assert!(epoch.anchor(0, DEFAULT_MODEL).is_some());
    assert_eq!(epoch.tail_marker(0, DEFAULT_MODEL, at(1)), None);
}

#[tokio::test]
async fn each_entry_keeps_the_digest_of_the_prefix_its_request_sent() {
    let provider = scripted(vec![calls_tool(1_000), answer("done", 1_200)]);
    let mut rig = Rig::new(provider);
    rig.turn("list").await;
    let (session, features) = rig.agent.request_parts();
    let rendering = rig.agent.rendering(3);
    let sent = Request {
        session: &session,
        tools: &rig.agent.sent,
        deferred: &[],
        by_value: &[],
        server_tools: rig.agent.server_tools(),
        messages: &rig.agent.history[..3],
        previous_response_id: None,
        features,
        options: Default::default(),
        rendering: &rendering,
        compaction: None,
    };
    let expected = sent_prefix(&sent);
    assert!(expected.is_some());
    assert_eq!(rig.entries()[1].prefix, expected);
    assert_ne!(rig.entries()[0].prefix, expected);
}

#[tokio::test]
async fn a_tools_or_system_change_between_turns_sets_the_mark() {
    let provider = scripted(vec![
        answer("hi", 1_000),
        answer("there", 1_100),
        answer("again", 1_200),
    ]);
    let mut rig = Rig::new(provider);
    rig.turn("first").await;
    rig.agent.set_toolbox(crate::tool::Toolbox::new([
        crate::tool::testkit::bash_entry(),
    ]));
    rig.turn("second").await;
    assert_eq!((rig.entries().len(), rig.mark()), (2, 1));
    rig.agent.session.instructions = Some("<instructions>be brief</instructions>".into());
    rig.turn("third").await;
    assert_eq!((rig.entries().len(), rig.mark()), (3, 2));
}

#[tokio::test]
async fn context_counts_push_no_entry_and_set_no_mark() {
    let provider = scripted(vec![answer("hi", 1_000)]);
    let mut rig = Rig::new(provider.clone());
    rig.turn("first").await;
    rig.agent.set_toolbox(crate::tool::Toolbox::new([
        crate::tool::testkit::bash_entry(),
    ]));
    let before = rig.agent.epoch.clone();
    let (tx, mut rx) = mpsc::channel(64);
    rig.agent
        .context_command(ReportDetail::Summary, RequestId(9), &tx)
        .await;
    while tokio::time::timeout(std::time::Duration::from_millis(200), rx.recv())
        .await
        .is_ok_and(|e| e.is_some())
    {}
    assert_eq!(rig.agent.epoch, before);
    assert_eq!(provider.sent().len(), 1);
}

#[tokio::test]
async fn a_turn_that_ends_before_its_first_request_still_marks_the_rebuild() {
    let provider = scripted(vec![answer("hi", 60_000)]);
    let mut rig = Rig::new(provider.clone());
    rig.turn("first").await;
    let warm = rig.agent.report(ReportDetail::Summary, RequestId(1));
    rig.change(None, None, Some(Effort::Low)).await;
    rig.agent.history.push(Message {
        role: wisp_protocol::Role::System,
        content: vec![ContentBlock::Text {
            text: "stray".into(),
        }],
    });
    let outcome = rig.turn("refused before sending").await;
    assert!(
        matches!(
            outcome,
            TurnOutcome::Error {
                kind: ErrorKind::InvalidRequest,
                ..
            }
        ),
        "{outcome:?}"
    );
    assert_eq!(provider.sent().len(), 1);
    assert_eq!((rig.entries().len(), rig.mark()), (1, 1));
    let cold = rig.agent.report(ReportDetail::Summary, RequestId(2));
    let p = prefix(&cold);
    assert_eq!(
        cold.compact_now_cost_usd,
        priced(
            &rig.agent,
            DEFAULT_MODEL,
            p,
            cold.next - p,
            rig.agent.instruction_estimate(),
            false
        )
    );
    assert!(cold.compact_now_cost_usd > warm.compact_now_cost_usd);
}

#[tokio::test]
async fn cache_lifetime_and_request_option_changes_set_no_mark() {
    let provider = scripted(vec![
        answer("hi", 1_000),
        answer("there", 1_100),
        answer("again", 1_200),
    ]);
    let mut rig = Rig::new(provider.clone());
    rig.turn("first").await;
    let named =
        |name: &str, value| wisp_protocol::settings::SettingChange::named(name, value).unwrap();
    let changes = wisp_protocol::settings::sorted_changes(vec![
        named(
            "cache_ttl",
            wisp_protocol::settings::SettingValue::Text("5m".into()),
        ),
        named(
            "debug_cache",
            wisp_protocol::settings::SettingValue::Bool(true),
        ),
        named(
            "refusal_fallback",
            wisp_protocol::settings::SettingValue::Bool(false),
        ),
    ]);
    rig.agent
        .enqueue(
            crate::agent::queue::Queued::Change {
                request: None,
                changes,
                save: false,
                projected: crate::agent::settings::Projected::default(),
                by: None,
            },
            &rig.events,
        )
        .await;
    rig.turn("second").await;
    assert_eq!((rig.entries().len(), rig.mark()), (2, 0));
    let ttls: Vec<CacheTtl> = rig.entries().iter().map(|e| e.ttl).collect();
    assert_eq!(ttls, [CacheTtl::OneHour, CacheTtl::FiveMinutes]);
    let sent = provider.body(1);
    assert!(sent.get("fallbacks").is_none());
    assert!(sent.get("diagnostics").is_some());
    rig.change(None, Some(OTHER_MODEL), None).await;
    rig.turn("third").await;
    assert_eq!((rig.entries().len(), rig.mark()), (3, 2));
}

#[test]
fn the_logged_byte_reasons_are_the_kernels_reasons_that_change_bytes() {
    let all = [
        MarkReason::EndpointSwitch,
        MarkReason::ModelSwitch,
        MarkReason::EffortChange,
        MarkReason::PerMessageEffortRejected,
        MarkReason::BindingControlsChange,
        MarkReason::NativeSearchRefused,
        MarkReason::ToolsChange,
        MarkReason::SystemChange,
        MarkReason::TailClosed,
        MarkReason::ThinkingCut,
        MarkReason::RenderingForm,
        MarkReason::SearchHistoryUnreadable,
        MarkReason::SearchHistoryRendered,
        MarkReason::AttachmentsAsNotes,
    ];
    for reason in all {
        let listed = match reason {
            MarkReason::EndpointSwitch
            | MarkReason::ModelSwitch
            | MarkReason::EffortChange
            | MarkReason::PerMessageEffortRejected
            | MarkReason::BindingControlsChange => false,
            MarkReason::NativeSearchRefused
            | MarkReason::ToolsChange
            | MarkReason::SystemChange
            | MarkReason::TailClosed
            | MarkReason::ThinkingCut
            | MarkReason::RenderingForm
            | MarkReason::SearchHistoryUnreadable
            | MarkReason::SearchHistoryRendered
            | MarkReason::AttachmentsAsNotes => true,
        };
        assert_eq!(reason.changes_bytes(), listed, "{reason:?}");
    }
    let named: BTreeSet<&str> = all.iter().map(|r| r.as_str()).collect();
    for byte in wisp_protocol::log::BYTE_REASONS {
        assert!(named.contains(byte), "{byte}");
    }
}
