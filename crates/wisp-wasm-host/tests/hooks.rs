#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    reason = "a failed step fails the test"
)]

mod support;

use std::panic::AssertUnwindSafe;
use std::sync::Arc;
use std::sync::atomic::Ordering;
use std::time::Duration;

use futures::FutureExt;
use wisp_plugin_api::Plugin;
use wisp_plugin_api::commands::{CommandLine, CommandReply, CommandRow};
use wisp_plugin_api::hooks::{CompactInfo, Event, Reply, SessionStart, ToolCall, ToolResult};
use wisp_plugin_api::prompt::{At, Prior};
use wisp_plugin_api::slots::{SlotView, TextSpan, Tone};
use wisp_plugin_api::strategy::{CallEntry, SaidEntry, StrategyInfo, View};
use wisp_plugin_testkit::hooks::{self as kit, before_turn, user_input};
use wisp_plugin_testkit::host::{FakeCall, FakeHost};

use support::{Rig, SETUP, kernelled, result, sleeping_host};

const TRAPPED: &str =
    "the wasm-fixture plugin stopped: wasm trap: wasm `unreachable` instruction executed";

fn started(resumed: bool) -> Event {
    Event::SessionStart(SessionStart { resumed })
}

fn turn() -> Event {
    Event::BeforeTurn(before_turn(1, "hello"))
}

fn line(state: &str) -> Reply {
    Reply::Append(vec![format!("wasm-fixture {state}, 0 agents spawned\n")])
}

fn before_tool(input: &str) -> Event {
    Event::BeforeTool(ToolCall {
        call_id: "toolu_1".to_owned(),
        tool: "echo".to_owned(),
        input: input.to_owned(),
    })
}

struct Said(Vec<SaidEntry>);

impl View for Said {
    fn said(&self) -> Vec<SaidEntry> {
        self.0.clone()
    }

    fn calls(&self) -> Vec<CallEntry> {
        Vec::new()
    }

    fn log_pointer(&self) -> Option<String> {
        None
    }

    fn output_pointer(&self) -> Option<String> {
        None
    }
}

fn info() -> StrategyInfo {
    StrategyInfo {
        compact: CompactInfo {
            trigger: "manual".to_owned(),
            engine: "wisp".to_owned(),
            turn: Some(2),
            focus: None,
            estimate: 100,
            point: None,
            epoch: 0,
            requested_by: None,
            instruction: String::new(),
        },
        epoch: 1,
        first_turn: 1,
        last_turn: 2,
        native: false,
        cut_off: None,
        verbatim_tokens: 0,
    }
}

async fn trapped(plugin: &dyn Plugin) {
    let unwound = AssertUnwindSafe(plugin.on_hook("traps", user_input("trap", 0)))
        .catch_unwind()
        .await
        .err()
        .unwrap();
    assert_eq!(unwound.downcast_ref::<String>().unwrap(), TRAPPED);
}

#[tokio::test]
async fn hooks_commands_sections_and_strategy_answer_through_the_instance() {
    let rig = Rig::new("exports");
    let plugin = rig.plugin(FakeHost::default());
    assert_eq!(plugin.on_hook("lead", turn()).await, line("not set up"));
    assert_eq!(plugin.on_hook(SETUP, started(false)).await, Reply::None);
    assert_eq!(
        plugin.on_hook("lead", turn()).await,
        line("set up (resumed: false)")
    );
    let blocked = plugin
        .on_hook("block", before_tool(r#"{"text":"block me"}"#))
        .await;
    assert_eq!(
        blocked,
        Reply::Block("the fixture blocks this input".to_owned())
    );
    let ran = Event::AfterTool(ToolResult {
        call_id: "toolu_1".to_owned(),
        tool: "echo".to_owned(),
        input: "{}".to_owned(),
        result: "Output:\nrewrite me".to_owned(),
        is_error: false,
    });
    assert_eq!(
        plugin.on_hook("rewrite", ran).await,
        Reply::Rewrite("rewritten by the fixture".to_owned())
    );
    assert_eq!(
        plugin.run_command("send", "hello there").await,
        CommandReply::Send("hello there".to_owned())
    );
    let span = TextSpan {
        text: "shown".to_owned(),
        tone: Tone::Accent,
        bold: true,
    };
    assert_eq!(
        plugin.run_command("lines", "shown").await,
        CommandReply::Lines(vec![
            CommandLine::Row(CommandRow {
                left: vec![span.clone()],
                right: vec![span.clone()]
            }),
            CommandLine::Text(vec![span]),
        ])
    );
    let rendered = plugin
        .render_section("clock", At::Start, Prior::None, None)
        .await;
    assert_eq!(rendered.text.as_deref(), Some("wasm-fixture clock: 0"));
    assert_eq!(
        plugin.render_slot("status", SlotView::Page).await,
        Ok(kit::line("wasm-fixture status: page"))
    );
    assert!(plugin.instruction(info()).await.is_some());
    assert_eq!(plugin.accept(info(), "kept".to_owned()).await, Ok(()));
    let entry = |turn| SaidEntry {
        turn,
        text: "hi".to_owned(),
        seq: None,
    };
    let view = Said(vec![entry(1), entry(2)]);
    let head = plugin
        .head(info(), &view, Some("the summary".to_owned()))
        .await;
    assert_eq!(
        head,
        ["The fixture's summary of 2 messages:\nthe summary".to_owned()]
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn a_trapped_hook_unwinds_and_the_restart_runs_session_start_before_the_next_call() {
    let rig = Rig::new("hook-trap");
    let (plugin, kernel) = kernelled(&rig);
    plugin.on_hook(SETUP, started(false)).await;
    trapped(&*plugin).await;
    let lead = tokio::time::timeout(Duration::from_secs(10), plugin.on_hook("lead", turn()));
    assert_eq!(lead.await, Ok(line("set up (resumed: true)")));
    assert_eq!(kernel.restarts.load(Ordering::SeqCst), 1);
    assert_eq!(kernel.relayed.load(Ordering::SeqCst), 2);
}

#[tokio::test(flavor = "multi_thread")]
async fn calls_wait_while_a_restart_sets_up_and_its_own_hooks_get_through() {
    let rig = Rig::new("hold");
    let (plugin, kernel) = kernelled(&rig);
    trapped(&*plugin).await;
    let held = kernel.gate.write().await;
    let waiting = plugin.clone();
    let first = tokio::spawn(async move { waiting.on_hook("lead", turn()).await });
    let relayed = async {
        while kernel.relayed.load(Ordering::SeqCst) < 2 {
            tokio::task::yield_now().await;
        }
    };
    tokio::time::timeout(Duration::from_secs(10), relayed)
        .await
        .unwrap();
    let counting = plugin.clone();
    let mut second = tokio::spawn(async move { result(&*counting, "count", "{}").await });
    let held_back = tokio::time::timeout(Duration::from_millis(300), &mut second).await;
    assert!(held_back.is_err() && !first.is_finished());
    assert_eq!(kernel.restarts.load(Ordering::SeqCst), 1);
    drop(held);
    assert_eq!(first.await.unwrap(), line("set up (resumed: true)"));
    assert_eq!(second.await.unwrap(), "1");
}

#[tokio::test(start_paused = true)]
async fn an_unbounded_call_counts_its_five_seconds_from_the_call_not_the_restart() {
    let rig = Rig::new("setup-limit");
    let (plugin, kernel) = kernelled(&rig);
    trapped(&*plugin).await;
    let held = kernel.gate.write().await;
    let started = tokio::time::Instant::now();
    let limit = tokio::time::timeout(
        Duration::from_secs(10),
        plugin.time_limit_ms("wait", r#"{"limit_ms":7}"#),
    )
    .await;
    assert_eq!(limit, Ok(None));
    assert_eq!(started.elapsed(), Duration::from_secs(5));
    drop(held);
    assert_eq!(
        plugin.time_limit_ms("wait", r#"{"limit_ms":7}"#).await,
        Some(7)
    );
    assert_eq!(kernel.restarts.load(Ordering::SeqCst), 2);
}

#[tokio::test]
async fn a_hook_answers_while_a_tool_run_waits_in_the_same_instance() {
    let rig = Rig::new("hook-overlap");
    let (host, _) = sleeping_host();
    let plugin: Arc<dyn Plugin> = rig.plugin(host);
    let prepared = plugin.prepare("wait", r#"{"ms":60000}"#).await.unwrap();
    let call = FakeCall::default();
    let mut waiting = std::pin::pin!(plugin.run(prepared, &call));
    let blocked = tokio::select! {
        biased;
        _ = &mut waiting => panic!("the wait ended"),
        reply = tokio::time::timeout(
            Duration::from_secs(2),
            plugin.on_hook("block", before_tool(r#"{"text":"block me"}"#)),
        ) => reply,
    };
    assert_eq!(
        blocked,
        Ok(Reply::Block("the fixture blocks this input".to_owned()))
    );
}
