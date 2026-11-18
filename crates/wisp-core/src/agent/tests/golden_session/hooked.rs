use wisp_plugin_api::hooks::{Event, Reply as HookReply};
use wisp_protocol::HookChange;
use wisp_protocol::log::LogEvent;

use super::*;

const TYPED: &str = "say hi";
const SENT: &str = "Say hi, in one short line.";

fn source(body: &str, script: wisp_plugin_testkit::hooks::Script) -> (String, crate::plugin::Make) {
    let make: crate::plugin::Make = wisp_plugin_testkit::hooks::instantiate(
        script,
        wisp_plugin_testkit::hooks::Calls::default(),
    );
    (wisp_plugin_testkit::hooks::manifest("aa", body), make)
}

fn hook(name: &str, point: &str) -> String {
    format!("\n[[hooks]]\nname = \"{name}\"\npoint = \"{point}\"\n")
}

fn answer(event: &Event) -> HookReply {
    match event {
        Event::UserInput(input) if input.text == TYPED => HookReply::Rewrite(SENT.into()),
        Event::BeforeEnd(end) if end.continues == 0 && end.reply == "Hi." => {
            HookReply::Continue("Check once more.".into())
        }
        Event::AfterTool(_) => HookReply::Rewrite("hooked result".into()),
        Event::BeforeTool(_) => HookReply::Block("not today".into()),
        _ => HookReply::None,
    }
}

fn scripted(points: &[&str]) -> (String, crate::plugin::Make) {
    let body: String = points
        .iter()
        .enumerate()
        .map(|(n, point)| hook(&format!("h{n}"), point))
        .collect();
    source(
        &body,
        Arc::new(|_, _, event| {
            let reply = answer(&event);
            Box::pin(async move { reply })
        }),
    )
}

fn rewriting() -> (String, crate::plugin::Make) {
    scripted(&["user-input"])
}

fn continuing() -> (String, crate::plugin::Make) {
    scripted(&["before-end"])
}

fn rewriting_and_continuing() -> (String, crate::plugin::Make) {
    scripted(&["user-input", "before-end"])
}

fn result_rewriting() -> (String, crate::plugin::Make) {
    scripted(&["after-tool"])
}

fn blocking() -> (String, crate::plugin::Make) {
    scripted(&["before-tool"])
}

fn hooked(hooks: fn() -> (String, crate::plugin::Make)) -> Setup {
    Setup {
        hooks: Some(hooks),
        ..Setup::default()
    }
}

fn tooled(hooks: fn() -> (String, crate::plugin::Make)) -> Setup {
    Setup {
        tools: Tools::Default,
        ..hooked(hooks)
    }
}

#[tokio::test]
async fn a_rewritten_input_is_sent_and_logged_whole() {
    let script = vec![says("msg_01", 400, "Hi."), says("msg_02", 520, "Again.")];
    let rig = Rig::new("hook_input_rewrite", hooked(rewriting), script).await;
    let mut live = rig.start();
    live.completed("u1", TYPED).await;
    live.completed("u2", "and again").await;
    let (agent, _) = live.close().await;
    rig.assert_rebuilds(&agent);
    let events = log_events(&rig.store);
    let texts: Vec<(String, String)> = pick!(
        &events,
        LogEvent::HookApplied {
            change: HookChange::Text { before, after },
            input_id: Some(_),
            ..
        } => (before.clone(), after.clone())
    );
    assert_eq!(texts, vec![(TYPED.to_owned(), SENT.to_owned())]);
    assert!(events.iter().any(|e| matches!(
        e,
        LogEvent::TurnStarted { turn: 1, text, .. } if text.contains(SENT) && !text.contains(TYPED)
    )));
    assert_extends(rig.case, &rig.server.seen().bodies);
    rig.check();
}

#[tokio::test]
async fn a_continued_turn_sends_the_hook_text_as_a_user_message() {
    let script = vec![
        says("msg_01", 400, "Hi."),
        says("msg_02", 520, "Checked."),
        says("msg_03", 640, "Again."),
    ];
    let rig = Rig::new("hook_continue", hooked(continuing), script).await;
    let mut live = rig.start();
    live.completed("u1", "hello").await;
    live.completed("u2", "and again").await;
    let (agent, seen) = live.close().await;
    assert_eq!(
        seen.iter()
            .filter(|e| matches!(e, AgentEvent::TurnEnded { .. }))
            .count(),
        2
    );
    rig.assert_rebuilds(&agent);
    assert_extends(rig.case, &rig.server.seen().bodies);
    rig.check();
}

#[tokio::test]
async fn a_rewritten_result_reaches_the_model_and_keeps_the_prefix() {
    let script = vec![
        calls(
            "msg_01",
            400,
            vec![call("toolu_01", "bash", json!({"command": "echo hi"}))],
        ),
        says("msg_02", 520, "Done."),
        says("msg_03", 640, "Again."),
    ];
    let rig = Rig::new("hook_result_rewrite", tooled(result_rewriting), script).await;
    let mut live = rig.start();
    live.completed("u1", "run it").await;
    live.completed("u2", "and again").await;
    let (agent, _) = live.close().await;
    rig.assert_rebuilds(&agent);
    let bodies = rig.server.seen().bodies;
    assert!(bodies[1].contains("hooked result"), "{}", bodies[1]);
    assert_extends(rig.case, &bodies);
    rig.check();
}

#[tokio::test]
async fn a_blocked_call_answers_with_the_reason_and_keeps_the_prefix() {
    let script = vec![
        calls(
            "msg_01",
            400,
            vec![call("toolu_01", "bash", json!({"command": "echo hi"}))],
        ),
        says("msg_02", 520, "Fine."),
        says("msg_03", 640, "Again."),
    ];
    let rig = Rig::new("hook_tool_block", tooled(blocking), script).await;
    let mut live = rig.start();
    live.completed("u1", "run it").await;
    live.completed("u2", "and again").await;
    let (agent, _) = live.close().await;
    rig.assert_rebuilds(&agent);
    let bodies = rig.server.seen().bodies;
    assert!(
        bodies[1].contains("Blocked by aa/h0: not today"),
        "{}",
        bodies[1]
    );
    assert_extends(rig.case, &bodies);
    rig.check();
}

#[tokio::test]
async fn a_resume_after_a_rewrite_and_a_continue_resends_the_live_bytes() {
    let script = vec![
        says("msg_01", 400, "Hi."),
        says("msg_02", 520, "Checked."),
        says("msg_03", 640, "Again."),
    ];
    let rig = Rig::new("hook_resume", hooked(rewriting_and_continuing), script).await;
    let mut live = rig.start();
    live.completed("u1", TYPED).await;
    let (before, _) = live.close().await;
    rig.assert_rebuilds(&before);
    let mut back = rig.resume();
    back.completed("u2", "and again").await;
    let (after, _) = back.close().await;
    rig.assert_rebuilds(&after);
    let bodies = rig.server.seen().bodies;
    assert_eq!(bodies.len(), 3);
    assert!(bodies[0].contains(SENT) && !bodies[0].contains(TYPED));
    assert_extends(rig.case, &bodies);
    rig.check();
}

const LOW_NATIVE: &str = "[models.\"claude-opus-5-5\"]\ncompact_at = 2000\n";
const LOW_WISP: &str =
    "[models.\"claude-opus-5-5\"]\ncompact_at = 2000\nnative_compaction = false\n";

fn compact_answer(event: &Event) -> HookReply {
    match event {
        Event::BeforeCompact(_) => HookReply::Block("not now".into()),
        _ => HookReply::None,
    }
}

fn vetoing() -> (String, crate::plugin::Make) {
    source(
        &hook("h0", "before-compact"),
        Arc::new(|_, _, event| {
            let reply = compact_answer(&event);
            Box::pin(async move { reply })
        }),
    )
}

fn heading() -> (String, crate::plugin::Make) {
    source(
        &hook("h0", "compaction-head"),
        Arc::new(|_, _, event| {
            let reply = match event {
                Event::CompactionHead(_) => {
                    HookReply::Append(vec!["Keep the plan in view.".into()])
                }
                _ => HookReply::None,
            };
            Box::pin(async move { reply })
        }),
    )
}

fn asking() -> (String, crate::plugin::Make) {
    source(
        &hook("h0", "context-threshold"),
        Arc::new(|_, _, event| {
            let reply = match event {
                Event::ContextThreshold(_) => HookReply::Compact(Some("Keep the plan.".into())),
                _ => HookReply::None,
            };
            Box::pin(async move { reply })
        }),
    )
}

fn instructing() -> (String, crate::plugin::Make) {
    source(
        &(hook("h0", "before-compact") + &hook("h1", "before-compact")),
        Arc::new(|_, hook, event| {
            let reply = match (event, hook.as_str()) {
                (Event::BeforeCompact(_), "h0") => HookReply::Rewrite("Summarise briefly.".into()),
                (Event::BeforeCompact(_), _) => HookReply::Append(vec!["Keep the plan.".into()]),
                _ => HookReply::None,
            };
            Box::pin(async move { reply })
        }),
    )
}

fn compacting(config: &'static str, hooks: fn() -> (String, crate::plugin::Make)) -> Setup {
    Setup {
        models: Some(config),
        ..hooked(hooks)
    }
}

fn native_summary(id: &str, input: u64) -> Reply {
    reply(
        id,
        input,
        vec![Part::Compaction("So far: a greeting.")],
        "compaction",
    )
}

fn failures(rig: &Rig) -> usize {
    records(&rig.store)
        .iter()
        .filter(|r| matches!(&r.event, LogEvent::CompactionFailed { .. }))
        .count()
}

fn compactions(rig: &Rig) -> usize {
    records(&rig.store)
        .iter()
        .filter(|r| matches!(&r.event, LogEvent::ContextCompacted { .. }))
        .count()
}

#[tokio::test]
async fn a_vetoed_automatic_compaction_sends_the_turn_as_it_stands() {
    let script = vec![
        says("msg_01", 5_000, "Hello."),
        says("msg_02", 5_200, "Again."),
    ];
    let rig = Rig::new("hook_compact_veto", compacting(LOW_NATIVE, vetoing), script).await;
    let mut live = rig.start();
    live.completed("u1", "hello").await;
    live.completed("u2", "again").await;
    let (agent, _) = live.close().await;
    rig.assert_rebuilds(&agent);
    assert_eq!((failures(&rig), compactions(&rig)), (1, 0));
    assert_extends(rig.case, &rig.server.seen().bodies);
    rig.check();
}

#[tokio::test]
async fn a_hook_asks_for_a_compaction_far_below_the_point() {
    let script = vec![
        says("msg_01", 1_000, "Hello."),
        native_summary("msg_02", 1_000),
        says("msg_03", 600, "Again."),
    ];
    let rig = Rig::new("hook_compact_early", hooked(asking), script).await;
    let mut live = rig.start();
    live.completed("u1", "hello").await;
    live.completed("u2", "again").await;
    let (agent, _) = live.close().await;
    rig.assert_rebuilds(&agent);
    assert_eq!((failures(&rig), compactions(&rig)), (0, 1));
    let requested: Vec<Option<String>> = pick!(
        log_events(&rig.store),
        LogEvent::ContextCompacted { requested_by, .. } => requested_by
    );
    assert_eq!(requested.len(), 1);
    assert!(requested[0].as_deref().is_some_and(|h| h.ends_with("/h0")));
    let bodies = rig.server.seen().bodies;
    assert!(bodies[1].contains("Focus: Keep the plan."), "{}", bodies[1]);
    rig.check();
}

#[tokio::test]
async fn a_veto_at_the_ceiling_is_rejected_and_the_compaction_runs() {
    let script = vec![
        says("msg_01", 900_000, "Hello."),
        native_summary("msg_02", 900_000),
        says("msg_03", 600, "Again."),
    ];
    let rig = Rig::new(
        "hook_compact_veto_ceiling",
        compacting(LOW_NATIVE, vetoing),
        script,
    )
    .await;
    let mut live = rig.start();
    live.completed("u1", "hello").await;
    live.completed("u2", "again").await;
    let (agent, _) = live.close().await;
    rig.assert_rebuilds(&agent);
    assert_eq!((failures(&rig), compactions(&rig)), (0, 1));
    rig.check();
}

#[tokio::test]
async fn a_head_addition_follows_the_compacted_context_and_keeps_the_prefix() {
    let script = vec![
        says("msg_01", 5_000, "Hello."),
        native_summary("msg_02", 5_000),
        says("msg_03", 600, "Again."),
        says("msg_04", 700, "More."),
    ];
    let rig = Rig::new(
        "hook_head_addition",
        compacting(LOW_NATIVE, heading),
        script,
    )
    .await;
    let mut live = rig.start();
    live.completed("u1", "hello").await;
    live.completed("u2", "again").await;
    live.completed("u3", "more").await;
    let (agent, _) = live.close().await;
    rig.assert_rebuilds(&agent);
    let bodies = rig.server.seen().bodies;
    assert!(
        bodies[2].contains("Keep the plan in view."),
        "{}",
        bodies[2]
    );
    assert!(super::binding::extends(&bodies[2], &bodies[3]));
    rig.check();
}

#[tokio::test]
async fn a_rewritten_instruction_reaches_wisps_summariser() {
    let script = vec![
        says("msg_01", 5_000, "Hello."),
        greeted("msg_02", 5_000),
        says("msg_03", 600, "Again."),
    ];
    let rig = Rig::new(
        "hook_instruction_rewrite_wisp",
        compacting(LOW_WISP, instructing),
        script,
    )
    .await;
    let mut live = rig.start();
    live.completed("u1", "hello").await;
    live.completed("u2", "again").await;
    let (agent, _) = live.close().await;
    rig.assert_rebuilds(&agent);
    let bodies = rig.server.seen().bodies;
    assert!(
        bodies[1].contains("Summarise briefly.\\nKeep the plan."),
        "{}",
        bodies[1]
    );
    rig.check();
}

#[tokio::test]
async fn a_rewritten_instruction_reaches_the_native_summariser() {
    let script = vec![
        says("msg_01", 5_000, "Hello."),
        native_summary("msg_02", 5_000),
        says("msg_03", 600, "Again."),
    ];
    let rig = Rig::new(
        "hook_instruction_rewrite_native",
        compacting(LOW_NATIVE, instructing),
        script,
    )
    .await;
    let mut live = rig.start();
    live.completed("u1", "hello").await;
    live.completed("u2", "again").await;
    let (agent, _) = live.close().await;
    rig.assert_rebuilds(&agent);
    let bodies = rig.server.seen().bodies;
    assert!(
        bodies[1].contains("Summarise briefly.\\nKeep the plan."),
        "{}",
        bodies[1]
    );
    rig.check();
}

const LAYOUT: &str = "testkit_layout";

fn laid_out(
    called: wisp_plugin_testkit::hooks::StrategyCalled,
) -> wisp_plugin_testkit::hooks::Answer {
    use wisp_plugin_testkit::hooks::{Answer, StrategyCalled};
    match called {
        StrategyCalled::Instruction(_) => Answer::Instruction(Some(
            "Summarise the conversation in a few lines.".to_owned(),
        )),
        StrategyCalled::Accept(..) => Answer::Accept(Ok(())),
        StrategyCalled::Head(info, viewed, summary) => Answer::Head(vec![format!(
            "<{LAYOUT} epoch=\"{}\">\n{}\nlog: {}\noutputs: {}\n</{LAYOUT}>\n",
            info.epoch,
            summary.unwrap_or_default(),
            viewed.log_pointer.unwrap_or_else(|| "none".to_owned()),
            viewed.output_pointer.unwrap_or_else(|| "none".to_owned()),
        )]),
    }
}

fn strategy_source() -> (String, crate::plugin::Make) {
    use wisp_plugin_testkit::hooks as kit;
    let make: crate::plugin::Make = kit::strategising(
        kit::none(),
        None,
        Arc::new(|_, called| {
            let answer = laid_out(called);
            Box::pin(async move { answer })
        }),
        kit::Calls::default(),
    );
    (kit::manifest_in("0.2.0", "aa", &kit::strategy_body()), make)
}

fn strategised(tools: Tools) -> Setup {
    Setup {
        tools,
        models: Some(LOW_WISP_POINTERS),
        located: true,
        hooks: Some(strategy_source),
        strategy: Some("aa"),
        ..Setup::default()
    }
}

const LOW_WISP_POINTERS: &str = "[models.\"claude-opus-5-5\"]\nnative_compaction = false\n";

fn layout_lines(body: &str) -> (String, String) {
    let value: Value = serde_json::from_str(body).unwrap();
    let text = value["messages"]
        .as_array()
        .unwrap()
        .iter()
        .flat_map(|m| m["content"].as_array().cloned().unwrap_or_default())
        .filter_map(|b| b["text"].as_str().map(str::to_owned))
        .find(|t| t.starts_with(&format!("<{LAYOUT}")))
        .unwrap();
    let line = |key: &str| {
        text.lines()
            .find_map(|l| l.strip_prefix(&format!("{key}: ")))
            .unwrap()
            .to_owned()
    };
    (line("log"), line("outputs"))
}

fn fixture_text(case: &str, n: usize) -> String {
    let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join(FIXTURES)
        .join(case)
        .join(format!("{n:02}.json"));
    let value: Value = serde_json::from_str(&fs::read_to_string(path).unwrap()).unwrap();
    value.to_string()
}

fn strategy_script() -> Vec<Reply> {
    vec![
        says("msg_01", 40_000, "Nothing to see."),
        greeted("msg_02", 40_100),
        says("msg_03", 520, "Still nothing."),
        says("msg_04", 640, "And again."),
    ]
}

#[tokio::test]
async fn a_testkit_strategy_lays_out_the_head_and_the_next_request_extends_it() {
    let rig = Rig::new(
        "compact_testkit_strategy",
        strategised(Tools::Default),
        strategy_script(),
    )
    .await;
    let mut live = rig.start();
    live.completed("u1", "Look around.").await;
    live.compacted(1).await;
    live.completed("u2", "Look again.").await;
    live.completed("u3", "Once more.").await;
    let (agent, _) = live.close().await;
    rig.assert_rebuilds(&agent);
    let bodies = rig.server.seen().bodies;
    assert!(
        bodies[1].contains("Summarise the conversation in a few lines."),
        "{}",
        bodies[1]
    );
    assert!(!bodies[2].contains("<compacted_context"), "{}", bodies[2]);
    let (log, outputs) = layout_lines(&bodies[2]);
    let shown = fixture_text("compact_pointers", 3);
    assert!(
        shown.contains(&format!(
            "The full earlier conversation can be read in the session log, {log}, and every tool output under {outputs}."
        )),
        "{log} {outputs}"
    );
    assert!(super::binding::extends(&bodies[2], &bodies[3]));
    let built: Vec<Option<String>> = pick!(
        &log_events(&rig.store),
        LogEvent::ContextCompacted { strategy, .. } => strategy.clone()
    );
    assert_eq!(built, [Some("aa".to_owned())]);
    rig.check();
}

#[tokio::test]
async fn a_strategy_in_a_chat_sees_the_log_pointer_the_chat_head_shows() {
    let rig = Rig::new(
        "compact_pointers_chat",
        strategised(Tools::Chat),
        strategy_script().into_iter().take(3).collect(),
    )
    .await;
    let mut live = rig.start();
    live.completed("u1", "Look around.").await;
    live.compacted(1).await;
    live.completed("u2", "Look again.").await;
    let (agent, _) = live.close().await;
    rig.assert_rebuilds(&agent);
    let bodies = rig.server.seen().bodies;
    let (log, outputs) = layout_lines(&bodies[2]);
    assert_eq!(outputs, "none");
    let shown = fixture_text("compact_pointers_chat", 3);
    assert!(
        shown.contains(&format!(
            "The full earlier conversation can be read in the session log, {log}."
        )),
        "{log}"
    );
}
