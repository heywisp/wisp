use std::collections::BTreeSet;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;

use tokio::sync::Semaphore;
use wisp_plugin_api::tree::{StopInfo, TreeEvent};
use wisp_plugin_testkit::hooks::{self, Calls, Render, Watch};
use wisp_protocol::AgentEvent;

use super::*;
use crate::agents::news::{StopNews, TreeNews};
use crate::plugin::slots::SlotLane;
use crate::plugin::testing::hook_body;
use crate::plugin::{KernelHost, Make, PluginConfig, Registry};
use crate::test_guard::{mail, path, state};

struct Lanes {
    engine: Arc<Engine>,
    news: Arc<News>,
    tree: TreeLane,
    slots: SlotLane,
}

async fn lanes(sources: Vec<(String, Make)>) -> Lanes {
    let (registry, problems) = Registry::from_sources(sources, &PluginConfig::default());
    assert!(problems.is_empty(), "{problems:?}");
    let started = registry
        .start(&BTreeSet::new(), Arc::new(KernelHost::new()))
        .await;
    let slots = SlotLane::start(
        &started.hooks.sections,
        &started.hooks.plugins,
        BTreeSet::new(),
    );
    let (engine, _finished) = Engine::new(
        started.hooks.table,
        started.hooks.plugins,
        Some(started.host),
    );
    engine.set_lane(slots.signals().clone());
    let news = Arc::new(News::default());
    let tree = TreeLane::start(engine.clone(), news.clone());
    Lanes {
        engine,
        news,
        tree,
        slots,
    }
}

async fn lane(body: &str, watch: Watch, calls: &Calls) -> (Arc<Engine>, Arc<News>, TreeLane) {
    let make: Make = hooks::observing(hooks::none(), watch, calls.clone());
    let lanes = lanes(vec![(hooks::manifest_in("0.2.0", "aa", body), make)]).await;
    (lanes.engine, lanes.news, lanes.tree)
}

fn gated(gate: &Arc<Semaphore>, on: &'static str) -> Watch {
    let gate = gate.clone();
    Arc::new(move |_, hook, _| {
        let gate = (hook == on).then(|| gate.clone());
        Box::pin(async move {
            if let Some(gate) = gate {
                let _ = gate.acquire().await.map(|p| p.forget());
            }
        })
    })
}

fn ended(agent: &str) -> TreeNews {
    TreeNews::Ended(StopNews {
        agent: path(agent),
        target: path(agent),
        by: None,
    })
}

async fn heard(calls: &Calls, count: usize) -> Vec<TreeEvent> {
    for _ in 0..1000 {
        let seen = calls.tree();
        if seen.len() >= count {
            return seen.into_iter().map(|s| s.event).collect();
        }
        tokio::task::yield_now().await;
    }
    panic!("heard only {:#?}", calls.tree());
}

#[tokio::test]
async fn a_newer_state_waiting_behind_a_held_hook_replaces_the_older_and_the_mail_stays_first() {
    let calls = Calls::default();
    let gate = Arc::new(Semaphore::new(0));
    let body = hook_body("ends", "agent-ended", "")
        + &hook_body("states", "agent-state", "")
        + &hook_body("mails", "agent-mail", "");
    let (_engine, news, lane) = lane(&body, gated(&gate, "ends"), &calls).await;
    news.push(ended("/z"));
    heard(&calls, 1).await;
    news.push(state("/a", 1.0));
    news.push(mail("/b", "/c"));
    news.push(state("/a", 2.0));
    gate.add_permits(1);
    let seen = heard(&calls, 3).await;
    tokio::time::sleep(Duration::from_millis(50)).await;
    assert_eq!(calls.tree().len(), 3, "{:#?}", calls.tree());
    assert_eq!(
        seen,
        [
            TreeEvent::AgentEnded(StopInfo {
                agent: "/z".to_owned(),
                target: "/z".to_owned(),
                by: None,
            }),
            crate::plugin::convert::tree_event(mail("/b", "/c")),
            crate::plugin::convert::tree_event(state("/a", 2.0)),
        ]
    );
    drop(lane);
}

#[tokio::test]
async fn a_blocked_background_hook_delays_no_other_point() {
    let calls = Calls::default();
    let gate = Arc::new(Semaphore::new(0));
    let body = hook_body(
        "ends",
        "agent-ended",
        "background = true\ntimeout_ms = 600000\n",
    ) + &hook_body("states", "agent-state", "");
    let (engine, news, lane) = lane(&body, gated(&gate, "ends"), &calls).await;
    news.push(ended("/z"));
    news.push(state("/a", 1.0));
    let seen = heard(&calls, 2).await;
    assert_eq!(
        seen[1],
        crate::plugin::convert::tree_event(state("/a", 1.0))
    );
    assert!(engine.background_running(None));
    drop(lane);
    drop(engine.abort_background(None));
}

#[tokio::test]
async fn a_section_its_point_marks_renders_again_once_the_points_hooks_have_run() {
    let calls = Calls::default();
    let gate = Arc::new(Semaphore::new(0));
    let heard = Arc::new(AtomicUsize::new(0));
    let (held, told) = (gate.clone(), heard.clone());
    let watch: Watch = Arc::new(move |_, _, _| {
        let (gate, heard) = (held.clone(), told.clone());
        Box::pin(async move {
            let _ = gate.acquire().await.map(|p| p.forget());
            heard.fetch_add(1, Ordering::SeqCst);
        })
    });
    let render: Render = Arc::new(move |_, _, _| {
        let text = format!("heard {}", heard.load(Ordering::SeqCst));
        Box::pin(async move { Ok(hooks::line(&text)) })
    });
    let section = hooks::section("sec", "marks = [\"agent-mail\"]\n");
    let lanes = lanes(vec![
        (
            hooks::slotted("aa", &section),
            hooks::sections(hooks::none(), render, calls.clone()),
        ),
        (
            hooks::manifest_in("0.2.0", "bb", &hook_body("mails", "agent-mail", "")),
            hooks::observing(hooks::none(), watch, calls.clone()),
        ),
    ])
    .await;
    let (events, mut shown) = tokio::sync::mpsc::channel(16);
    lanes
        .slots
        .show(vec!["aa/sec".to_owned()], Vec::new(), &events);
    let text = |event: Option<AgentEvent>| serde_json::to_string(&event).unwrap();
    assert!(text(shown.recv().await).contains("heard 0"));
    lanes.news.push(mail("/b", "/c"));
    let rendered = async {
        while calls.renders().len() < 2 {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    };
    assert!(
        tokio::time::timeout(Duration::from_secs(2), rendered)
            .await
            .is_ok()
    );
    gate.add_permits(1);
    let again = tokio::time::timeout(Duration::from_secs(2), shown.recv()).await;
    assert!(text(again.unwrap()).contains("heard 1"));
}

#[tokio::test]
async fn after_a_set_is_replaced_the_lane_reaches_the_joined_plugin_and_never_the_removed_one() {
    let calls = Calls::default();
    let gate = Arc::new(Semaphore::new(0));
    let source = |id: &str, hook: &str, watch: Watch| -> (String, Make) {
        let make: Make = hooks::observing(hooks::none(), watch, calls.clone());
        let body = hook_body(hook, "agent-ended", "");
        (hooks::manifest_in("0.2.0", id, &body), make)
    };
    let quiet = || -> Watch { Arc::new(|_, _, _| Box::pin(async {})) };
    let Lanes {
        engine, news, tree, ..
    } = lanes(vec![
        source("aa", "a-end", gated(&gate, "a-end")),
        source("bb", "b-end", quiet()),
    ])
    .await;
    news.push(ended("/z"));
    heard(&calls, 1).await;
    let (registry, _) = Registry::from_sources(
        vec![
            source("aa", "a-end", quiet()),
            source("cc", "c-end", quiet()),
        ],
        &PluginConfig::default(),
    );
    let started = registry
        .start(&BTreeSet::new(), Arc::new(KernelHost::new()))
        .await;
    engine.replace(started.hooks.table, started.hooks.plugins);
    gate.add_permits(1);
    news.push(ended("/y"));
    heard(&calls, 3).await;
    tokio::time::sleep(Duration::from_millis(50)).await;
    let hooks: Vec<String> = calls.tree().into_iter().map(|s| s.hook).collect();
    assert_eq!(hooks, ["a-end", "a-end", "c-end"]);
    drop(tree);
}
