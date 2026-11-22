use std::sync::Mutex;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;

use futures::future::BoxFuture;
use futures::stream::{self, BoxStream};
use futures::{FutureExt, StreamExt};
use tokio::sync::mpsc;
use wisp_protocol::{CacheTtl, ClientCommand, ImageSource, StopReason};

use super::*;
use crate::agent::testing::{Script, ended, started, text};
use crate::agent::tests::usage;
use crate::agent::{DEFAULT_MODEL, LastRequest, default_session, spawn};
use crate::context::CompactionCostInput;
use crate::pricing;
use crate::provider::{Provider, ProviderError, ProviderEvent};
use crate::test_guard::{log_context, pick, until_event, until_turn_end};

#[derive(Debug, Clone)]
struct Asked {
    model: String,
    system: Option<String>,
    instructions: Option<String>,
    tools: Vec<String>,
    server_tools: usize,
    messages: Vec<Message>,
}

type Rule = Box<dyn Fn(&Asked) -> TokenCount + Send + Sync>;

struct Mock {
    replies: Mutex<Vec<mpsc::UnboundedReceiver<Result<ProviderEvent, ProviderError>>>>,
    streams: AtomicUsize,
    counts: AtomicUsize,
    rule: Mutex<Option<Rule>>,
    hold: Mutex<Option<Arc<tokio::sync::Semaphore>>>,
    counted: Mutex<Vec<Asked>>,
    bodies: Mutex<Vec<Vec<u8>>>,
    adaptive: std::sync::atomic::AtomicBool,
}

static ADAPTIVE: crate::provider::ModelCapabilities = crate::provider::ModelCapabilities {
    efforts: &[],
    default_effort: None,
    per_message_effort: false,
    effort_in_id: None,
    adaptive_thinking: true,
    thinking_budgets: None,
    max_output: None,
    images: None,
    inline_tools: false,
    tools_after_results: false,
    deferred_tools: false,
};

impl Mock {
    fn new() -> Arc<Self> {
        Arc::new(Self {
            replies: Mutex::new(vec![]),
            streams: AtomicUsize::new(0),
            counts: AtomicUsize::new(0),
            rule: Mutex::new(None),
            hold: Mutex::new(None),
            counted: Mutex::new(vec![]),
            bodies: Mutex::new(vec![]),
            adaptive: std::sync::atomic::AtomicBool::new(false),
        })
    }

    fn rule(&self, rule: impl Fn(&Asked) -> TokenCount + Send + Sync + 'static) {
        *self.rule.lock().unwrap() = Some(Box::new(rule));
    }

    fn hold(&self) -> Arc<tokio::sync::Semaphore> {
        let gate = Arc::new(tokio::sync::Semaphore::new(0));
        *self.hold.lock().unwrap() = Some(gate.clone());
        gate
    }

    fn asked(&self) -> Vec<Asked> {
        self.counted.lock().unwrap().clone()
    }

    fn reply(&self, events: Script) {
        let (tx, rx) = mpsc::unbounded_channel();
        for e in events {
            tx.send(e).unwrap();
        }
        self.replies.lock().unwrap().insert(0, rx);
    }

    fn live(&self) -> mpsc::UnboundedSender<Result<ProviderEvent, ProviderError>> {
        let (tx, rx) = mpsc::unbounded_channel();
        self.replies.lock().unwrap().insert(0, rx);
        tx
    }
}

impl Provider for Mock {
    fn stream(
        &self,
        request: &Request<'_>,
    ) -> BoxStream<'static, Result<ProviderEvent, ProviderError>> {
        self.streams.fetch_add(1, Ordering::SeqCst);
        if let Ok(body) = crate::provider::anthropic::build_body(request) {
            self.bodies.lock().unwrap().push(body);
        }
        let rx = self
            .replies
            .lock()
            .unwrap()
            .pop()
            .expect("no reply scripted");
        stream::unfold(rx, |mut rx| async move { rx.recv().await.map(|e| (e, rx)) }).boxed()
    }

    fn capabilities(&self, _model: &str) -> Option<crate::provider::ModelCapabilities> {
        self.adaptive.load(Ordering::SeqCst).then_some(ADAPTIVE)
    }

    fn count_tokens(&self, request: &Request<'_>) -> BoxFuture<'static, TokenCount> {
        self.counts.fetch_add(1, Ordering::SeqCst);
        let asked = Asked {
            model: request.session.model.clone(),
            system: request.session.system.clone(),
            instructions: request.session.instructions.clone(),
            tools: request.tools.iter().map(|t| t.name.clone()).collect(),
            server_tools: request.server_tools.len(),
            messages: request.messages.to_vec(),
        };
        self.counted.lock().unwrap().push(asked.clone());
        let answer = match self.rule.lock().unwrap().as_ref() {
            Some(rule) => rule(&asked),
            None => TokenCount::Unsupported,
        };
        let hold = self.hold.lock().unwrap().clone();
        async move {
            if let Some(hold) = hold {
                let _ = hold.acquire().await.map(|permit| permit.forget());
            }
            answer
        }
        .boxed()
    }

    fn cache_prefix(&self, _request: &Request<'_>) -> Option<String> {
        None
    }
}

fn head(input: u64, read: u64) -> Result<ProviderEvent, ProviderError> {
    Ok(started(Some("msg_1"), DEFAULT_MODEL, usage(input, read, 0)))
}

fn tail(input: u64, read: u64, output: u64, iterations: Vec<Iteration>) -> Script {
    ended(StopReason::EndTurn, usage(input, read, output), iterations)
}

fn answer(words: &str, input: u64, read: u64, output: u64) -> Script {
    let mut events = vec![head(input, read)];
    events.extend(text(0, words));
    events.extend(tail(input, read, output, vec![]));
    events
}

fn report_command(detail: ReportDetail, n: u64) -> ClientCommand {
    ClientCommand::ContextReport {
        detail,
        request: RequestId(n),
    }
}

fn is_report(e: &AgentEvent) -> bool {
    matches!(e, AgentEvent::ContextReport(_))
}

#[test]
fn used_is_the_last_iteration_prompt() {
    let top = usage(900, 100, 50);
    assert_eq!(used_of(&top, &[]), 1_000);
    let iterations = vec![
        Iteration {
            kind: "message".into(),
            model: DEFAULT_MODEL.into(),
            usage: usage(500, 0, 10),
        },
        Iteration {
            kind: "message".into(),
            model: DEFAULT_MODEL.into(),
            usage: usage(200, 300, 10),
        },
    ];
    assert_eq!(used_of(&top, &iterations), 500);
    assert_eq!(crate::agent::misses::first_pass(&top, &iterations), 500);
}

#[test]
fn a_tool_label_is_its_first_string_input() {
    assert_eq!(
        call_label("shell", &serde_json::json!({"command": "cargo test\nmore"})),
        "cargo test"
    );
    assert_eq!(call_label("shell", &serde_json::json!({"n": 1})), "shell");
}

#[tokio::test]
async fn a_report_before_any_message_makes_no_request() {
    let provider = Mock::new();
    let agent = Agent::new(provider.clone(), default_session());
    let (commands, mut events, _task) = spawn(agent);
    commands
        .send(report_command(ReportDetail::Other("by_turn".into()), 7))
        .await
        .unwrap();
    let mut seen = vec![];
    let AgentEvent::ContextReport(report) =
        until_event(&mut events, &mut seen, "the context report", is_report).await
    else {
        unreachable!()
    };
    assert_eq!(report.answers, RequestId(7));
    assert_eq!(report.detail, ReportDetail::Summary);
    assert_eq!(report.fill, Fill::None);
    assert!(report.auto_compact);
    assert_eq!(
        report.basis,
        ReportBasis::Estimated {
            reason: EstimateReason::NothingYet
        }
    );
    assert_eq!(report.endpoint, crate::agent::IMPLICIT_ENDPOINT);
    assert_eq!(report.model, DEFAULT_MODEL);
    let window = report.window.clone().unwrap();
    assert_eq!(window.tokens, 1_000_000);
    assert_eq!(report.compact_at.as_ref().unwrap().tokens, 850_000);
    assert_eq!(report.reserve, Some(150_000));
    assert_eq!(report.categories[0].category, Category::SystemPrompt);
    let sum: u64 = report.categories.iter().map(|c| c.tokens).sum();
    assert_eq!(report.next, sum);
    assert_eq!(
        report.compact_now_head_write_usd,
        Some(usd(compaction_cost(
            &CompactionCostInput {
                prefix_tokens: 0,
                warm_tokens: 0,
                after_anchor_tokens: 0,
                summary_tokens: 0,
                head_tokens: Agent::new(Mock::new(), default_session()).compaction_head_tokens(),
                head_ttl: CacheTtl::OneHour,
                prefix_warm: false,
                tail_warm: false,
            },
            pricing::model_info(DEFAULT_MODEL).unwrap()
        )
        .head_write_micros))
    );
    assert_eq!(report.compact_now_head_write_usd, Some(0.048176));
    drop(commands);
    assert_eq!(provider.streams.load(Ordering::SeqCst), 0);
    assert_eq!(provider.counts.load(Ordering::SeqCst), 0);
}

#[tokio::test]
async fn after_a_step_the_meter_and_report_are_measured() {
    let provider = Mock::new();
    provider.reply(answer("hi there", 1_000, 9_000, 300));
    let agent = Agent::new(provider.clone(), default_session());
    let (commands, mut events, _task) = spawn(agent);
    let mut seen = vec![];
    commands
        .send(ClientCommand::send("a", "hello"))
        .await
        .unwrap();
    until_turn_end(&mut events, &mut seen).await;
    let changed = seen
        .iter()
        .find_map(|e| match e {
            AgentEvent::ContextChanged {
                used,
                used_estimated,
                window,
                compact_at,
                meter_limit,
                epoch,
                ..
            } => Some((
                *used,
                *used_estimated,
                *window,
                *compact_at,
                *meter_limit,
                *epoch,
            )),
            _ => None,
        })
        .expect("context_changed");
    assert_eq!(
        changed,
        (
            10_000,
            false,
            Some(1_000_000),
            Some(850_000),
            Some(850_000),
            0
        )
    );
    let ended = seen
        .iter()
        .position(|e| matches!(e, AgentEvent::StepEnded { .. }))
        .unwrap();
    let after = seen
        .iter()
        .position(|e| matches!(e, AgentEvent::ContextChanged { .. }))
        .unwrap();
    assert!(after > ended);
    commands
        .send(report_command(ReportDetail::All, 1))
        .await
        .unwrap();
    let AgentEvent::ContextReport(report) =
        until_event(&mut events, &mut seen, "the context report", is_report).await
    else {
        unreachable!()
    };
    assert_eq!(
        report.fill,
        Fill::Measured {
            used: 10_000,
            turn: 1,
            step: 1
        }
    );
    assert_eq!(report.detail, ReportDetail::All);
    assert_eq!(report.next, 10_300);
    let sum: u64 = report.categories.iter().map(|c| c.tokens).sum();
    assert_eq!(sum, report.next);
    let replies = report
        .categories
        .iter()
        .find(|c| c.category == Category::Replies)
        .unwrap();
    assert_eq!(
        (replies.tokens, &replies.source),
        (300, &FigureSource::Measured)
    );
    for row in &report.categories {
        match row.category {
            Category::SystemPrompt | Category::Tools | Category::Instructions => {
                assert_eq!(row.source, FigureSource::Estimated, "{row:?}");
            }
            Category::Replies => {}
            _ => assert_eq!(row.source, FigureSource::Split, "{row:?}"),
        }
    }
    assert!(report.reread_cost_usd.is_some());
    assert!(report.compact_now_cost_usd.is_some());
}

#[tokio::test]
async fn a_model_switch_updates_the_meter_window() {
    let provider = Mock::new();
    provider.reply(answer("hi", 1_000, 0, 30));
    provider.reply(answer("hi", 1_000, 0, 30));
    let mut config = crate::models::ConfigModels::default();
    let mut row = crate::models::ModelRow::new(DEFAULT_MODEL);
    row.context_window = Some(500_000);
    config.endpoints.insert("other".into(), vec![row]);
    let settings =
        crate::context::ModelSettings::new(crate::models::ModelTable::shipped().unwrap(), config);
    let mut agent = Agent::new(provider.clone(), default_session()).with_model_settings(settings);
    let other = agent.connections[0].clone();
    let mut renamed = other.clone();
    renamed.name = "other".into();
    agent = agent.with_connections(
        vec![other, renamed],
        vec![],
        crate::agent::IMPLICIT_ENDPOINT,
    );
    let (commands, mut events, _task) = spawn(agent);
    let mut seen = vec![];
    commands
        .send(ClientCommand::send("a", "hello"))
        .await
        .unwrap();
    until_turn_end(&mut events, &mut seen).await;
    commands
        .send(ClientCommand::shortcut(
            RequestId(1),
            Some("other".into()),
            None,
            None,
        ))
        .await
        .unwrap();
    seen.clear();
    commands
        .send(ClientCommand::send("b", "again"))
        .await
        .unwrap();
    until_turn_end(&mut events, &mut seen).await;
    let windows: Vec<Option<u64>> = pick!(
        &seen,
        AgentEvent::ContextChanged { window, .. } => *window
    );
    assert_eq!(windows, [Some(500_000), Some(500_000)], "{seen:?}");
    let started = seen
        .iter()
        .position(|e| matches!(e, AgentEvent::StepStarted { .. }))
        .unwrap();
    let first = seen
        .iter()
        .position(|e| matches!(e, AgentEvent::ContextChanged { .. }))
        .unwrap();
    assert!(first < started, "{seen:?}");
}

#[tokio::test]
async fn a_report_before_the_first_message_counts_the_environment() {
    let dir = std::env::temp_dir();
    let workspace = crate::workspace::Workspace {
        root: dir.clone(),
        home: None,
        temp: dir,
    };
    let agent = Agent::new(Mock::new(), default_session())
        .with_tools(crate::tool::test_toolbox(), workspace)
        .with_platform("Linux")
        .with_resources(None);
    let block = wisp_plugin_workspace::environment_block(&agent.environment_facts().unwrap());
    let (commands, mut events, _task) = spawn(agent);
    commands
        .send(report_command(ReportDetail::Summary, 8))
        .await
        .unwrap();
    let mut seen = vec![];
    let AgentEvent::ContextReport(report) =
        until_event(&mut events, &mut seen, "the context report", is_report).await
    else {
        unreachable!()
    };
    let environment = report
        .categories
        .iter()
        .find(|c| c.category == Category::Environment)
        .map(|c| c.tokens);
    assert_eq!(environment, Some(crate::context::text_tokens(&block)));
}

#[tokio::test]
async fn categories_count_images_tool_results_and_the_environment() {
    let provider = Mock::new();
    let mut agent = Agent::new(provider, default_session());
    let png = {
        let mut bytes = Vec::new();
        image::DynamicImage::new_rgb8(100, 100)
            .write_to(
                &mut std::io::Cursor::new(&mut bytes),
                image::ImageFormat::Png,
            )
            .unwrap();
        bytes
    };
    let image = ContentBlock::Image {
        source: ImageSource::Base64 {
            media_type: wisp_protocol::ImageType::Png,
            data: wisp_media::encode_base64(&png).into(),
        },
    };
    agent.history = vec![
        Message {
            role: Role::User,
            content: vec![
                ContentBlock::Text {
                    text: format!("{ENVIRONMENT_TAG}\ncwd: .\n</environment>"),
                },
                ContentBlock::Text {
                    text: "look at this".into(),
                },
                image.clone(),
            ],
        },
        Message {
            role: Role::Assistant,
            content: vec![ContentBlock::ToolUse {
                id: "t1".into(),
                name: "shell".into(),
                input: serde_json::json!({"command": "cargo test"}),
            }],
        },
        Message {
            role: Role::User,
            content: vec![ContentBlock::ToolResult {
                tool_use_id: "t1".into(),
                content: vec![ContentBlock::Text {
                    text: "x".repeat(3_000),
                }],
                is_error: false,
            }],
        },
    ];
    agent.turns = 1;
    agent.sync_fill();
    agent.note_reply(1, 77);
    let rows = agent.categories(3, false);
    let find = |category: Category| rows.iter().find(|r| r.category == category).cloned();
    assert_eq!(find(Category::Replies).unwrap().tokens, 77);
    let results = find(Category::ToolResults).unwrap();
    assert_eq!(results.tokens, 1_000);
    assert_eq!(results.items[0].name, "cargo test");
    assert_eq!(results.items[0].turn, Some(1));
    let images = find(Category::Attachments).unwrap();
    assert_eq!(images.items.len(), 1);
    assert_eq!(
        images.tokens,
        image_tokens(match &image {
            ContentBlock::Image { source } => source,
            _ => unreachable!(),
        })
    );
    assert_eq!(images.items[0].name, "image 1");
    assert!(find(Category::Environment).is_some());
    assert_eq!(find(Category::UserMessages).unwrap().tokens, 4);
    assert!(find(Category::Instructions).is_none());
}

#[tokio::test]
async fn the_instructions_row_reads_the_real_block() {
    let provider = Mock::new();
    let mut session = default_session();
    session.instructions = Some("y".repeat(300));
    let agent = Agent::new(provider, session);
    let rows = agent.categories(0, false);
    let row = rows
        .iter()
        .find(|r| r.category == Category::Instructions)
        .unwrap();
    assert_eq!(row.tokens, 100);
}

fn last(used: u64, ttl: CacheTtl, minutes_ago: i64, agent: &Agent) -> LastRequest {
    LastRequest {
        response_id: None,
        endpoint: agent.endpoint,
        model: DEFAULT_MODEL.into(),
        asked: DEFAULT_MODEL.into(),
        effort: agent.session.effort,
        prompt_tokens: used,
        turn: 1,
        step: 1,
        used,
        output_tokens: 700,
        cache_read: None,
        started_at: Some(
            agent
                .clock
                .now()
                .checked_sub(jiff::SignedDuration::from_mins(minutes_ago))
                .unwrap(),
        ),
        ttl: Some(ttl),
        messages_len: 0,
        first_pass: Some(used),
        print: None,
        fell_back: false,
    }
}

fn anchored(agent: &mut Agent, last: LastRequest) {
    agent.epoch = crate::agent::epoch::Epoch::default();
    agent.last = Some(last);
    agent.note_committed(None);
}

#[tokio::test]
async fn the_compaction_figures_follow_the_anchor() {
    let provider = Mock::new();
    let mut agent = Agent::new(provider, default_session());
    let info = pricing::model_info(DEFAULT_MODEL).unwrap();
    let request = last(67_400, CacheTtl::OneHour, 1, &agent);
    anchored(&mut agent, request);
    let report = agent.report(ReportDetail::Summary, RequestId(1));
    let prefix: u64 = report
        .categories
        .iter()
        .filter(|c| matches!(c.category, Category::SystemPrompt | Category::Tools))
        .map(|c| c.tokens)
        .sum();
    let warm = compaction_cost(
        &CompactionCostInput {
            prefix_tokens: prefix,
            warm_tokens: 67_400 - prefix,
            after_anchor_tokens: 700 + crate::agent::compact::instruction_tokens(),
            summary_tokens: context::SUMMARY_TOKENS_ESTIMATE,
            head_tokens: agent.compaction_head_tokens(),
            head_ttl: CacheTtl::OneHour,
            prefix_warm: true,
            tail_warm: true,
        },
        info,
    );
    assert_eq!(report.compact_now_cost_usd, Some(usd(warm.estimate_micros)));
    assert_eq!(report.compact_now_head_write_usd, Some(0.048176));
    agent.session.cache_ttl = CacheTtl::FiveMinutes;
    let report = agent.report(ReportDetail::Summary, RequestId(2));
    assert_eq!(report.compact_now_head_write_usd, Some(0.03011));
    agent.session.cache_ttl = CacheTtl::OneHour;
    let request = last(67_400, CacheTtl::FiveMinutes, 10, &agent);
    anchored(&mut agent, request);
    let cold_tail = agent.report(ReportDetail::Summary, RequestId(3));
    assert!(cold_tail.compact_now_cost_usd > report.compact_now_cost_usd);
    let mut moved = last(67_400, CacheTtl::OneHour, 1, &agent);
    moved.asked = "claude-haiku-5-5".into();
    anchored(&mut agent, moved);
    let switched = agent.report(ReportDetail::Summary, RequestId(4));
    let expired = {
        let request = last(67_400, CacheTtl::OneHour, 61, &agent);
        anchored(&mut agent, request);
        agent.report(ReportDetail::Summary, RequestId(5))
    };
    assert_eq!(switched.compact_now_cost_usd, expired.compact_now_cost_usd);
    assert!(expired.compact_now_cost_usd > cold_tail.compact_now_cost_usd);
}

#[tokio::test]
async fn each_request_records_when_it_went_out() {
    let provider = Mock::new();
    let mut paused = vec![head(1_000, 0)];
    paused.extend(text(0, "searching"));
    paused.extend(ended(StopReason::PauseTurn, usage(1_000, 0, 10), vec![]));
    provider.reply(paused);
    provider.reply(answer("done", 1_000, 0, 30));
    let clock = crate::clock::TickingClock::new();
    let start = crate::clock::Clock::now(clock.as_ref());
    let mut agent = Agent::new(provider, default_session()).with_clock(clock.clone());
    let (_keep, mut commands) = mpsc::channel(8);
    let (events, mut sink) = mpsc::channel(1_024);
    tokio::spawn(async move { while sink.recv().await.is_some() {} });
    agent
        .run_turn("go".into(), &mut commands, &events)
        .await
        .unwrap();
    let last = agent.last.as_ref().unwrap();
    assert_eq!(last.step, 2);
    let minutes = |n| {
        start
            .checked_add(jiff::SignedDuration::from_mins(n))
            .unwrap()
    };
    assert_eq!(last.started_at, Some(minutes(3)));
}

fn split_row(category: Category, tokens: u64, source: FigureSource) -> CategoryEstimate {
    CategoryEstimate {
        category,
        tokens,
        source,
        items: vec![],
    }
}

fn split_rows() -> Vec<CategoryEstimate> {
    vec![
        split_row(Category::SystemPrompt, 300, FigureSource::Estimated),
        split_row(Category::Tools, 1_600, FigureSource::Estimated),
        split_row(Category::Instructions, 3_400, FigureSource::Estimated),
        split_row(Category::Environment, 100, FigureSource::Estimated),
        split_row(Category::UserMessages, 5_000, FigureSource::Estimated),
        split_row(Category::Replies, 19_300, FigureSource::Measured),
        CategoryEstimate {
            items: vec![
                ItemEstimate {
                    name: "cargo test".into(),
                    tokens: 9_000,
                    turn: Some(7),
                    unestimated: false,
                },
                ItemEstimate {
                    name: "git log".into(),
                    tokens: 3_000,
                    turn: Some(6),
                    unestimated: false,
                },
            ],
            ..split_row(Category::ToolResults, 30_000, FigureSource::Estimated)
        },
        split_row(Category::Attachments, 2_000, FigureSource::Estimated),
    ]
}

fn tokens_of(rows: &[CategoryEstimate], category: Category) -> u64 {
    rows.iter()
        .find(|r| r.category == category)
        .map_or(0, |r| r.tokens)
}

#[test]
fn the_split_adds_up_to_next_and_keeps_replies_measured() {
    for next in [63_100, 61_700, 100_000, 24_600] {
        let rows = split(split_rows(), next);
        assert_eq!(rows.iter().map(|r| r.tokens).sum::<u64>(), next, "{next}");
        let replies = rows
            .iter()
            .find(|r| r.category == Category::Replies)
            .unwrap();
        assert_eq!(replies.tokens, 19_300);
        assert_eq!(replies.source, FigureSource::Measured);
        for row in &rows {
            if matches!(
                row.category,
                Category::SystemPrompt | Category::Tools | Category::Instructions
            ) {
                assert_eq!(row.source, FigureSource::Estimated);
            }
        }
        assert_eq!(tokens_of(&rows, Category::Tools), 1_600);
    }
    let rows = split(split_rows(), 63_100);
    let pool = 63_100 - 300 - 1_600 - 3_400 - 19_300;
    let results = rows
        .iter()
        .find(|r| r.category == Category::ToolResults)
        .unwrap();
    assert_eq!(results.source, FigureSource::Split);
    assert_eq!(results.tokens, pool * 30_000 / 37_100);
    let items: u64 = results.items.iter().map(|i| i.tokens).sum();
    assert!(items <= results.tokens, "{items} of {}", results.tokens);
    assert_eq!(results.tokens, 31_132);
    assert_eq!(results.items[0].tokens, 9_340);
}

#[test]
fn a_negative_remainder_scales_every_conversation_row_and_marks_replies_split() {
    let next = 5_300 + 12_000;
    let rows = split(split_rows(), next);
    assert_eq!(rows.iter().map(|r| r.tokens).sum::<u64>(), next);
    assert_eq!(tokens_of(&rows, Category::Instructions), 3_400);
    let replies = rows
        .iter()
        .find(|r| r.category == Category::Replies)
        .unwrap();
    assert_eq!(replies.source, FigureSource::Split);
    assert!(replies.tokens < 19_300);
    assert!(rows.iter().all(|r| r.tokens <= 30_000));
}

#[test]
fn a_fill_below_the_fixed_part_scales_every_row_with_none_negative() {
    let rows = split(split_rows(), 2_650);
    assert_eq!(rows.iter().map(|r| r.tokens).sum::<u64>(), 2_650);
    assert!(rows.iter().all(|r| r.source == FigureSource::Split));
    assert!(tokens_of(&rows, Category::Tools) < 1_600);
    assert!(tokens_of(&rows, Category::Instructions) < 3_400);
}

#[test]
fn an_estimated_reply_joins_the_pool() {
    let mut rows = split_rows();
    rows[5].source = FigureSource::Estimated;
    let rows = split(rows, 63_100);
    assert_eq!(rows.iter().map(|r| r.tokens).sum::<u64>(), 63_100);
    let replies = rows
        .iter()
        .find(|r| r.category == Category::Replies)
        .unwrap();
    assert_eq!(replies.source, FigureSource::Split);
    assert_ne!(replies.tokens, 19_300);
}

#[test]
fn a_leftover_with_no_conversation_row_goes_to_your_messages() {
    let rows = vec![
        split_row(Category::SystemPrompt, 300, FigureSource::Estimated),
        split_row(Category::Tools, 1_600, FigureSource::Estimated),
        split_row(Category::Attachments, 0, FigureSource::Estimated),
    ];
    let rows = split(rows, 2_500);
    let order: Vec<Category> = rows.iter().map(|r| r.category.clone()).collect();
    assert_eq!(
        order,
        vec![
            Category::SystemPrompt,
            Category::Tools,
            Category::UserMessages,
            Category::Attachments
        ]
    );
    assert_eq!(tokens_of(&rows, Category::UserMessages), 600);
    assert_eq!(rows.iter().map(|r| r.tokens).sum::<u64>(), 2_500);
}

#[test]
fn apportion_is_exact_and_proportional() {
    assert_eq!(apportion(10, &[1, 1, 1]), vec![4, 3, 3]);
    assert_eq!(apportion(7, &[0, 5, 0]), vec![0, 7, 0]);
    assert_eq!(apportion(5, &[0, 0]), vec![0, 0]);
    assert_eq!(apportion(100, &[30, 70]), vec![30, 70]);
    let shares = apportion(1_000_003, &[3, 7, 11, 13]);
    assert_eq!(shares.iter().sum::<u64>(), 1_000_003);
}

mod count;

#[test]
fn plan_context_reports_provenance_and_pricing_none_hides_all_costs() {
    use crate::provider::anthropic::AnthropicProvider;
    use crate::provider::credential::{Credential, SubscriptionKind};
    let provider = Arc::new(
        AnthropicProvider::new(
            &Credential::Subscription {
                kind: SubscriptionKind::Anthropic,
                secret: "synthetic-plan".into(),
                profile: None,
            },
            "https://api.anthropic.com",
        )
        .unwrap(),
    );
    let mut agent = Agent::new(provider.clone(), default_session());
    agent.compaction.cost_usd = Some(0.3);
    agent.session_billing = Some(wisp_protocol::Billing::Mixed);
    let report = agent.report(ReportDetail::Summary, RequestId(1));
    assert_eq!(report.billing, wisp_protocol::Billing::AnthropicPlan);
    assert_eq!(report.spent_billing, wisp_protocol::Billing::Mixed);
    assert!(report.compact_now_cost_usd.is_some());
    let agent = agent.with_connections(
        vec![crate::agent::Connection {
            name: "plan".into(),
            provider,
            priced: false,
            context: crate::agent::LogContext {
                endpoint_name: "plan".into(),
                ..log_context()
            },
        }],
        vec![],
        "plan",
    );
    let report = agent.report(ReportDetail::Summary, RequestId(2));
    assert_eq!(report.compaction_cost_usd, None);
    assert_eq!(report.reread_cost_usd, None);
    assert_eq!(report.compact_now_cost_usd, None);
    assert_eq!(report.compact_now_head_write_usd, None);
}

fn part_tokens(parts: &[PartTokens], part: FillPart) -> u64 {
    parts
        .iter()
        .filter(|p| p.part == part)
        .map(|p| p.tokens)
        .sum()
}

#[tokio::test]
async fn the_parts_add_up_to_the_figure_and_keep_the_fixed_estimates() {
    let dir = std::env::temp_dir();
    let workspace = crate::workspace::Workspace {
        root: dir.clone(),
        home: None,
        temp: dir,
    };
    let mut session = default_session();
    session.system = Some("s".repeat(3_000));
    session.instructions = Some("y".repeat(300));
    let agent = Agent::new(Mock::new(), session).with_tools(crate::tool::test_toolbox(), workspace);
    let rows = agent.categories(agent.history.len(), false);
    let row = |category: Category| {
        rows.iter()
            .find(|r| r.category == category)
            .map_or(0, |r| r.tokens)
    };
    let system = row(Category::SystemPrompt) + row(Category::Instructions);
    let tools = row(Category::Tools);
    assert!(system > 0 && tools > 0);
    let fixed = system + tools;
    for total in [fixed + 5_000, fixed, fixed / 2, 0] {
        let parts = agent.parts(total);
        assert_eq!(
            parts.iter().map(|p| p.tokens).sum::<u64>(),
            total,
            "{total}"
        );
        assert!(parts.iter().all(|p| p.tokens > 0), "{parts:?}");
        let order: Vec<FillPart> = parts.iter().map(|p| p.part).collect();
        let mut sorted = order.clone();
        sorted.sort();
        assert_eq!(order, sorted);
    }
    for total in [fixed + 5_000, fixed] {
        let parts = agent.parts(total);
        assert_eq!(part_tokens(&parts, FillPart::System), system);
        assert_eq!(part_tokens(&parts, FillPart::Tools), tools);
        assert_eq!(part_tokens(&parts, FillPart::Chat), total - fixed);
    }
    let below = agent.parts(fixed / 2);
    assert!(part_tokens(&below, FillPart::System) < system);
    assert_eq!(part_tokens(&below, FillPart::Chat), 0);
}

#[tokio::test]
async fn before_the_first_request_chat_is_the_pending_environment() {
    let dir = std::env::temp_dir();
    let workspace = crate::workspace::Workspace {
        root: dir.clone(),
        home: None,
        temp: dir,
    };
    let mut agent = Agent::new(Mock::new(), default_session())
        .with_tools(crate::tool::test_toolbox(), workspace)
        .with_platform("Linux")
        .with_resources(None);
    let block = wisp_plugin_workspace::environment_block(&agent.environment_facts().unwrap());
    let (closed, mut commands) = mpsc::channel(1);
    drop(closed);
    let (events, mut sink) = mpsc::channel(1_024);
    tokio::spawn(async move { while sink.recv().await.is_some() {} });
    agent.serve(&mut commands, &events).await;
    let AgentEvent::ContextChanged { used, .. } = agent.context_figures() else {
        unreachable!()
    };
    let parts = agent.parts(used);
    assert_eq!(
        part_tokens(&parts, FillPart::Chat),
        crate::context::text_tokens(&block)
    );
    assert_eq!(agent.cached(), None);
}

#[tokio::test]
async fn cached_is_the_last_requests_share_read_from_the_cache() {
    let provider = Mock::new();
    provider.reply(answer("hi there", 1_000, 9_000, 300));
    provider.reply(answer("again", 7_100, 2_900, 300));
    let mut agent = Agent::new(provider, default_session());
    assert_eq!(agent.cached(), None);
    let (_keep, mut commands) = mpsc::channel(8);
    let (events, mut sink) = mpsc::channel(1_024);
    tokio::spawn(async move { while sink.recv().await.is_some() {} });
    let mut shares = Vec::new();
    for text in ["go", "on"] {
        agent
            .run_turn(text.into(), &mut commands, &events)
            .await
            .unwrap();
        shares.push(agent.cached());
    }
    assert_eq!(shares, [Some(90), Some(29)]);
}
