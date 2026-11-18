use wisp_plugin_api::hooks::Reply;
use wisp_plugin_testkit::hooks::Calls;
use wisp_protocol::HookOutcome;

use super::auto::{LOW, no_retries};
use super::background::{NEAR, background, background_starts, swapped_in, tool_call};
use super::gate::{
    GATE, blocked_records, blocking, blocking_trigger, ceiling, gate_body, infos, kit_none, ran,
    replying,
};
use super::held::auto_starts;
use super::hooks::heard_by;
use super::overflow::{TOO_LONG, too_long};
use super::*;
use crate::agent::tests::held;
use crate::plugin::testing::hook_body;

#[tokio::test]
async fn a_vetoed_automatic_compaction_arms_no_hold_and_the_next_turn_asks_again() {
    let calls = Calls::default();
    let store = MemoryStore::default();
    let provider = scripted(vec![
        answer("Hello.", 5_000),
        answer("Two.", 5_100),
        answer("Three.", 5_200),
    ]);
    let agent = heard_by(
        on(&provider, &store, LOW),
        &gate_body(""),
        blocking(),
        &calls,
    )
    .await;
    let mut live = Live::start(agent);
    for (id, text) in [("u1", "first"), ("u2", "second"), ("u3", "third")] {
        assert!(completed(&live.turn(id, text).await));
    }
    let (_, seen) = live.close().await;
    assert!(held(&seen).is_empty(), "{:?}", held(&seen));
    assert_eq!(calls.count("gate"), 2);
    assert_eq!(blocked_records(&store).len(), 2);
    assert_eq!(provider.sent().len(), 3);
}

#[tokio::test]
async fn a_vetoed_overflow_of_the_request_after_a_compaction_leaves_that_compactions_measure() {
    let calls = Calls::default();
    let store = MemoryStore::default();
    let provider = scripted(vec![
        answer("Hello.", 5_000),
        compaction(SUMMARY),
        too_long(TOO_LONG),
        answer("Three.", 7_000),
    ]);
    let agent = no_retries(on(&provider, &store, LOW));
    let agent = heard_by(agent, &gate_body(""), blocking_trigger("overflow"), &calls).await;
    let mut live = Live::start(agent);
    assert!(completed(&live.turn("u1", "first").await));
    assert!(!completed(&live.turn("u2", "second").await));
    assert!(held(&live.seen).is_empty());
    assert!(completed(&live.turn("u3", "third").await));
    let (_, seen) = live.close().await;
    assert_eq!(calls.count("gate"), 2);
    assert_eq!(
        blocked_records(&store)
            .into_iter()
            .map(|(trigger, _)| trigger)
            .collect::<Vec<_>>(),
        [CompactionTrigger::Overflow]
    );
    let notices = held(&seen);
    assert_eq!(notices.len(), 1, "{notices:?}");
    assert_eq!(notices[0].1, 2_000);
    assert_eq!(provider.sent().len(), 4);
}

#[tokio::test]
async fn while_held_no_automatic_compaction_asks_the_gate_even_at_the_ceiling() {
    let calls = Calls::default();
    let store = MemoryStore::default();
    let ceiling = ceiling();
    let provider = scripted(vec![
        answer("Hello.", 5_000),
        compaction(SUMMARY),
        answer("Two.", 3_500),
        answer("Three.", ceiling + 1_000),
        answer("Four.", ceiling + 1_500),
        compaction(SUMMARY),
    ]);
    let agent = heard_by(
        on(&provider, &store, LOW),
        &gate_body(""),
        kit_none(),
        &calls,
    )
    .await;
    let mut live = Live::start(agent);
    for (id, text) in [("u1", "first"), ("u2", "second"), ("u3", "third")] {
        assert!(completed(&live.turn(id, text).await));
    }
    assert_eq!(held(&live.seen).len(), 1);
    assert!(completed(&live.turn("u4", "fourth").await));
    assert!(matches!(
        live.compact(9, None).await,
        AgentEvent::Compacted {
            trigger: CompactionTrigger::Manual,
            ..
        }
    ));
    let (_, seen) = live.close().await;
    assert_eq!(auto_starts(&seen), 1);
    let asked = infos(&calls, "gate");
    let triggers: Vec<&str> = asked.iter().map(|i| i.trigger.as_str()).collect();
    assert_eq!(triggers, ["auto", "manual"]);
    let points: Vec<Option<u64>> = asked.iter().map(|i| i.point).collect();
    assert_eq!(points, [Some(2_000), Some(2_000)]);
    assert!(asked[1].estimate >= ceiling, "{}", asked[1].estimate);
    assert_eq!(provider.sent().len(), 6);
}

#[tokio::test]
async fn a_ceiling_compaction_after_a_veto_arms_the_hold_like_any_automatic_one() {
    let calls = Calls::default();
    let store = MemoryStore::default();
    let ceiling = ceiling();
    let provider = scripted(vec![
        answer("Hello.", 5_000),
        tool_call(ceiling + 1_000, "Working."),
        compaction(SUMMARY),
        answer("Done.", 5_000),
        answer("Three.", 300),
    ]);
    let agent = heard_by(
        on(&provider, &store, LOW),
        &gate_body(""),
        blocking(),
        &calls,
    )
    .await;
    let mut live = Live::start(agent);
    for (id, text) in [("u1", "first"), ("u2", "second"), ("u3", "third")] {
        assert!(completed(&live.turn(id, text).await));
    }
    let (_, seen) = live.close().await;
    let blocked = HookOutcome::Blocked {
        reason: "not now".into(),
    };
    let ceilinged = HookOutcome::Rejected {
        rule: "ceiling".into(),
    };
    assert_eq!(ran(&store, GATE), [blocked, ceilinged]);
    let notices = held(&seen);
    assert_eq!(notices.len(), 1, "{notices:?}");
    assert_eq!(notices[0].1, 2_000);
    assert_eq!(provider.sent().len(), 5);
}

#[tokio::test]
async fn head_texts_that_leave_the_head_over_the_point_pause_the_turn_and_can_arm_the_hold() {
    let calls = Calls::default();
    let store = MemoryStore::default();
    let note = "z".repeat(10_000);
    let provider = scripted(vec![
        answer("Hello.", 5_000),
        summarised(SUMMARY, 6_000, 0, vec![]),
        answer("Two.", 3_000),
        answer("Three.", 3_100),
    ]);
    let script = replying(vec![("head", Reply::Append(vec![note.clone()]))]);
    let body = hook_body("head", "compaction-head", "");
    let agent = heard_by(on(&provider, &store, LOW), &body, script, &calls).await;
    let mut live = Live::start(agent);
    for (id, text) in [("u1", "first"), ("u2", "second"), ("u3", "third")] {
        assert!(completed(&live.turn(id, text).await));
    }
    let (_, seen) = live.close().await;
    assert_eq!(ran(&store, "aa/head"), [HookOutcome::Applied]);
    assert!(provider.body(2).to_string().contains(&note));
    assert!(
        seen.iter().any(|e| matches!(
            e,
            AgentEvent::Compacted {
                trigger: CompactionTrigger::Auto,
                paused: true,
                ..
            }
        )),
        "{seen:#?}"
    );
    let notices = held(&seen);
    assert_eq!(notices.len(), 1, "{notices:?}");
    assert_eq!(auto_starts(&seen), 1);
    assert_eq!(provider.sent().len(), 4);
}

#[tokio::test]
async fn while_held_the_background_gate_is_not_asked() {
    let calls = Calls::default();
    let store = MemoryStore::default();
    let provider = documented(vec![
        answer("Hello.", NEAR),
        compaction(SUMMARY),
        answer("Again.", 21_000),
        answer("More.", 21_500),
    ]);
    let agent = heard_by(
        background(&provider, &store),
        &gate_body(""),
        kit_none(),
        &calls,
    )
    .await;
    let mut live = Live::start(agent);
    assert!(completed(&live.turn("u1", "first").await));
    live.until(prepared).await;
    assert!(completed(&live.turn("u2", "second").await));
    assert!(completed(&live.turn("u3", "third").await));
    let (_, seen) = live.close().await;
    assert!(swapped_in(&seen, 2));
    assert_eq!(held(&seen).len(), 1, "{:?}", held(&seen));
    assert_eq!(background_starts(&seen), 1);
    assert_eq!(calls.count("gate"), 1);
    assert_eq!(provider.sent().len(), 4);
}
