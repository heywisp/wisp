use std::collections::BTreeSet;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;

use jiff::Timestamp;
use tokio::sync::{Semaphore, mpsc};
use wisp_plugin_api::hooks::{Event, Reply};
use wisp_plugin_api::usage::UsageState;
use wisp_plugin_testkit::hooks::{self, Calls, Render, Script};
use wisp_protocol::RequestId;
use wisp_protocol::plan_usage::{Percent, PlanUsage, UsageScope, UsageSource, UsageWindow};

use super::*;
use crate::agent::testing::{Fake, Knobs};
use crate::clock::ShiftedClock;
use crate::plugin::host::{Snapshot, UsageAccount};
use crate::plugin::slots::SlotLane;
use crate::plugin::{PluginConfig, Registry};
use crate::test_guard::within;

const NOW: i64 = 1_800_000_000;

struct Lanes {
    cache: PlanUsageCache,
    generation: crate::plan_usage::Generation,
    host: Arc<KernelHost>,
    slots: SlotLane,
    _usage: UsageLane,
    _finished: crate::plugin::hooks::Finished,
}

async fn lanes(script: Script, render: Render, calls: &Calls) -> Lanes {
    let now = NOW;
    let cache = PlanUsageCache::default();
    let generation = cache
        .bind("account", Fake::with(Knobs::default(), []))
        .unwrap();
    let host = Arc::new(KernelHost::new());
    host.publish(Snapshot {
        clock: Arc::new(ShiftedClock::at(Timestamp::from_second(now).unwrap(), None)),
        usage_accounts: vec![UsageAccount {
            name: "account".into(),
            yours: true,
            off: false,
            expired: false,
            unavailable: false,
        }],
        usage_cache: cache.clone(),
        ..Snapshot::default()
    });
    let body = "[capabilities]\nrequested = [\"usage\"]\n".to_owned()
        + &hooks::section("limits", "marks = [\"usage-changed\"]\n")
        + &crate::plugin::testing::hook_body("changed", USAGE_CHANGED, "");
    let source = (
        hooks::manifest_in("0.4.0", "context", &body),
        hooks::sections(script, render, calls.clone()),
    );
    let (registry, problems) = Registry::from_sources(vec![source], &PluginConfig::default());
    assert!(problems.is_empty(), "{problems:?}");
    let started = registry.start(&BTreeSet::new(), host.clone()).await;
    let slots = SlotLane::start(
        &started.hooks.sections,
        &started.hooks.plugins,
        BTreeSet::new(),
    );
    let (engine, finished) = Engine::new(started.hooks.table, started.hooks.plugins, Some(host));
    engine.set_lane(slots.signals().clone());
    let usage = UsageLane::start(engine, started.host.clone(), &cache);
    Lanes {
        cache,
        generation,
        host: started.host,
        slots,
        _usage: usage,
        _finished: finished,
    }
}

#[tokio::test]
async fn usage_hooks_and_renders_read_live_rows_and_interest_changes() {
    let calls = Calls::default();
    let (sent, mut heard) = mpsc::unbounded_channel();
    let script: Script = Arc::new(move |host, _, event| {
        sent.send((event, host.usage().accounts())).unwrap();
        Box::pin(async { Reply::None })
    });
    let render: Render = Arc::new(|host, _, _| {
        let rows = format!("{:?}", host.usage().accounts());
        Box::pin(async move { Ok(hooks::line(&rows)) })
    });
    let lanes = lanes(script, render, &calls).await;
    let (cache, generation) = (&lanes.cache, lanes.generation);
    let now = NOW;
    let (events, mut shown) = mpsc::channel(8);
    lanes
        .slots
        .show(vec!["context/limits".into()], Vec::new(), &events);
    within("initial usage render", shown.recv()).await.unwrap();
    let reading = |observed_at| {
        PlanUsage::new(
            None,
            vec![UsageWindow {
                key: "five_hour".into(),
                label: "5-hour".into(),
                duration_seconds: Some(18_000),
                scope: UsageScope::All,
                used_percent: Some(Percent::try_from(43.0).unwrap()),
                resets_at: Some(now + 3600),
                observed_at,
                source: UsageSource::UsageEndpoint,
            }],
        )
        .unwrap()
    };
    cache.observe("account", generation, Ok(reading(now)), now);
    let (event, rows) = within("usage hook", heard.recv()).await.unwrap();
    assert!(
        matches!(event, Event::Custom(custom) if custom.point == USAGE_CHANGED && custom.payload.is_empty())
    );
    assert_eq!(rows[0].windows[0].used_percent, Some(43.0));
    assert_eq!(rows[0].state, wisp_plugin_api::usage::UsageState::Ok);
    let rendered = within("changed usage render", shown.recv()).await.unwrap();
    assert!(serde_json::to_string(&rendered).unwrap().contains("43.0"));
    assert_eq!(calls.renders().len(), 2);
    cache.observe("account", generation, Ok(reading(now + 1)), now + 1);
    assert!(
        tokio::time::timeout(Duration::from_millis(350), heard.recv())
            .await
            .is_err()
    );
    assert!(shown.try_recv().is_err());
    assert_eq!(calls.count("changed"), 1);
    assert_eq!(calls.renders().len(), 2);
    cache.hold("section");
    let generation = cache.rebind("account", Fake::with(Knobs::default(), []));
    let (_, rows) = within("checking after replacement", heard.recv())
        .await
        .unwrap();
    assert_eq!(rows[0].state, UsageState::Checking);
    cache.unhold("section");
    assert_eq!(
        lanes.host.usage_accounts()[0].state,
        UsageState::Unavailable
    );
    let (_, rows) = within("last hold removal notice", heard.recv())
        .await
        .unwrap();
    assert_eq!(rows[0].state, UsageState::Unavailable);
    let (client, _inbox) = mpsc::channel(8);
    cache.subscribe("account", generation, "client", RequestId(1), &client, "");
    cache.reset_lookup("account", now);
    let (_, rows) = within("checking with a listener", heard.recv())
        .await
        .unwrap();
    assert_eq!(rows[0].state, UsageState::Checking);
    cache.release("client", Some("account"));
    assert_eq!(
        lanes.host.usage_accounts()[0].state,
        UsageState::Unavailable
    );
    let (_, rows) = within("last listener release notice", heard.recv())
        .await
        .unwrap();
    assert_eq!(rows[0].state, UsageState::Unavailable);
}

#[tokio::test]
async fn a_usage_section_renders_again_once_its_observer_has_updated_it() {
    let calls = Calls::default();
    let gate = Arc::new(Semaphore::new(0));
    let observed = Arc::new(AtomicUsize::new(0));
    let (held, updated) = (gate.clone(), observed.clone());
    let script: Script = Arc::new(move |_, _, _| {
        let (gate, updated) = (held.clone(), updated.clone());
        Box::pin(async move {
            let _ = gate.acquire().await.map(|p| p.forget());
            updated.fetch_add(1, Ordering::SeqCst);
            Reply::None
        })
    });
    let render: Render = Arc::new(move |_, _, _| {
        let text = format!("heard {}", observed.load(Ordering::SeqCst));
        Box::pin(async move { Ok(hooks::line(&text)) })
    });
    let lanes = lanes(script, render, &calls).await;
    let (events, mut shown) = mpsc::channel(8);
    lanes
        .slots
        .show(vec!["context/limits".into()], Vec::new(), &events);
    let initial = within("initial render", shown.recv()).await.unwrap();
    assert!(serde_json::to_string(&initial).unwrap().contains("heard 0"));
    lanes.cache.observe(
        "account",
        lanes.generation,
        Ok(PlanUsage::new(None, vec![]).unwrap()),
        NOW,
    );
    within("pre-hook render", async {
        while calls.renders().len() < 2 {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await;
    gate.add_permits(1);
    let next = tokio::time::timeout(Duration::from_secs(2), shown.recv()).await;
    assert!(
        next.is_ok(),
        "usage hook completed without redrawing its marked section"
    );
    assert!(
        serde_json::to_string(&next.unwrap())
            .unwrap()
            .contains("heard 1")
    );
}
