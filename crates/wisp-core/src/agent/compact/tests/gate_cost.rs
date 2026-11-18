use wisp_plugin_testkit::hooks::{Calls, Script as HookScript};

use super::background::{NEAR, background, discarded};
use super::gate::{blocking_trigger, gate_body, kit_none};
use super::hooks::heard_by;
use super::overflow::{TOO_LONG, cut_off, too_long};
use super::*;
use crate::resume::LoggedSession;
use crate::test_guard::records;
use wisp_protocol::DiscardReason;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Mode {
    Hookless,
    Passing,
    Blocking,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum At {
    Overflow,
    BeforeTurn,
}

struct Run {
    seen: Vec<AgentEvent>,
    store: MemoryStore,
    provider: Arc<Fake>,
}

fn steps(at: At) -> Vec<Step> {
    match at {
        At::Overflow => vec![
            answer("Hello.", NEAR),
            Step::hang(),
            too_long(TOO_LONG),
            compaction(SUMMARY),
            answer("Again.", 300),
        ],
        At::BeforeTurn => vec![
            answer("Hello.", NEAR),
            Step::hang(),
            cut_off(2_000, "A long answer", false),
            compaction(SUMMARY),
            answer("Three.", 900),
        ],
    }
}

fn script(mode: Mode) -> HookScript {
    match mode {
        Mode::Blocking => blocking_trigger("overflow"),
        _ => kit_none(),
    }
}

async fn run(at: At, mode: Mode) -> Run {
    let calls = Calls::default();
    let store = MemoryStore::default();
    let provider = documented(steps(at));
    let agent = background(&provider, &store);
    let agent = if mode == Mode::Hookless {
        heard_by(agent, "", kit_none(), &calls).await
    } else {
        heard_by(agent, &gate_body(""), script(mode), &calls).await
    };
    let mut live = Live::start(agent);
    assert!(completed(&live.turn("u1", &"word ".repeat(4_000)).await));
    live.until(background_started).await;
    match at {
        At::Overflow => {
            live.turn("u2", "second").await;
        }
        At::BeforeTurn => {
            live.turn("u2", "second").await;
            live.turn("u3", "third").await;
        }
    }
    let (_, seen) = live.close().await;
    Run {
        seen,
        store,
        provider,
    }
}

fn last_turn_usd(seen: &[AgentEvent]) -> f64 {
    let ended = seen
        .iter()
        .rev()
        .find(|e| matches!(e, AgentEvent::TurnEnded { .. }))
        .unwrap();
    serde_json::to_value(ended).unwrap()["outcome"]["cost"]["turn_usd"]
        .as_f64()
        .unwrap()
}

fn resumed_usd(store: &MemoryStore) -> f64 {
    let logged = LoggedSession::read(&records(store)).unwrap();
    logged.last_turn.unwrap().usd.unwrap()
}

fn reasons(seen: &[AgentEvent]) -> Vec<String> {
    discarded(seen).into_iter().map(|(r, _)| r).collect()
}

async fn costs_match(at: At) {
    let hookless = run(at, Mode::Hookless).await;
    let passing = run(at, Mode::Passing).await;
    for run in [&hookless, &passing] {
        assert_eq!(
            reasons(&run.seen)[0],
            DiscardReason::Superseded.as_str(),
            "{at:?} {:#?}",
            run.seen
        );
        assert!(spent(&run.seen) > 0.0, "{at:?} {:#?}", run.seen);
        assert!((resumed_usd(&run.store) - last_turn_usd(&run.seen)).abs() < 1e-9);
    }
    let (plain, hooked) = (last_turn_usd(&hookless.seen), last_turn_usd(&passing.seen));
    assert!((plain - hooked).abs() < 1e-9, "{at:?} {plain} {hooked}");
    assert_eq!(
        hookless.provider.sent().len(),
        passing.provider.sent().len()
    );
}

fn spent(seen: &[AgentEvent]) -> f64 {
    seen.iter()
        .find_map(|e| match e {
            AgentEvent::CompactionFailed {
                trigger: CompactionTrigger::Background,
                cost: Some(cost),
                ..
            } => Some(cost.turn_usd),
            _ => None,
        })
        .unwrap_or_default()
}

#[tokio::test]
async fn a_precompute_discarded_behind_the_gate_at_an_overflow_is_charged_to_the_turn() {
    costs_match(At::Overflow).await;
}

#[tokio::test]
async fn a_precompute_discarded_behind_the_gate_before_a_turn_is_charged_to_the_turn() {
    costs_match(At::BeforeTurn).await;
}

async fn left_running(at: At) {
    let blocked = run(at, Mode::Blocking).await;
    assert!(
        !reasons(&blocked.seen)
            .iter()
            .any(|r| r == DiscardReason::Superseded.as_str()),
        "{at:?} {:#?}",
        blocked.seen
    );
    assert_eq!(
        reasons(&blocked.seen).last().map(String::as_str),
        Some(DiscardReason::SessionEnded.as_str())
    );
}

#[tokio::test]
async fn a_veto_at_an_overflow_leaves_the_precompute_running() {
    left_running(At::Overflow).await;
}
