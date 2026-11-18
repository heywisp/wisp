#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    reason = "a failed step fails the test"
)]

mod support;

use std::sync::Arc;
use std::time::{Duration, Instant};

use futures::FutureExt;
use tokio::sync::Notify;

use wisp_plugin_api::Plugin;
use wisp_plugin_api::types::Outcome;
use wisp_plugin_testkit::host::{FakeCall, FakeHost};

use support::{LABEL, Rig, build, result, sleeping_host};

#[tokio::test]
async fn echo_runs_as_the_testkits_bundled_echo_does() {
    let rig = Rig::new("echo");
    let wasm = rig.plugin(FakeHost::default());
    let bundled = (wisp_plugin_testkit::bundled().instantiate)(Arc::new(FakeHost::default()));
    let input = r#"{"text":"hello\nworld"}"#;
    let mut runs = Vec::new();
    for plugin in [&wasm, &bundled] {
        let prepared = plugin.prepare("echo", input).await.unwrap();
        let display = prepared.display();
        let call = FakeCall::default();
        let run = plugin.run(prepared, &call).await;
        runs.push((display, run, call.written()));
    }
    assert_eq!(runs[0], runs[1]);
    assert_eq!(runs[0].2, b"hello\nworld");
}

#[tokio::test]
async fn files_writes_and_reads_back_under_the_calls_folder() {
    let rig = Rig::new("files");
    let plugin = rig.plugin(FakeHost::default());
    let prepared = plugin
        .prepare("files", r#"{"name":"note.txt","text":"kept"}"#)
        .await
        .unwrap();
    let call = FakeCall {
        cwd: rig.scratch.root.display().to_string(),
        ..FakeCall::default()
    };
    let run = plugin.run(prepared, &call).await;
    assert_eq!(run.result.as_deref(), Some("kept"));
    let on_disk = std::fs::read_to_string(rig.scratch.root.join("note.txt")).unwrap();
    assert_eq!(on_disk, "kept");
}

#[tokio::test(start_paused = true)]
async fn streamed_chunks_reach_the_output_as_they_are_written() {
    let rig = Rig::new("stream");
    let (host, _) = sleeping_host();
    let plugin = rig.plugin(host);
    let prepared = plugin
        .prepare("stream", r#"{"chunks":3,"ms":1000}"#)
        .await
        .unwrap();
    let call = FakeCall::default();
    let run = plugin.run(prepared, &call);
    let mut run = std::pin::pin!(run);
    let early = tokio::time::timeout(Duration::from_millis(1500), &mut run).await;
    assert!(early.is_err());
    assert_eq!(call.written(), b"chunk 1\nchunk 2\n");
    run.await;
    assert_eq!(call.written(), b"chunk 1\nchunk 2\nchunk 3\n");
}

#[tokio::test]
async fn a_refused_prepare_is_the_guests_message() {
    let rig = Rig::new("refused");
    let plugin = rig.plugin(FakeHost::default());
    let refused = plugin.prepare("echo", "not json").await.err().unwrap();
    assert!(refused.starts_with("input is not JSON"), "{refused}");
    let unknown = plugin.prepare("nope", "{}").await.err().unwrap();
    assert_eq!(unknown, "there is no tool named `nope`");
}

#[tokio::test]
async fn the_per_call_exports_answer_through_the_instance() {
    let rig = Rig::new("exports");
    let plugin = rig.plugin(FakeHost::default());
    let input = r#"{"ms":1,"limit_ms":250}"#;
    let shown = plugin.rejected_display("wait", input).await.unwrap();
    assert_eq!(shown.target, "rejected");
    assert_eq!(plugin.time_limit_ms("wait", input).await, Some(250));
    assert!(plugin.movable("wait", input).await);
    assert!(!plugin.movable("echo", input).await);
    let interrupted = plugin.interrupted_result("wait", input).await;
    assert_eq!(interrupted.as_deref(), Some("the wait was interrupted"));
    assert_eq!(plugin.time_limit_ms("echo", input).await, None);
}

#[tokio::test]
async fn two_waits_overlap_and_a_describe_answers_while_they_run() {
    let rig = Rig::new("overlap");
    let (host, _) = sleeping_host();
    let plugin = rig.plugin(host);
    let wait = || async {
        let prepared = plugin.prepare("wait", r#"{"ms":600}"#).await.unwrap();
        plugin.run(prepared, &FakeCall::default()).await
    };
    let started = Instant::now();
    let described = async {
        tokio::time::sleep(Duration::from_millis(100)).await;
        let defs = plugin.describe(&build()).await;
        (defs.len(), started.elapsed())
    };
    let (first, second, (tools, at)) = tokio::join!(wait(), wait(), described);
    assert_eq!(first.result.as_deref(), Some("waited"));
    assert_eq!(second.result.as_deref(), Some("waited"));
    assert!(
        started.elapsed() < Duration::from_millis(1100),
        "{:?}",
        started.elapsed()
    );
    assert_eq!(tools, 11);
    assert!(at < Duration::from_millis(500), "{at:?}");
}

#[tokio::test]
async fn overlapping_calls_keep_their_own_context_and_reply() {
    let rig = Rig::new("own");
    let (host, _) = sleeping_host();
    let plugin = rig.plugin(host);
    let echo = |text: &'static str| {
        let plugin = &plugin;
        async move {
            let input = format!(r#"{{"text":"{text}"}}"#);
            let prepared = plugin.prepare("echo", &input).await.unwrap();
            let call = FakeCall::default();
            let ran = plugin.run(prepared, &call).await;
            (ran, call.written())
        }
    };
    let ((_, a), (_, b)) = tokio::join!(echo("first"), echo("second"));
    assert_eq!(
        (a.as_slice(), b.as_slice()),
        (&b"first"[..], &b"second"[..])
    );
}

#[tokio::test]
async fn each_import_runs_under_the_caller_of_its_call() {
    let rig = Rig::new("scope");
    let (host, slept) = sleeping_host();
    let plugin = rig.plugin(host);
    let wait = |label: &'static str, ms: u64| {
        let plugin = &plugin;
        LABEL.scope(label.to_owned(), async move {
            let input = format!(r#"{{"ms":{ms}}}"#);
            let prepared = plugin.prepare("wait", &input).await.unwrap();
            plugin.run(prepared, &FakeCall::default()).await
        })
    };
    tokio::join!(wait("a", 300), wait("b", 200));
    let mut seen = slept.lock().unwrap().clone();
    seen.sort();
    assert_eq!(
        seen,
        [(Some("a".to_owned()), 300), (Some("b".to_owned()), 200)]
    );
}

#[tokio::test]
async fn a_call_without_a_caller_sees_none() {
    let rig = Rig::new("unscoped");
    let (host, slept) = sleeping_host();
    let plugin = rig.plugin(host);
    let prepared = plugin.prepare("wait", r#"{"ms":5}"#).await.unwrap();
    plugin.run(prepared, &FakeCall::default()).await;
    assert_eq!(slept.lock().unwrap().as_slice(), [(None, 5)]);
}

#[tokio::test]
async fn the_guest_reads_the_calls_cancel_id_and_folder() {
    let rig = Rig::new("context");
    let (host, _) = sleeping_host();
    let plugin = rig.plugin(host);
    let call = FakeCall {
        id: "call-7".to_owned(),
        cwd: "/work".to_owned(),
        ..FakeCall::default()
    };
    let prepared = plugin.prepare("whoami", "{}").await.unwrap();
    let ran = plugin.run(prepared, &call).await.result.unwrap();
    assert!(ran.contains("call: call-7\ncwd: /work"), "{ran}");
    let prepared = plugin.prepare("wait", r#"{"ms":60000}"#).await.unwrap();
    let cancel = call.cancel.clone();
    let (ran, ()) = tokio::join!(plugin.run(prepared, &call), async move {
        tokio::time::sleep(Duration::from_millis(100)).await;
        cancel.set();
    });
    assert_eq!(ran.outcome, Outcome::Cancelled);
    assert_eq!(ran.result.as_deref(), Some("saw the call cancelled"));
}

#[tokio::test]
async fn a_jobs_spawn_reaches_the_hosts_jobs_through_the_call() {
    let rig = Rig::new("spawn");
    let plugin = rig.plugin(FakeHost::default());
    let ran = result(&*plugin, "spawn", r#"{"program":"true"}"#).await;
    assert!(ran.contains("the fake host runs no jobs"), "{ran}");
}

#[tokio::test]
async fn each_agent_gets_its_own_instance() {
    let rig = Rig::new("agents");
    let found = rig.fixture();
    let first: Arc<dyn Plugin> = (found.make)(Arc::new(FakeHost::default()));
    let second: Arc<dyn Plugin> = (found.make)(Arc::new(FakeHost::default()));
    assert_eq!(result(&*first, "count", "{}").await, "1");
    assert_eq!(result(&*first, "count", "{}").await, "2");
    assert_eq!(result(&*second, "count", "{}").await, "1");
}

#[tokio::test(flavor = "multi_thread")]
async fn the_render_lock_waits_for_running_calls_and_calls_that_wait_go_first() {
    let rig = Rig::new("render-lock");
    let (entered, release) = (Arc::new(Notify::new()), Arc::new(Notify::new()));
    let (entering, releasing) = (entered.clone(), release.clone());
    let host = FakeHost {
        sleep: Some(Arc::new(move |_| {
            entering.notify_one();
            let releasing = releasing.clone();
            Box::pin(async move { releasing.notified().await })
        })),
        ..sleeping_host().0
    };
    let plugin = rig.plugin(host);
    let wait = plugin.prepare("wait", r#"{"ms":1000}"#).await.unwrap();
    assert!(plugin.lock_for_render().is_some());
    let running = plugin.clone();
    let ran = tokio::spawn(async move { running.run(wait, &FakeCall::default()).await });
    entered.notified().await;
    assert!(plugin.lock_for_render().is_none());
    release.notify_one();
    assert_eq!(ran.await.unwrap().result.as_deref(), Some("waited"));
    let count = plugin.prepare("count", "{}").await.unwrap();
    let call = FakeCall::default();
    let held = plugin.lock_for_render().unwrap();
    let mut prepare = plugin.prepare("echo", r#"{"text":"x"}"#);
    let mut run = plugin.run(count, &call);
    let mut limit = plugin.time_limit_ms("wait", r#"{"limit_ms":7}"#);
    assert!((&mut prepare).now_or_never().is_none());
    assert!((&mut run).now_or_never().is_none());
    assert!((&mut limit).now_or_never().is_none());
    drop(held);
    assert!(plugin.lock_for_render().is_none());
    let (prepared, ran, limited) = tokio::join!(prepare, run, limit);
    assert_eq!(prepared.unwrap().display().target, "x");
    assert_eq!(ran.result.as_deref(), Some("1"));
    assert_eq!(limited, Some(7));
    assert!(plugin.lock_for_render().is_some());
}

#[tokio::test]
async fn probe_shows_a_code_view_and_reaches_the_four_newer_imports() {
    use wisp_plugin_api::host::{ScreenReport, SecretEnd, WarmRow};
    use wisp_plugin_api::index::{Entry, Kind};
    use wisp_plugin_api::slots::NodeState;
    use wisp_plugin_api::tree::ShownState;
    use wisp_plugin_api::types::{CodeView, Fact, Hit, View};
    let rig = Rig::new("probe");
    let entry = Entry {
        kind: Kind::Skill,
        name: "host-note".to_owned(),
        summary: "Placeholder summary".to_owned(),
        text: "Placeholder search text".to_owned(),
    };
    let host = FakeHost {
        states: vec![ShownState {
            path: "/child".to_owned(),
            state: NodeState::Waiting,
            keep_warm: Some(WarmRow {
                refreshes: 1,
                limit: 2,
                paused: false,
            }),
        }],
        screen_report: Some(ScreenReport {
            report: "Placeholder report".to_owned(),
            images: Vec::new(),
        }),
        secret_end: Some(SecretEnd::Typed),
        entries: vec![entry],
        ..FakeHost::default()
    };
    let expected = concat!(
        "states: [ShownState { path: \"/child\", state: NodeState::Waiting, keep-warm: Some(WarmRow { refreshes: 1, limit: 2, paused: false }) }]\n",
        "report: Some(ScreenReport { report: \"Placeholder report\", images: [] })\n",
        "secret: SecretEnd::Typed\n",
        "rename: Ok(Renamed { from: \"/old\", to: \"/new\" })\n",
        "entries: [Entry { kind: Kind::Skill, name: \"host-note\", summary: \"Placeholder summary\", text: \"Placeholder search text\" }]",
    );
    let asked = host.secret_asked.clone();
    let renamed = host.renamed.clone();
    let plugin = rig.plugin(host);
    let prepared = plugin
        .prepare("probe", r#"{"code":"placeholder\n"}"#)
        .await
        .unwrap();
    assert_eq!(
        prepared.display().view,
        Some(View::Code(CodeView {
            language: Some("text".to_owned()),
            code: "placeholder\n".to_owned()
        }))
    );
    let call = FakeCall {
        id: "probe-call".to_owned(),
        ..FakeCall::default()
    };
    let run = plugin.run(prepared, &call).await;
    assert_eq!(run.result.as_deref(), Some(expected));
    assert_eq!(
        run.facts,
        [Fact::Hits(vec![Hit {
            kind: Kind::Skill,
            name: "host-note".to_owned()
        }])]
    );
    {
        let asks = asked.lock().unwrap();
        assert_eq!(asks.len(), 1);
        assert_eq!(
            (&asks[0].0, asks[0].1.process, asks[0].1.name.as_str()),
            (&call.id, 7, "Placeholder")
        );
    }
    assert_eq!(
        *renamed.lock().unwrap(),
        [(call.id, "old".to_owned(), "new".to_owned())]
    );
    assert_eq!(
        plugin.entries().await,
        [Entry {
            kind: Kind::Document,
            name: "fixture-note".to_owned(),
            summary: "Placeholder entry".to_owned(),
            text: "Placeholder search text".to_owned()
        }]
    );
    assert_eq!(
        plugin.open_entry("fixture-note").await,
        Ok("Placeholder entry body".to_owned())
    );
    assert_eq!(
        plugin.open_entry("missing").await,
        Err("nothing named `missing` was found".to_owned())
    );
}

#[tokio::test]
async fn the_map_section_renders_a_topology_block() {
    use wisp_plugin_api::slots::{Block, Content, NodeState, SlotView, Topology, TopologyNode};
    let rig = Rig::new("map");
    let plugin = rig.plugin(FakeHost::default());
    assert_eq!(
        plugin.render_slot("map", SlotView::Panel).await,
        Ok(Content {
            note: Vec::new(),
            blocks: vec![Block::Topology(Topology {
                root: Some("main".to_owned()),
                nodes: vec![TopologyNode {
                    key: "main".to_owned(),
                    parent: None,
                    state: NodeState::Working
                }],
                flows: Vec::new(),
                counted: None,
            })],
        })
    );
}
