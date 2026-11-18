use std::collections::BTreeMap;

use futures::FutureExt;

use super::background::{NEAR, background};
use super::wisp::written;
use super::*;
use crate::agent::retry::RetryPolicy;
use crate::lanes::Lanes;

const ENDPOINT: &str = "env";

fn limited() -> Lanes {
    Lanes::new(BTreeMap::from([(ENDPOINT.to_owned(), Some(1))]), None)
}

fn queued_steps(seen: &[AgentEvent]) -> Vec<(u32, u32)> {
    pick!(seen, AgentEvent::RequestQueued { step, ahead, .. } => (*step, *ahead))
}

async fn takes_a_slot(summary: Step, engine: CompactionEngine, config: &str) {
    let store = MemoryStore::default();
    let lanes = limited();
    let provider = scripted(vec![answer("Hello.", 1_000), summary]);
    let agent = logged(provider.clone(), &store)
        .with_model_settings(models(config))
        .with_lanes(lanes.clone());
    let mut live = Live::start(agent);
    assert!(completed(&live.turn("u1", "first").await));
    let held = match lanes.enter(ENDPOINT, 1) {
        Ok(pass) => pass,
        Err(_) => panic!("the lane was busy"),
    };
    live.send(compact(2, None)).await;
    live.until(|e| matches!(e, AgentEvent::RequestQueued { .. }))
        .await;
    assert_eq!(
        provider.sent().len(),
        1,
        "the compaction waits for the slot"
    );
    drop(held);
    let done = live.until(|e| answers(e, 2)).await;
    assert!(
        matches!(&done, AgentEvent::Compacted { engine: e, .. } if *e == engine),
        "{done:?}"
    );
    let (_, seen) = live.close().await;
    assert_eq!(provider.sent().len(), 2);
    assert_eq!(queued_steps(&seen), [(0, 0)]);
    assert!(
        seen.iter()
            .any(|e| matches!(e, AgentEvent::RequestAdmitted { step: 0, .. }))
    );
    assert_eq!(lanes.in_flight(ENDPOINT), 0);
}

#[tokio::test]
async fn a_native_compaction_takes_a_slot() {
    takes_a_slot(compaction(SUMMARY), CompactionEngine::Native, "").await;
}

#[tokio::test]
async fn wisps_own_compaction_takes_a_slot() {
    takes_a_slot(
        written(WRITTEN, 60_000),
        CompactionEngine::Wisp,
        "[models.\"claude-opus-5-5\"]\nnative_compaction = false\n",
    )
    .await;
}

#[tokio::test(start_paused = true)]
async fn the_main_agents_background_job_goes_before_a_deeper_waiter() {
    let store = MemoryStore::default();
    let lanes = limited();
    let provider = documented(vec![
        answer("Hello.", NEAR),
        server_failed(),
        compaction(SUMMARY),
        answer("Again.", 300),
    ]);
    let agent = background(&provider, &store)
        .with_retry_policy(RetryPolicy {
            step_retries: Some(1),
            base_delay: Duration::from_secs(10),
            jitter: || 0.0,
            busy_window: Some(Duration::ZERO),
            ..RetryPolicy::default()
        })
        .with_lanes(lanes.clone());
    let mut live = Live::start(agent);
    assert!(completed(&live.turn("u1", "first").await));
    while provider.sent().len() < 2 {
        tokio::task::yield_now().await;
    }
    let deep_stream = match lanes.enter(ENDPOINT, 2) {
        Ok(pass) => pass,
        Err(_) => panic!("the lane was busy"),
    };
    let mut deep_waiter = match lanes.enter(ENDPOINT, 2) {
        Ok(_) => panic!("admitted past a full lane"),
        Err(waiter) => waiter,
    };
    tokio::time::sleep(Duration::from_secs(11)).await;
    assert_eq!(lanes.waiting(ENDPOINT), 2, "the job queues for its retry");
    assert_eq!(deep_waiter.ahead(), 1, "the job ranks at depth 0");
    live.send(send("u2", &"word ".repeat(2_000))).await;
    tokio::time::sleep(Duration::from_secs(1)).await;
    assert_eq!(
        provider.sent().len(),
        2,
        "the turn waits for its compaction"
    );
    drop(deep_stream);
    assert!(deep_waiter.admitted(1).now_or_never().is_none());
    let outcome = loop {
        if let AgentEvent::TurnEnded { outcome, .. } = live
            .until(|e| matches!(e, AgentEvent::TurnEnded { .. }))
            .await
        {
            break outcome;
        }
    };
    assert!(completed(&outcome), "{outcome:?}");
    let sent = provider.sent();
    assert_eq!(sent.len(), 4);
    assert_eq!(
        sent[2].body(),
        sent[1].body(),
        "the job's retry sends its first bytes"
    );
    assert!(lead_of_compaction(&sent[3].body()), "{}", sent[3].body());
    assert!(deep_waiter.admitted(0).now_or_never().is_some());
    drop(live.close().await);
}

fn lead_of_compaction(body: &Value) -> bool {
    body["messages"][0]["content"][0]["type"] == "compaction"
}
