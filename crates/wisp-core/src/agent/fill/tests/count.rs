use std::sync::atomic::Ordering;

use serde_json::json;
use wisp_protocol::MessageTime;
use wisp_protocol::context::ContextReport;

use super::*;
use crate::agent::BASE_SYSTEM_PROMPT;
use crate::agent::testing::{Fake, Knobs, Sent, body_of};
use crate::provider::anthropic::{AnthropicProvider, Credential};
use crate::test_guard::{http, until, until_event, within};
use crate::tool::test_toolbox;

const FRAMING: u64 = 3;
const PER_TOOL: u64 = 100;
const PER_MESSAGE: u64 = 50;
const SONNET: &str = "claude-sonnet-5-5";
const KIND: &str = "Be kind to the reader.";

fn counting() -> Arc<Fake> {
    Fake::with(
        Knobs {
            count: Some(1),
            ..Knobs::default()
        },
        [],
    )
}

fn size(asked: &Asked) -> u64 {
    let text = |t: &Option<String>| t.as_deref().map_or(0, str::len) as u64;
    FRAMING
        + text(&asked.system)
        + text(&asked.instructions)
        + PER_TOOL * asked.tools.len() as u64
        + PER_MESSAGE * asked.messages.len() as u64
}

fn sizes(asked: &Asked) -> TokenCount {
    TokenCount::Counted {
        tokens: size(asked),
    }
}

fn is_placeholder(asked: &Asked) -> bool {
    asked.messages == [Message::user_text(".")]
}

fn reply_from(model: &str, input: u64, read: u64, output: u64) -> Script {
    let mut events = vec![Ok(started(Some("msg_1"), model, usage(input, read, 0)))];
    events.extend(text(0, "ok"));
    events.extend(tail(input, read, output, vec![]));
    events
}

struct Rig {
    agent: Agent,
    provider: Arc<Mock>,
    tx: Option<mpsc::Sender<AgentEvent>>,
    rx: mpsc::Receiver<AgentEvent>,
    asked: u64,
}

fn rig_with(instructions: Option<&str>, tools: bool, provider: Arc<Mock>) -> Rig {
    let mut session = default_session();
    session.instructions = instructions.map(str::to_owned);
    session.message_time = MessageTime::Off;
    let mut agent = Agent::new(provider.clone(), session);
    if tools {
        agent.set_toolbox(test_toolbox());
    }
    let (tx, rx) = mpsc::channel(64);
    Rig {
        agent,
        provider,
        tx: Some(tx),
        rx,
        asked: 0,
    }
}

fn rig(instructions: Option<&str>, tools: bool) -> Rig {
    let provider = Mock::new();
    provider.rule(sizes);
    rig_with(instructions, tools, provider)
}

impl Rig {
    async fn turn(&mut self, model: &str) {
        self.provider.reply(reply_from(model, 1_000, 9_000, 300));
        let (_keep, mut commands) = mpsc::channel(8);
        let (events, mut sink) = mpsc::channel(1_024);
        tokio::spawn(async move { while sink.recv().await.is_some() {} });
        self.agent
            .run_turn("go".into(), &mut commands, &events)
            .await
            .unwrap();
    }

    fn calls(&self) -> usize {
        self.provider.counts.load(Ordering::SeqCst)
    }

    async fn ask(&mut self) -> ContextReport {
        self.asked += 1;
        let tx = self.tx.clone().unwrap();
        self.agent
            .context_command(ReportDetail::Summary, RequestId(self.asked), &tx)
            .await;
        self.next().await
    }

    async fn next(&mut self) -> ContextReport {
        let event = within("a context report", self.rx.recv())
            .await
            .expect("events closed");
        let AgentEvent::ContextReport(report) = event else {
            panic!("{event:?}")
        };
        report
    }

    async fn quiet(&mut self) {
        let more = tokio::time::timeout(Duration::from_millis(100), self.rx.recv()).await;
        assert!(more.is_err(), "{more:?}");
    }

    async fn report(&mut self) -> Vec<ContextReport> {
        let first = self.ask().await;
        if first.basis.settled() {
            self.quiet().await;
            return vec![first];
        }
        let last = self.next().await;
        assert_eq!(last.answers, first.answers);
        assert!(last.basis.settled(), "{last:?}");
        self.quiet().await;
        vec![first, last]
    }
}

fn row(report: &ContextReport, category: Category) -> CategoryEstimate {
    report
        .categories
        .iter()
        .find(|r| r.category == category)
        .cloned()
        .unwrap_or_else(|| panic!("no {category:?} in {report:?}"))
}

fn rows_total(report: &ContextReport) -> u64 {
    report.categories.iter().map(|r| r.tokens).sum()
}

#[tokio::test]
async fn the_first_report_counts_four_parts_and_the_second_none() {
    let mut rig = rig(Some(KIND), true);
    rig.turn(DEFAULT_MODEL).await;
    let reports = rig.report().await;
    assert_eq!(rig.calls(), 4);
    assert_eq!(reports.len(), 2);
    let (first, last) = (&reports[0], &reports[1]);
    assert_eq!(first.basis, ReportBasis::Counting);
    for category in [
        Category::SystemPrompt,
        Category::Instructions,
        Category::Tools,
    ] {
        assert_eq!(row(first, category.clone()).source, FigureSource::Estimated);
        assert_eq!(row(last, category).source, FigureSource::Counted);
    }
    assert_eq!(last.basis, ReportBasis::Counted);
    assert_eq!(
        row(last, Category::SystemPrompt).tokens,
        rig.agent.session.system.as_deref().unwrap().len() as u64
    );
    assert!(
        rig.agent
            .session
            .system
            .as_deref()
            .unwrap()
            .starts_with(BASE_SYSTEM_PROMPT)
    );
    assert_eq!(row(last, Category::Instructions).tokens, KIND.len() as u64);
    assert_eq!(row(last, Category::Tools).tokens, 2 * PER_TOOL);
    let tools = row(last, Category::Tools);
    assert_eq!(
        tools.items.iter().map(|i| i.tokens).sum::<u64>(),
        tools.tokens
    );
    assert_eq!(row(last, Category::Replies).source, FigureSource::Measured);
    assert_eq!(last.next, 10_300);
    assert_eq!(rows_total(last), last.next);
    assert_eq!(rows_total(first), first.next);
    let asked = rig.provider.asked();
    assert!(asked.iter().all(is_placeholder), "{asked:?}");
    assert!(asked.iter().all(|a| a.server_tools == 0), "{asked:?}");
    assert!(asked.iter().all(|a| a.model == DEFAULT_MODEL));
    let shapes: Vec<(bool, bool, usize)> = asked
        .iter()
        .map(|a| (a.system.is_some(), a.instructions.is_some(), a.tools.len()))
        .collect();
    assert_eq!(
        shapes,
        [
            (false, false, 0),
            (true, false, 0),
            (true, true, 0),
            (true, true, 2)
        ]
    );
    let again = rig.report().await;
    assert_eq!(rig.calls(), 4);
    assert_eq!(again.len(), 1);
    assert_eq!(again[0].basis, ReportBasis::Counted);
    assert_eq!(again[0].categories, last.categories);
}

#[tokio::test]
async fn a_changed_instructions_block_counts_only_its_parts_again() {
    let mut rig = rig(Some(KIND), true);
    rig.turn(DEFAULT_MODEL).await;
    rig.report().await;
    assert_eq!(rig.calls(), 4);
    rig.agent.session.instructions = Some("Be terse.".into());
    let reports = rig.report().await;
    assert_eq!(rig.calls(), 6);
    let fresh: Vec<Asked> = rig.provider.asked().split_off(4);
    assert!(
        fresh
            .iter()
            .all(|a| a.instructions.as_deref() == Some("Be terse.")),
        "{fresh:?}"
    );
    assert_eq!(fresh[1].tools.len(), 2);
    let last = reports.last().unwrap();
    assert_eq!(last.basis, ReportBasis::Counted);
    assert_eq!(row(last, Category::Instructions).tokens, 9);
}

#[tokio::test]
async fn no_instructions_and_no_tools_count_two_parts() {
    let mut rig = rig(None, false);
    rig.turn(DEFAULT_MODEL).await;
    let reports = rig.report().await;
    assert_eq!(rig.calls(), 2);
    let last = reports.last().unwrap();
    assert_eq!(last.basis, ReportBasis::Counted);
    assert!(
        last.categories
            .iter()
            .all(|r| !matches!(r.category, Category::Instructions | Category::Tools))
    );
}

#[tokio::test]
async fn a_model_switch_counts_again_and_the_whole_request_until_it_is_measured() {
    let mut rig = rig(Some(KIND), true);
    rig.turn(DEFAULT_MODEL).await;
    rig.report().await;
    assert_eq!(rig.calls(), 4);
    rig.agent.session.model = SONNET.into();
    let reports = rig.report().await;
    assert_eq!(rig.calls(), 9);
    let fresh = rig.provider.asked().split_off(4);
    assert!(fresh.iter().all(|a| a.model == SONNET));
    let whole: Vec<&Asked> = fresh.iter().filter(|a| !is_placeholder(a)).collect();
    assert_eq!(whole.len(), 1);
    assert_eq!(whole[0].tools.len(), 2);
    assert_eq!(whole[0].server_tools, 0);
    let last = reports.last().unwrap();
    assert_eq!(last.basis, ReportBasis::Counted);
    assert_eq!(last.next, size(whole[0]) + 300);
    assert_eq!(rows_total(last), last.next);
    rig.turn(SONNET).await;
    let measured = rig.report().await;
    assert_eq!(rig.calls(), 9);
    assert_eq!(measured.len(), 1);
    assert_eq!(measured[0].basis, ReportBasis::Counted);
    assert_eq!(measured[0].next, 10_300);
}

#[tokio::test]
async fn an_endpoint_switch_on_the_same_model_counts_again() {
    let mut rig = rig(None, true);
    let first = rig.agent.connections[0].clone();
    let mut other = first.clone();
    other.name = "other".into();
    rig.agent = std::mem::replace(&mut rig.agent, Agent::new(Mock::new(), default_session()))
        .with_connections(vec![first, other], vec![], crate::agent::IMPLICIT_ENDPOINT);
    rig.turn(DEFAULT_MODEL).await;
    rig.report().await;
    assert_eq!(rig.calls(), 3);
    rig.agent.use_connection(1).unwrap();
    assert_eq!(rig.agent.endpoint_name(), "other");
    let reports = rig.report().await;
    assert_eq!(rig.calls(), 7);
    let fresh = rig.provider.asked().split_off(3);
    assert_eq!(fresh.iter().filter(|a| is_placeholder(a)).count(), 3);
    assert!(fresh.iter().all(|a| a.model == DEFAULT_MODEL));
    let last = reports.last().unwrap();
    assert_eq!(last.endpoint, "other");
    assert_eq!(last.basis, ReportBasis::Counted);
    rig.agent.use_connection(0).unwrap();
    rig.report().await;
    assert_eq!(rig.calls(), 7);
}

#[tokio::test]
async fn a_change_in_thinking_features_alone_counts_again() {
    let mut rig = rig(None, true);
    rig.turn(DEFAULT_MODEL).await;
    rig.report().await;
    assert_eq!(rig.calls(), 3);
    let (before, _) = rig.agent.request_parts();
    rig.provider.adaptive.store(true, Ordering::SeqCst);
    rig.agent.settings.binding_controls = true;
    let (after, features) = rig.agent.request_parts();
    assert_eq!(after, before);
    assert!(features.binding_controls.is_some());
    let reports = rig.report().await;
    assert_eq!(rig.calls(), 6);
    assert!(rig.provider.asked().split_off(3).iter().all(is_placeholder));
    assert_eq!(reports.last().unwrap().basis, ReportBasis::Counted);
}

#[tokio::test]
async fn a_fallback_answer_from_another_model_takes_the_whole_request_path() {
    let mut rig = rig(None, true);
    rig.turn(DEFAULT_MODEL).await;
    rig.report().await;
    assert_eq!(rig.calls(), 3);
    if let Some(last) = rig.agent.last.as_mut() {
        last.model = "claude-haiku-5-5".into();
    }
    let reports = rig.report().await;
    assert_eq!(rig.calls(), 4);
    let whole = rig.provider.asked().pop().unwrap();
    assert!(!is_placeholder(&whole));
    let last = reports.last().unwrap();
    assert_eq!(last.basis, ReportBasis::Counted);
    assert_eq!(last.next, size(&whole) + 300);
}

#[tokio::test]
async fn a_resumed_history_without_a_request_counts_the_whole_request() {
    let mut rig = rig(None, false);
    rig.agent.history = vec![Message::user_text("hello there")];
    rig.agent.sync_fill();
    let reports = rig.report().await;
    assert_eq!(rig.calls(), 3);
    let whole = rig
        .provider
        .asked()
        .into_iter()
        .find(|a| !is_placeholder(a))
        .unwrap();
    let last = reports.last().unwrap();
    assert_eq!(last.fill, Fill::None);
    assert_eq!(last.basis, ReportBasis::Counted);
    assert_eq!(last.next, size(&whole));
    assert_eq!(rows_total(last), last.next);
}

#[tokio::test]
async fn an_endpoint_without_a_counter_is_learned_for_the_session() {
    let provider = Mock::new();
    provider.rule(|_| TokenCount::Unsupported);
    let mut rig = rig_with(None, true, provider);
    rig.turn(DEFAULT_MODEL).await;
    let reports = rig.report().await;
    assert_eq!(rig.calls(), 3);
    let no_counter = ReportBasis::Estimated {
        reason: EstimateReason::NoCounter,
    };
    assert_eq!(reports[1].basis, no_counter);
    assert!(
        reports[1]
            .categories
            .iter()
            .all(|r| r.source != FigureSource::Counted)
    );
    assert_eq!(rows_total(&reports[1]), reports[1].next);
    assert!(rig.agent.uncountable(crate::agent::IMPLICIT_ENDPOINT));
    let again = rig.report().await;
    assert_eq!(rig.calls(), 3);
    assert_eq!(again.len(), 1);
    assert_eq!(again[0].basis, no_counter);
}

#[tokio::test]
async fn a_failed_part_is_estimated_kept_out_of_the_cache_and_retried_alone() {
    let provider = Mock::new();
    provider.rule(|asked| {
        if asked.tools.is_empty() {
            sizes(asked)
        } else {
            TokenCount::Failed {
                reason: "timed out".into(),
            }
        }
    });
    let mut rig = rig_with(None, true, provider);
    rig.turn(DEFAULT_MODEL).await;
    let reports = rig.report().await;
    assert_eq!(rig.calls(), 3);
    let last = reports.last().unwrap();
    assert_eq!(
        last.basis,
        ReportBasis::PartlyEstimated {
            reason: "timed out".into()
        }
    );
    assert_eq!(
        row(last, Category::SystemPrompt).source,
        FigureSource::Counted
    );
    assert_eq!(row(last, Category::Tools).source, FigureSource::Estimated);
    assert_eq!(rows_total(last), last.next);
    assert!(!rig.agent.uncountable(crate::agent::IMPLICIT_ENDPOINT));
    rig.provider.rule(sizes);
    let retried = rig.report().await;
    assert_eq!(rig.calls(), 4);
    assert_eq!(rig.provider.asked().pop().unwrap().tools.len(), 2);
    assert_eq!(retried.last().unwrap().basis, ReportBasis::Counted);
}

#[tokio::test]
async fn when_every_part_fails_the_report_is_estimated_with_the_reason() {
    let provider = Mock::new();
    provider.rule(|_| TokenCount::Failed {
        reason: "HTTP 500".into(),
    });
    let mut rig = rig_with(None, true, provider);
    rig.turn(DEFAULT_MODEL).await;
    let reports = rig.report().await;
    let last = reports.last().unwrap();
    assert_eq!(
        last.basis,
        ReportBasis::Estimated {
            reason: EstimateReason::Failed("HTTP 500".into())
        }
    );
    assert!(
        last.categories
            .iter()
            .all(|r| r.source != FigureSource::Counted)
    );
}

#[tokio::test]
async fn counting_off_never_asks_the_endpoint() {
    let provider = Mock::new();
    provider.rule(|asked| panic!("counted with counting off: {asked:?}"));
    let Rig {
        agent,
        provider,
        tx,
        rx,
        asked,
    } = rig_with(Some(KIND), true, provider);
    let mut rig = Rig {
        agent: agent.with_count_tokens(false),
        provider,
        tx,
        rx,
        asked,
    };
    rig.turn(DEFAULT_MODEL).await;
    for _ in 0..2 {
        let reports = rig.report().await;
        assert_eq!(reports.len(), 1);
        assert_eq!(
            reports[0].basis,
            ReportBasis::Estimated {
                reason: EstimateReason::Off
            }
        );
    }
    assert_eq!(rig.calls(), 0);
}

#[tokio::test]
async fn the_late_report_describes_the_moment_it_was_asked() {
    let mut rig = rig(None, true);
    rig.turn(DEFAULT_MODEL).await;
    let gate = rig.provider.hold();
    let first = rig.ask().await;
    assert_eq!(first.basis, ReportBasis::Counting);
    let before = rig.agent.history.len();
    rig.turn(DEFAULT_MODEL).await;
    assert!(rig.agent.history.len() > before);
    gate.add_permits(16);
    let last = rig.next().await;
    assert_eq!(last.answers, first.answers);
    assert_eq!(last.basis, ReportBasis::Counted);
    assert_eq!(last.fill, first.fill);
    assert_eq!(last.next, first.next);
    assert_eq!(rows_total(&last), first.next);
}

#[tokio::test]
async fn stopping_the_counts_aborts_the_task() {
    let mut rig = rig(None, true);
    rig.turn(DEFAULT_MODEL).await;
    let gate = rig.provider.hold();
    let first = rig.ask().await;
    assert_eq!(first.basis, ReportBasis::Counting);
    rig.agent.stop_counts();
    drop(rig.tx.take());
    gate.add_permits(16);
    let closed = within("the events' end", rig.rx.recv()).await;
    assert!(closed.is_none(), "{closed:?}");
}

#[tokio::test]
async fn a_count_in_flight_never_holds_up_a_streaming_reply() {
    let provider = Mock::new();
    provider.rule(sizes);
    provider.reply(answer("first", 1_000, 0, 30));
    let live = provider.live();
    let gate = provider.hold();
    let mut session = default_session();
    session.message_time = MessageTime::Off;
    let agent = Agent::new(provider.clone(), session);
    let (commands, mut events, _task) = spawn(agent);
    let mut seen = vec![];
    commands
        .send(ClientCommand::send("z", "first"))
        .await
        .unwrap();
    until_turn_end(&mut events, &mut seen).await;
    seen.clear();
    commands
        .send(ClientCommand::send("a", "hello"))
        .await
        .unwrap();
    live.send(head(1_000, 0)).unwrap();
    for e in text(0, "partial") {
        live.send(e).unwrap();
    }
    until_event(&mut events, &mut seen, "the event", |e| {
        matches!(e, AgentEvent::TextDelta { .. })
    })
    .await;
    commands
        .send(report_command(ReportDetail::Summary, 9))
        .await
        .unwrap();
    let AgentEvent::ContextReport(first) =
        until_event(&mut events, &mut seen, "the context report", is_report).await
    else {
        unreachable!()
    };
    assert_eq!(first.basis, ReportBasis::Counting);
    for e in tail(1_000, 0, 20, vec![]) {
        live.send(e).unwrap();
    }
    drop(live);
    until_turn_end(&mut events, &mut seen).await;
    assert_eq!(seen.iter().filter(|e| is_report(e)).count(), 1);
    gate.add_permits(16);
    let AgentEvent::ContextReport(last) =
        until_event(&mut events, &mut seen, "the context report", is_report).await
    else {
        unreachable!()
    };
    assert_eq!(last.answers, RequestId(9));
    assert_eq!(last.basis, ReportBasis::Counted);
}

#[tokio::test]
async fn a_pending_count_never_holds_the_session_open() {
    let provider = Mock::new();
    provider.rule(sizes);
    provider.reply(answer("hi", 1_000, 0, 30));
    let _gate = provider.hold();
    let agent = Agent::new(provider.clone(), default_session());
    let (commands, mut events, _task) = spawn(agent);
    let mut seen = vec![];
    commands
        .send(ClientCommand::send("a", "hello"))
        .await
        .unwrap();
    until_turn_end(&mut events, &mut seen).await;
    commands
        .send(report_command(ReportDetail::Summary, 2))
        .await
        .unwrap();
    until_event(&mut events, &mut seen, "the context report", is_report).await;
    drop(commands);
    within("the events' end", async {
        while events.recv().await.is_some() {}
    })
    .await;
}

async fn settled_report(
    commands: &mpsc::Sender<ClientCommand>,
    events: &mut mpsc::Receiver<AgentEvent>,
    seen: &mut Vec<AgentEvent>,
    asked: &mut u64,
) {
    *asked += 1;
    commands
        .send(report_command(ReportDetail::All, *asked))
        .await
        .unwrap();
    until_event(
        events,
        seen,
        "the settled context report",
        |e| matches!(e, AgentEvent::ContextReport(r) if r.basis.settled()),
    )
    .await;
}

async fn session_bodies(with_reports: bool) -> Vec<Vec<u8>> {
    let provider = Mock::new();
    provider.rule(sizes);
    for _ in 0..10 {
        provider.reply(answer("ok", 1_000, 2_000, 30));
    }
    let mut session = default_session();
    session.message_time = MessageTime::Off;
    session.instructions = Some(KIND.into());
    let mut agent = Agent::new(provider.clone(), session);
    agent.set_toolbox(test_toolbox());
    let (commands, mut events, _task) = spawn(agent);
    let mut seen = vec![];
    let mut asked = 0;
    for n in 0..10 {
        if with_reports {
            settled_report(&commands, &mut events, &mut seen, &mut asked).await;
        }
        commands
            .send(ClientCommand::send(format!("m{n}"), format!("step {n}")))
            .await
            .unwrap();
        until_turn_end(&mut events, &mut seen).await;
    }
    if with_reports {
        settled_report(&commands, &mut events, &mut seen, &mut asked).await;
        assert!(provider.counts.load(Ordering::SeqCst) > 0);
    }
    drop(commands);
    provider.bodies.lock().unwrap().clone()
}

#[tokio::test]
async fn context_reports_leave_every_message_request_byte_identical() {
    let plain = session_bodies(false).await;
    let reported = session_bodies(true).await;
    assert_eq!(plain.len(), 10);
    assert_eq!(reported.len(), plain.len());
    for (i, (a, b)) in plain.iter().zip(&reported).enumerate() {
        assert!(
            a == b,
            "request {i} changed:\n{}\n---\n{}",
            String::from_utf8_lossy(a),
            String::from_utf8_lossy(b)
        );
    }
}

#[tokio::test]
async fn the_part_bodies_match_their_fixtures() {
    let provider = counting();
    provider.knobs(|k| k.threads = true);
    let mut session = default_session();
    session.system = Some("be brief".into());
    session.instructions = Some("be kind".into());
    session.effort = wisp_protocol::Effort::High;
    let mut agent = Agent::new(provider.clone(), session);
    agent.set_toolbox(test_toolbox());
    agent.history = vec![Message::user_text("hi")];
    agent.sync_fill();
    let (tx, mut rx) = mpsc::channel(8);
    agent
        .context_command(ReportDetail::Summary, RequestId(1), &tx)
        .await;
    rx.recv().await.unwrap();
    rx.recv().await.unwrap();
    let seen: Vec<Vec<u8>> = provider.counted().iter().map(Sent::count_body).collect();
    assert_eq!(seen.len(), 5);
    assert!(
        provider
            .counted()
            .iter()
            .all(|c| c.features.thread.is_none())
    );
    let names = ["base", "system", "instructions", "tools"];
    for (name, body) in names.iter().zip(&seen) {
        let text = String::from_utf8(body.clone()).unwrap() + "\n";
        assert!(!text.contains("cache_control"), "{text}");
        assert!(!text.contains("web_search"), "{text}");
        let value: Value = serde_json::from_str(&text).unwrap();
        assert_eq!(
            value["messages"],
            json!([{"role": "user", "content": [{"type": "text", "text": "."}]}])
        );
        assert_eq!(value["model"], DEFAULT_MODEL);
        assert_eq!(value["output_config"], json!({"effort": "high"}));
        assert!(value.get("thinking").is_some());
        let path = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join(format!("tests/fixtures/requests/count_part_{name}.json"));
        if std::env::var_os("WISP_BLESS").is_some() {
            std::fs::write(&path, &text).unwrap();
        }
        assert_eq!(std::fs::read_to_string(&path).unwrap(), text, "{name}");
    }
    let whole: Value = serde_json::from_slice(&seen[4]).unwrap();
    assert_eq!(whole["messages"][0]["content"][0]["text"], "hi");
}

enum Gateway {
    Counts,
    NotFound,
    Slow(Arc<tokio::sync::Semaphore>),
    Fails,
}

fn sse_reply() -> String {
    let events = [
        json!({"type": "message_start", "message": {"id": "msg_1", "type": "message", "role": "assistant", "model": DEFAULT_MODEL, "content": [], "usage": {"input_tokens": 1000, "output_tokens": 1}}}),
        json!({"type": "content_block_start", "index": 0, "content_block": {"type": "text", "text": ""}}),
        json!({"type": "content_block_delta", "index": 0, "delta": {"type": "text_delta", "text": "ok"}}),
        json!({"type": "content_block_stop", "index": 0}),
        json!({"type": "message_delta", "delta": {"stop_reason": "end_turn"}, "usage": {"output_tokens": 5}}),
        json!({"type": "message_stop"}),
    ];
    let body: String = events
        .iter()
        .map(|e| http::event(e["type"].as_str().unwrap(), e))
        .collect();
    http::response("200 OK", &[("content-type", "text/event-stream")], &body)
}

async fn gateway(mode: Gateway) -> (String, Arc<Mutex<Vec<String>>>) {
    let counts = Arc::new(Mutex::new(Vec::new()));
    let seen = counts.clone();
    let mode = Arc::new(mode);
    let base = http::gateway(move |request| {
        let (seen, mode) = (seen.clone(), mode.clone());
        async move {
            let (head, body) = request.split_once("\r\n\r\n").unwrap();
            if !head.starts_with("POST /v1/messages/count_tokens ") {
                return http::Reply::Send(sse_reply());
            }
            seen.lock().unwrap().push(body.to_owned());
            let counted = http::json(
                "200 OK",
                &json!({"input_tokens": 10 + body.len()}).to_string(),
            );
            http::Reply::Send(match mode.as_ref() {
                Gateway::Counts => counted,
                Gateway::NotFound => http::json("404 Not Found", r#"{"error":"nope"}"#),
                Gateway::Fails => http::response(
                    "500 Internal Server Error",
                    &[("content-type", "text/plain")],
                    "x",
                ),
                Gateway::Slow(gate) => {
                    let _ = gate.acquire().await.map(|p| p.forget());
                    counted
                }
            })
        }
    })
    .await;
    (base, counts)
}

async fn through(mode: Gateway) -> (Rig, Arc<Mutex<Vec<String>>>) {
    let (base, counts) = gateway(mode).await;
    let provider = AnthropicProvider::new(&Credential::ApiKey("fake-key".into()), &base).unwrap();
    let mut session = default_session();
    session.message_time = MessageTime::Off;
    let agent = Agent::new(Arc::new(provider), session);
    let (tx, rx) = mpsc::channel(64);
    let mut rig = Rig {
        agent,
        provider: Mock::new(),
        tx: Some(tx),
        rx,
        asked: 0,
    };
    let (_keep, mut commands) = mpsc::channel(8);
    let (events, mut sink) = mpsc::channel(1_024);
    tokio::spawn(async move { while sink.recv().await.is_some() {} });
    rig.agent
        .run_turn("go".into(), &mut commands, &events)
        .await
        .unwrap();
    (rig, counts)
}

#[tokio::test]
async fn a_gateway_that_counts_gives_a_counted_report() {
    let (mut rig, counts) = through(Gateway::Counts).await;
    let reports = rig.report().await;
    assert_eq!(counts.lock().unwrap().len(), 2);
    let last = reports.last().unwrap();
    assert_eq!(last.basis, ReportBasis::Counted);
    let bodies = counts.lock().unwrap().clone();
    let (with, without): (Vec<&String>, Vec<&String>) =
        bodies.iter().partition(|b| b.contains("\"system\""));
    let system = with[0].len() - without[0].len();
    assert_eq!(row(last, Category::SystemPrompt).tokens, system as u64);
    rig.report().await;
    assert_eq!(counts.lock().unwrap().len(), 2);
}

#[tokio::test]
async fn a_gateway_without_count_tokens_falls_back_once_and_stays_quiet() {
    let (mut rig, counts) = through(Gateway::NotFound).await;
    let reports = rig.report().await;
    assert_eq!(
        reports.last().unwrap().basis,
        ReportBasis::Estimated {
            reason: EstimateReason::NoCounter
        }
    );
    let asked = counts.lock().unwrap().len();
    assert!(asked > 0);
    let again = rig.report().await;
    assert_eq!(again.len(), 1);
    assert_eq!(counts.lock().unwrap().len(), asked);
}

#[tokio::test]
async fn a_failing_gateway_is_estimated_with_its_reason_and_asked_again() {
    let (mut rig, counts) = through(Gateway::Fails).await;
    let reports = rig.report().await;
    assert_eq!(
        reports.last().unwrap().basis,
        ReportBasis::Estimated {
            reason: EstimateReason::Failed("HTTP 500".into())
        }
    );
    let asked = counts.lock().unwrap().len();
    rig.report().await;
    assert_eq!(counts.lock().unwrap().len(), 2 * asked);
}

#[tokio::test]
async fn a_slow_gateway_shows_counting_first() {
    let gate = Arc::new(tokio::sync::Semaphore::new(0));
    let (mut rig, counts) = through(Gateway::Slow(gate.clone())).await;
    let first = rig.ask().await;
    assert_eq!(first.basis, ReportBasis::Counting);
    assert!(
        until(|| counts.lock().unwrap().len() >= 2).await,
        "no second count"
    );
    rig.quiet().await;
    gate.add_permits(16);
    let last = rig.next().await;
    assert_eq!(last.answers, first.answers);
    assert_eq!(last.basis, ReportBasis::Counted);
}

#[tokio::test]
async fn the_tools_part_and_the_whole_request_count_the_server_tool() {
    let provider = counting();
    let config = format!(
        "[endpoints.env.models.\"{DEFAULT_MODEL}\"]\nweb_search = [\"web_search_20250305\"]\n"
    );
    let models = crate::context::ModelSettings::new(
        crate::models::ModelTable::shipped().unwrap(),
        crate::models::ConfigModels::parse(&config).unwrap(),
    );
    let mut agent = Agent::new(provider.clone(), default_session())
        .with_model_settings(models)
        .with_web_search(true);
    agent.set_toolbox(test_toolbox().with_web_search(true));
    agent.history = vec![Message::user_text("hi")];
    agent.sync_fill();
    let (tx, mut rx) = mpsc::channel(8);
    agent
        .context_command(ReportDetail::Summary, RequestId(1), &tx)
        .await;
    rx.recv().await.unwrap();
    rx.recv().await.unwrap();
    let seen: Vec<Vec<u8>> = provider.counted().iter().map(Sent::count_body).collect();
    let bodies: Vec<Value> = seen
        .iter()
        .map(|b| serde_json::from_slice(b).unwrap())
        .collect();
    let message_tools = {
        let rendering = agent.rendering(1);
        let request = Request {
            session: agent.session(),
            tools: agent.toolbox().specs(),
            deferred: &[],
            by_value: &[],
            server_tools: agent.server_tools(),
            messages: &agent.history,
            previous_response_id: None,
            features: Features::default(),
            options: Default::default(),
            rendering: &rendering,
            compaction: None,
        };
        body_of(&request)["tools"].clone()
    };
    let entry = json!({"name": "web_search", "type": "web_search_20250305"});
    assert!(message_tools.as_array().unwrap().contains(&entry));
    let with_tools: Vec<&Value> = bodies.iter().filter(|b| b.get("tools").is_some()).collect();
    assert_eq!(with_tools.len(), 2, "the tools part and the whole request");
    for body in with_tools {
        assert_eq!(body["tools"], message_tools);
    }
}

fn png_block() -> ContentBlock {
    ContentBlock::media("image/png".parse().unwrap(), "iVBORw0KGgo=")
}

#[tokio::test]
async fn a_media_count_asks_about_the_block_alone() {
    let mut rig = rig(Some(KIND), true);
    rig.agent.history = vec![Message::user_text("earlier")];
    let answer = rig.agent.media_count().count(png_block()).await;
    let asked = rig.provider.asked();
    assert_eq!(asked.len(), 1);
    assert_eq!(asked[0].model, DEFAULT_MODEL);
    assert_eq!(asked[0].system, None);
    assert_eq!(asked[0].instructions, None);
    assert!(asked[0].tools.is_empty());
    assert_eq!(asked[0].server_tools, 0);
    assert_eq!(
        asked[0].messages,
        [Message {
            role: Role::User,
            content: vec![png_block()],
        }]
    );
    assert_eq!(
        answer,
        TokenCount::Counted {
            tokens: FRAMING + PER_MESSAGE
        }
    );
}

#[tokio::test]
async fn a_media_count_body_has_no_system_tools_or_cache_markers() {
    let provider = counting();
    let mut session = default_session();
    session.system = Some("be brief".into());
    session.instructions = Some("be kind".into());
    let mut agent = Agent::new(provider.clone(), session);
    agent.set_toolbox(test_toolbox());
    agent.media_count().count(png_block()).await;
    let seen: Vec<Vec<u8>> = provider.counted().iter().map(Sent::count_body).collect();
    assert_eq!(seen.len(), 1);
    let body: Value = serde_json::from_slice(&seen[0]).unwrap();
    assert!(body.get("system").is_none(), "{body}");
    assert!(body.get("tools").is_none(), "{body}");
    assert!(!body.to_string().contains("cache_control"), "{body}");
    assert_eq!(
        body["messages"],
        json!([{"role": "user", "content": [{
            "type": "image",
            "source": {"type": "base64", "media_type": "image/png", "data": "iVBORw0KGgo="}
        }]}])
    );
}

#[tokio::test]
async fn an_endpoint_that_can_t_count_is_not_asked_again() {
    let provider = Mock::new();
    let rig = rig_with(None, false, provider);
    let first = rig.agent.media_count().count(png_block()).await;
    assert_eq!(first, TokenCount::Unsupported);
    assert!(rig.agent.uncountable(crate::agent::IMPLICIT_ENDPOINT));
    let second = rig.agent.media_count().count(png_block()).await;
    assert_eq!(second, TokenCount::Unsupported);
    assert_eq!(rig.calls(), 1);
    let off = rig_with(None, false, Mock::new());
    let agent = off.agent.with_count_tokens(false);
    assert_eq!(
        agent.media_count().count(png_block()).await,
        TokenCount::Unsupported
    );
    assert_eq!(off.provider.counts.load(Ordering::SeqCst), 0);
}
