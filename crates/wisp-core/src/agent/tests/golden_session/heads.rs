use super::*;

const WISP: &str = "[models.\"claude-opus-5-5\"]\nnative_compaction = false\n";
#[cfg(unix)]
const WISP_SHORT_VERBATIM: &str =
    "[models.\"claude-opus-5-5\"]\nnative_compaction = false\nverbatim_user_tokens = 10\n";
#[cfg(unix)]
const BAND: &str = "[models.\"claude-opus-5-5\"]\ncompact_at = 20000\n";
#[cfg(unix)]
const NEAR: u64 = 19_500;
#[cfg(unix)]
const PAST_POINT: u64 = 20_500;

#[cfg(unix)]
fn background(id: &'static str) -> Part {
    call(
        id,
        "bash",
        json!({"command": "sleep 60", "run_in_background": true}),
    )
}

#[cfg(unix)]
fn in_project(models: &'static str) -> Setup {
    Setup {
        tools: Tools::Jobs,
        start: Some("sub"),
        instructions: true,
        models: Some(models),
        ..Setup::default()
    }
}

#[cfg(unix)]
fn banded() -> Setup {
    Setup {
        background_tokens: Some(1_000),
        ..in_project(BAND)
    }
}

#[cfg(unix)]
fn assert_every_request_tracked(agent: &Agent, rig: &Rig) {
    let sent = u64::try_from(rig.server.seen().bodies.len()).unwrap();
    assert!(sent >= 5, "{sent}");
    assert_eq!(agent.warmth_table().requests(), sent);
    assert!(
        agent
            .warmth_table()
            .heats()
            .iter()
            .all(|h| *h == crate::warmth::Heat::Warm)
    );
}

#[cfg(unix)]
fn swapped(seen: &[AgentEvent]) -> usize {
    seen.iter()
        .filter(|e| {
            matches!(
                e,
                AgentEvent::Compacted {
                    trigger: wisp_protocol::CompactionTrigger::Background,
                    ..
                }
            )
        })
        .count()
}

#[cfg(unix)]
#[tokio::test]
async fn wisps_head_with_tools_holds_the_environment_ledger_and_cut_message() {
    let script = vec![
        calls("msg_01", 400, vec![background("toolu_01")]),
        calls(
            "msg_02",
            520,
            vec![call("toolu_02", "view_file", json!({"path": "red.png"}))],
        ),
        says("msg_03", 40_000, "Both done."),
        summary("msg_04", 40_100),
        says("msg_05", 760, "Still running."),
    ];
    let rig = Rig::new(
        "compact_head_tools",
        in_project(WISP_SHORT_VERBATIM),
        script,
    )
    .await;
    rig.project();
    fs::write(rig.start_dir().join("red.png"), crate::tool::snap::png()).unwrap();
    let mut live = rig.start();
    live.completed(
        "u1",
        "Start a job in the background—then show me the picture in red.png.",
    )
    .await;
    live.compacted(1).await;
    live.completed("u2", "Is it still running?").await;
    live.send(ClientCommand::StopProcess { id: 1 }).await;
    let (agent, _) = live.close().await;
    assert_every_request_tracked(&agent, &rig);
    rig.assert_rebuilds(&agent);
    rig.check();
}

#[cfg(unix)]
#[tokio::test]
async fn an_overflow_after_a_cut_off_reply_keeps_the_replys_end_in_the_head() {
    let setup = Setup {
        tools: Tools::Jobs,
        models: Some(WISP),
        ..Setup::default()
    };
    let script = vec![
        calls("msg_01", 400, vec![background("toolu_01")]),
        says("msg_02", 50_000, "One."),
        says("msg_03", 100_000, "Two."),
        reply(
            "msg_04",
            160_000,
            vec![
                Part::Text("The end of a long thought"),
                call("toolu_02", "bash", json!({"command": "ls"})),
            ],
            "model_context_window_exceeded",
        ),
        too_long(),
        summary("msg_05", 100_000),
        says("msg_06", 900, "Done."),
    ];
    let rig = Rig::new("compact_overflow_cut_off", setup, script).await;
    let mut live = rig.start();
    live.completed("u1", "first").await;
    live.completed("u2", "second").await;
    live.completed("u3", "third").await;
    live.send(ClientCommand::StopProcess { id: 1 }).await;
    let (agent, _) = live.close().await;
    rig.assert_rebuilds(&agent);
    rig.check();
}

#[cfg(unix)]
#[tokio::test]
async fn a_native_summary_swapped_in_between_turns() {
    let script = vec![
        calls("msg_01", 400, vec![background("toolu_01")]),
        says("msg_02", NEAR, "Started."),
        reply(
            "msg_03",
            1_200,
            vec![Part::Compaction("So far: a job started.")],
            "compaction",
        ),
        says("msg_04", 300, "Again."),
        says("msg_05", 500, "More."),
    ];
    let rig = Rig::new("compact_swap_between_turns", banded(), script).await;
    rig.project();
    let mut live = rig.start();
    live.completed("u1", "Start a job.").await;
    live.until(prepared).await;
    live.completed("u2", "second").await;
    live.completed("u3", "third").await;
    live.send(ClientCommand::StopProcess { id: 1 }).await;
    let (agent, seen) = live.close().await;
    assert_eq!(swapped(&seen), 1, "{seen:#?}");
    assert_every_request_tracked(&agent, &rig);
    rig.assert_rebuilds(&agent);
    rig.check();
}

#[cfg(unix)]
#[tokio::test]
async fn a_native_summary_swapped_in_within_a_turn() {
    let precompute = Arc::new(tokio::sync::Semaphore::new(0));
    let script = vec![
        calls("msg_01", 400, vec![background("toolu_01")]),
        says("msg_02", NEAR, "Started."),
        reply(
            "msg_03",
            1_200,
            vec![Part::Compaction("So far: a job started.")],
            "compaction",
        )
        .gated(&precompute),
        calls(
            "msg_04",
            PAST_POINT,
            vec![call("toolu_02", "bash", json!({"command": "echo hi"}))],
        ),
        says("msg_05", 400, "Done."),
        says("msg_06", 500, "More."),
    ];
    let rig = Rig::new("compact_swap_in_turn", banded(), script).await;
    rig.project();
    let mut live = rig.start();
    live.completed("u1", "Start a job.").await;
    rig.server.wait_bodies(3).await;
    live.send(ClientCommand::send("u2", "Say hi.")).await;
    live.until(|e| matches!(e, AgentEvent::ToolCallFinished { .. }))
        .await;
    precompute.add_permits(1);
    let outcome = live.ended().await;
    assert!(completed(&outcome), "{outcome:?}");
    live.completed("u3", "third").await;
    live.send(ClientCommand::StopProcess { id: 1 }).await;
    let (agent, seen) = live.close().await;
    assert_eq!(swapped(&seen), 1, "{seen:#?}");
    rig.assert_rebuilds(&agent);
    rig.check();
}

async fn pointed(rig: &Rig) {
    let mut live = rig.start();
    live.completed("u1", "Look around.").await;
    live.compacted(1).await;
    live.completed("u2", "Look again.").await;
    let (agent, _) = live.close().await;
    rig.assert_rebuilds(&agent);
    rig.check();
}

fn pointers_script() -> Vec<Reply> {
    vec![
        says("msg_01", 40_000, "Nothing to see."),
        summary("msg_02", 40_100),
        says("msg_03", 520, "Still nothing."),
    ]
}

#[cfg(unix)]
#[tokio::test]
async fn wisps_head_with_tools_points_at_the_log_and_the_outputs() {
    let setup = Setup {
        tools: Tools::Default,
        models: Some(WISP),
        located: true,
        ..Setup::default()
    };
    let rig = Rig::new("compact_pointers", setup, pointers_script()).await;
    pointed(&rig).await;
}

#[cfg(unix)]
#[tokio::test]
async fn without_the_compaction_plugin_the_head_holds_only_the_summary() {
    let setup = Setup {
        tools: Tools::Default,
        models: Some(WISP),
        located: true,
        compaction: false,
        ..Setup::default()
    };
    let rig = Rig::new("compact_fallback", setup, pointers_script()).await;
    pointed(&rig).await;
    let bodies = rig.server.seen().bodies;
    let asked = &bodies[1];
    assert!(
        asked.contains("Write a summary of the conversation so far, so that the work can continue from the summary alone. Don't call tools.\\n"),
        "{asked}"
    );
    assert!(!asked.contains("six sections"), "{asked}");
    let after = &bodies[2];
    assert!(
        after.contains("<compacted_context epoch=\\\"1\\\""),
        "{after}"
    );
    for gone in [
        "<user_messages>",
        "<tool_calls>",
        "<pointers>",
        "Running processes",
    ] {
        assert!(!after.contains(gone), "{gone}: {after}");
    }
    let failed: Vec<String> = pick!(
        &log_events(&rig.store),
        LogEvent::HookRan {
            hook,
            outcome: wisp_protocol::HookOutcome::Failed { message },
            ..
        } => format!("{hook}: {message}")
    );
    assert!(
        failed
            .iter()
            .all(|f| f.starts_with("compaction/strategy: the compaction plugin isn't installed")),
        "{failed:?}"
    );
    assert!(!failed.is_empty());
}

#[tokio::test]
async fn wisps_head_in_a_chat_points_at_the_log_by_name() {
    let setup = Setup {
        models: Some(WISP),
        located: true,
        ..Setup::default()
    };
    let rig = Rig::new("compact_pointers_chat", setup, pointers_script()).await;
    pointed(&rig).await;
}
