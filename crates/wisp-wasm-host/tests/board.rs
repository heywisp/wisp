#![allow(
    clippy::unwrap_used,
    clippy::panic,
    reason = "a failed step fails the test"
)]

mod support;

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use wisp_plugin_api::Plugin;
use wisp_plugin_api::tools::{Effect, ToolKind};
use wisp_plugin_api::types::{Action, Completed, Failure, Outcome, ToolRun};
use wisp_plugin_testkit::host::{FakeCall, FakeHost};

use support::{Rig, board_host, build};

async fn call(plugin: &dyn Plugin, tool: &str, input: &str) -> (ToolRun, Vec<u8>) {
    let prepared = plugin.prepare(tool, input).await.unwrap();
    let ctx = FakeCall::default();
    let run = plugin.run(prepared, &ctx).await;
    (run, ctx.written())
}

fn completed(is_error: bool) -> Outcome {
    Outcome::Completed(Completed {
        is_error,
        exit: None,
    })
}

#[tokio::test]
async fn the_board_component_serves_its_tools_through_the_host() {
    let rig = Rig::new("board");
    rig.fixture();
    let found = rig.board();
    assert_eq!(rig.installed().plugins.len(), 2);
    let plugin = (found.make)(Arc::new(board_host(&rig)));
    assert_eq!(
        plugin
            .render_slot("unused", wisp_plugin_api::slots::SlotView::Panel)
            .await,
        Ok(wisp_plugin_api::slots::Content::default())
    );
    assert_eq!(
        plugin
            .on_hook("unused", wisp_plugin_api::hooks::Event::SessionEnd)
            .await,
        wisp_plugin_api::hooks::Reply::None
    );
    assert_eq!(
        plugin.run_command("unused", "").await,
        wisp_plugin_api::commands::CommandReply::None
    );
    assert!(plugin.entries().await.is_empty());
    assert_eq!(
        plugin.open_entry("missing").await,
        Err("entry not found".to_owned())
    );
    let defs = plugin.describe(&build()).await;
    let metas: Vec<(&str, Effect, bool, bool, bool, u64)> = defs
        .iter()
        .map(|def| match &def.kind {
            ToolKind::Client(tool) => (
                def.name.as_str(),
                tool.meta.effect,
                tool.meta.parallel_safe,
                tool.meta.saves_output,
                tool.meta.needs_workspace,
                tool.meta.timeouts.default_ms,
            ),
            ToolKind::Native(_) => panic!("{}", def.name),
        })
        .collect();
    assert_eq!(
        metas,
        [
            ("board_read", Effect::ReadOnly, true, true, false, 10_000),
            ("board_write", Effect::LocalTxn, false, false, false, 10_000)
        ]
    );
    for tool in ["board_read", "board_write"] {
        assert_eq!(plugin.time_limit_ms(tool, "{}").await, Some(10_000));
    }
    let (created, _) = call(
        &*plugin,
        "board_write",
        r#"{"board":"phase2","cards":[{"id":"a","title":"A"}]}"#,
    )
    .await;
    assert_eq!(created.outcome, completed(false));
    assert_eq!(
        created.result.as_deref(),
        Some("board phase2: created, 1 card added")
    );
    let (read, written) = call(&*plugin, "board_read", r#"{"board":"phase2"}"#).await;
    assert_eq!((read.outcome, read.result), (completed(false), None));
    assert_eq!(
        String::from_utf8(written).unwrap(),
        "board phase2 · owner main · edit owner\ntodo (1)\n  a · A\ndoing (0)\ndone (0)\n"
    );
    let (refused, _) = call(
        &*plugin,
        "board_write",
        r#"{"board":"phase2","remove":["z"]}"#,
    )
    .await;
    assert_eq!(refused.outcome, completed(true));
    assert_eq!(
        refused.result.as_deref(),
        Some("board phase2 has no card z")
    );
    let alone = r#"{"board":"phase2","delete":true,"owner":"main"}"#;
    assert_eq!(
        plugin.prepare("board_write", alone).await.err().as_deref(),
        Some("delete and owner each go alone in a write")
    );
    let kept = plugin.rejected_display("board_write", alone).await.unwrap();
    assert_eq!(
        (kept.action, kept.target.as_str()),
        (Action::Write, "board phase2")
    );
    let delete = plugin
        .prepare("board_write", r#"{"board":"phase2","delete":true}"#)
        .await
        .unwrap();
    let verbs = delete.display().verbs.unwrap();
    assert_eq!(
        (verbs.done.as_str(), verbs.failed.as_str()),
        ("Deleted", "couldn't delete")
    );
    let alone_host = (found.make)(Arc::new(FakeHost::default()));
    let (failed, _) = call(&*alone_host, "board_read", "{}").await;
    assert_eq!(
        failed.outcome,
        Outcome::Failed(Failure {
            message: "the board needs this session's agent tree".to_owned(),
            before_start: false,
        })
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_read_never_fails_while_another_agent_writes_the_board() {
    let rig = Rig::new("board-race");
    let found = rig.board();
    let writer = (found.make)(Arc::new(board_host(&rig)));
    let reader = (found.make)(Arc::new(board_host(&rig)));
    let titled = |n: u32| format!(r#"{{"board":"phase2","cards":[{{"id":"a","title":"A{n}"}}]}}"#);
    let (created, _) = call(&*writer, "board_write", &titled(0)).await;
    assert_eq!(created.outcome, completed(false));
    let stop = Arc::new(AtomicBool::new(false));
    let writing = stop.clone();
    let writes = tokio::spawn(async move {
        let mut n = 0;
        while !writing.load(Ordering::SeqCst) {
            n += 1;
            let (written, _) = call(&*writer, "board_write", &titled(n)).await;
            assert_eq!(written.outcome, completed(false));
        }
    });
    for _ in 0..150 {
        for input in [r#"{"board":"phase2"}"#, "{}"] {
            let (read, _) = call(&*reader, "board_read", input).await;
            assert_eq!(
                (read.outcome, read.result),
                (completed(false), None),
                "{input}"
            );
        }
    }
    stop.store(true, Ordering::SeqCst);
    writes.await.unwrap();
}
