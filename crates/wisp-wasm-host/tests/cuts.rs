#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    reason = "a failed step fails the test"
)]

mod support;

use std::panic::AssertUnwindSafe;
use std::sync::Arc;
use std::time::{Duration, Instant};

use std::sync::atomic::Ordering;

use futures::FutureExt;
use tokio::time::error::Elapsed;
use wisp_plugin_api::Plugin;
use wisp_plugin_api::hooks::{Event, Reply};
use wisp_plugin_api::slots::{Content, SlotView};
use wisp_plugin_api::types::Outcome;
use wisp_plugin_testkit::hooks::{self as kit, before_turn};
use wisp_plugin_testkit::host::{FakeCall, FakeHost};

use support::{Rig, build, kernelled, result, sleeping_host};

#[tokio::test(flavor = "multi_thread")]
async fn a_cut_spin_ends_within_ticks_and_the_next_call_restarts_with_describe() {
    let rig = Rig::new("spin");
    let host = Arc::new(FakeHost::default());
    let plugin = (rig.fixture().make)(host.clone());
    plugin.describe(&build()).await;
    assert_eq!(result(&*plugin, "count", "{}").await, "1");
    let prepared = plugin.prepare("spin", "{}").await.unwrap();
    let started = Instant::now();
    let call = FakeCall::default();
    let cut = tokio::time::timeout(Duration::from_millis(200), plugin.run(prepared, &call)).await;
    assert!(cut.is_err());
    assert!(
        started.elapsed() < Duration::from_millis(400),
        "{:?}",
        started.elapsed()
    );
    tokio::time::sleep(Duration::from_millis(50)).await;
    assert_eq!(
        Arc::strong_count(&host),
        2,
        "the cut store still holds the host"
    );
    assert_eq!(result(&*plugin, "count", "{}").await, "1");
    let who = result(&*plugin, "whoami", "{}").await;
    assert!(who.contains("describe: none"), "{who}");
}

#[tokio::test]
async fn a_cut_drops_an_instance_waiting_in_an_import() {
    let rig = Rig::new("waiting");
    let (host, _) = sleeping_host();
    let plugin = rig.plugin(host);
    assert_eq!(result(&*plugin, "count", "{}").await, "1");
    let prepared = plugin
        .prepare("wait", r#"{"ms":60000,"stubborn":true}"#)
        .await
        .unwrap();
    let call = FakeCall::default();
    let cut = tokio::time::timeout(Duration::from_millis(100), plugin.run(prepared, &call)).await;
    assert!(cut.is_err());
    let started = Instant::now();
    assert_eq!(result(&*plugin, "count", "{}").await, "1");
    assert!(started.elapsed() < Duration::from_millis(500));
}

#[tokio::test(flavor = "multi_thread")]
async fn a_trap_fails_naming_the_plugin_in_one_line_and_restarts() {
    let rig = Rig::new("trap");
    let plugin = rig.plugin(FakeHost::default());
    assert_eq!(result(&*plugin, "count", "{}").await, "1");
    let prepared = plugin.prepare("trap", "{}").await.unwrap();
    let ran = plugin.run(prepared, &FakeCall::default()).await;
    let Outcome::Failed(failure) = ran.outcome else {
        panic!("{:?}", ran.outcome);
    };
    assert_eq!(
        failure.message,
        "the wasm-fixture plugin stopped: wasm trap: wasm `unreachable` instruction executed"
    );
    assert_eq!(result(&*plugin, "count", "{}").await, "1");
}

#[tokio::test]
async fn a_call_prepared_before_a_restart_answers_the_restart_sentence() {
    let rig = Rig::new("stale");
    let plugin = rig.plugin(FakeHost::default());
    let stale = plugin.prepare("echo", r#"{"text":"x"}"#).await.unwrap();
    let trap = plugin.prepare("trap", "{}").await.unwrap();
    plugin.run(trap, &FakeCall::default()).await;
    let ran = plugin.run(stale, &FakeCall::default()).await;
    let Outcome::Failed(failure) = ran.outcome else {
        panic!("{:?}", ran.outcome);
    };
    assert_eq!(
        failure.message,
        "the wasm-fixture plugin restarted before this call ran"
    );
}

#[tokio::test(start_paused = true)]
async fn autonomous_calls_that_never_return_are_cut_after_five_seconds() {
    for entries in [false, true] {
        let rig = Rig::new("stall");
        let host = FakeHost {
            sleep: Some(Arc::new(|_| Box::pin(std::future::pending()))),
            ..FakeHost::default()
        };
        let plugin = rig.plugin(host);
        assert_eq!(result(&*plugin, "count", "{}").await, "1");
        let started = tokio::time::Instant::now();
        let refused = tokio::time::timeout(Duration::from_secs(6), async {
            if entries {
                let stopped = AssertUnwindSafe(plugin.entries())
                    .catch_unwind()
                    .await
                    .unwrap_err();
                *stopped.downcast::<String>().unwrap()
            } else {
                plugin.prepare("stall", "{}").await.err().unwrap()
            }
        })
        .await
        .unwrap();
        assert_eq!(started.elapsed(), Duration::from_secs(5));
        assert_eq!(refused, "the wasm-fixture plugin stopped: cut");
        assert_eq!(result(&*plugin, "count", "{}").await, "1");
    }
}

async fn render(plugin: &dyn Plugin, name: &str) -> Result<Result<Content, String>, Elapsed> {
    let held = plugin.lock_for_render().unwrap();
    let limit = Duration::from_millis(500);
    let rendered = tokio::time::timeout(limit, plugin.render_slot(name, SlotView::Panel)).await;
    drop(held);
    rendered
}

#[tokio::test(flavor = "multi_thread")]
async fn a_cut_render_restarts_the_plugin_and_a_call_that_waited_survives_it() {
    let rig = Rig::new("render-cut");
    let (plugin, kernel) = kernelled(&rig);
    let drawn = |text: &str| Ok(Ok(kit::line(text)));
    assert_eq!(
        render(&*plugin, "loops").await,
        drawn("wasm-fixture loops: panel")
    );
    assert!(render(&*plugin, "loops").await.is_err());
    assert_eq!(
        render(&*plugin, "status").await,
        drawn("wasm-fixture status: panel")
    );
    assert_eq!(kernel.restarts.load(Ordering::SeqCst), 1);
    assert_eq!(
        render(&*plugin, "loops").await,
        drawn("wasm-fixture loops: panel")
    );
    let held = plugin.lock_for_render().unwrap();
    let waiting = plugin.clone();
    let lead = tokio::spawn(async move {
        tokio::time::sleep(Duration::from_millis(100)).await;
        let turn = Event::BeforeTurn(before_turn(1, "hello"));
        waiting.on_hook("lead", turn).await
    });
    let spun = tokio::time::timeout(
        Duration::from_millis(500),
        plugin.render_slot("loops", SlotView::Panel),
    );
    assert!(spun.await.is_err());
    assert!(!lead.is_finished());
    drop(held);
    let lead = tokio::time::timeout(Duration::from_secs(10), lead).await;
    let set_up = "wasm-fixture set up (resumed: true), 0 agents spawned\n".to_owned();
    assert_eq!(lead.unwrap().unwrap(), Reply::Append(vec![set_up]));
    assert_eq!(kernel.restarts.load(Ordering::SeqCst), 2);
}
