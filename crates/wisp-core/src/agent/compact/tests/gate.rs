use tokio::sync::Notify;
use wisp_plugin_api::hooks::{CompactInfo, Event, Reply};
use wisp_plugin_testkit::hooks::{Calls, Script as HookScript};
use wisp_protocol::settings::{Withdrawal, default_point};
use wisp_protocol::{HookChange, HookOutcome, Placement};

use super::auto::{LOW, no_retries};
use super::background::{background, discarded, tool_call};
use super::hooks::heard_by;
use super::overflow::{
    TOO_LONG, cut_off, error_of, off, overflow_started, three_answers, too_long,
};
use super::smaller::{WINDOWS_NO_NATIVE, windows};
use super::*;
use crate::agent::compact::tests::marker::change;
use crate::agent::compact::tests::quote::withdrawal;
use crate::agent::tests::gated;
use crate::plugin::testing::hook_body;
use wisp_plugin_compaction::summary_instruction;

pub(super) const GATE: &str = "aa/gate";
const LATER: &str = "aa/later";

pub(super) fn gate_body(extra: &str) -> String {
    hook_body("gate", "before-compact", extra)
}

fn gate_then_later(extra: &str) -> String {
    gate_body(extra) + &hook_body("later", "before-compact", "after = [\"aa/gate\"]\n")
}

pub(super) fn replying(replies: Vec<(&'static str, Reply)>) -> HookScript {
    Arc::new(move |_, hook, _| {
        let reply = replies
            .iter()
            .find(|(name, _)| *name == hook)
            .map_or(Reply::None, |(_, r)| r.clone());
        Box::pin(async move { reply })
    })
}

pub(super) fn blocking() -> HookScript {
    replying(vec![("gate", Reply::Block("not now".into()))])
}

pub(super) fn blocking_trigger(trigger: &'static str) -> HookScript {
    Arc::new(move |_, hook, event| {
        let reply = match event {
            Event::BeforeCompact(info) if hook == "gate" && info.trigger == trigger => {
                Reply::Block("not now".into())
            }
            _ => Reply::None,
        };
        Box::pin(async move { reply })
    })
}

pub(super) fn infos(calls: &Calls, hook: &str) -> Vec<CompactInfo> {
    calls
        .of(hook)
        .into_iter()
        .filter_map(|c| match c.event {
            Event::BeforeCompact(info) => Some(info),
            _ => None,
        })
        .collect()
}

pub(super) fn ran(store: &MemoryStore, hook: &str) -> Vec<HookOutcome> {
    log_events(store)
        .into_iter()
        .filter_map(|e| match e {
            LogEvent::HookRan {
                hook: h, outcome, ..
            } if h == hook => Some(outcome),
            _ => None,
        })
        .collect()
}

pub(super) fn blocked_records(store: &MemoryStore) -> Vec<(CompactionTrigger, String)> {
    pick!(
        log_events(store),
        LogEvent::CompactionFailed {
            trigger,
            kind: CompactionFailure::Blocked,
            message,
            ..
        } => (trigger, message)
    )
}

fn blocked_event(trigger: CompactionTrigger) -> impl Fn(&AgentEvent) -> bool {
    move |e| {
        matches!(
            e,
            AgentEvent::CompactionFailed { trigger: t, kind: CompactionFailure::Blocked, .. }
                if *t == trigger
        )
    }
}

fn compacted_triggers(store: &MemoryStore) -> Vec<CompactionTrigger> {
    pick!(log_events(store), LogEvent::ContextCompacted { trigger, .. } => trigger)
}

pub(super) fn ceiling() -> u64 {
    let agent = Agent::new(scripted(Vec::new()), default_session());
    let window = agent.window_and_point().0.unwrap();
    default_point(window, agent.session.max_tokens)
}

pub(super) const NOT_NOW: &str = "Blocked by aa/gate: not now";

#[tokio::test]
async fn a_compact_blocked_by_a_hook_answers_blocked_naming_it_and_sends_nothing() {
    let calls = Calls::default();
    let store = MemoryStore::default();
    let provider = scripted(vec![answer("Hello.", 1_000)]);
    let agent = heard_by(
        logged(provider.clone(), &store),
        &gate_body(""),
        blocking(),
        &calls,
    )
    .await;
    let mut live = Live::start(agent);
    assert!(completed(&live.turn("u1", "first").await));
    let answered = live.compact(2, None).await;
    live.close().await;
    assert!(
        matches!(
            &answered,
            AgentEvent::CompactionFailed {
                trigger: CompactionTrigger::Manual,
                kind: CompactionFailure::Blocked,
                message,
                answers: Some(RequestId(2)),
                ..
            } if message == NOT_NOW
        ),
        "{answered:?}"
    );
    assert_eq!(provider.sent().len(), 1);
    assert_eq!(
        blocked_records(&store),
        [(CompactionTrigger::Manual, NOT_NOW.to_owned())]
    );
    assert_eq!(
        ran(&store, GATE),
        [HookOutcome::Blocked {
            reason: "not now".into()
        }]
    );
    let info = &infos(&calls, "gate")[0];
    assert_eq!(
        (
            info.trigger.as_str(),
            info.engine.as_str(),
            info.turn,
            info.epoch
        ),
        ("manual", "native", None, 0)
    );
    assert_eq!(info.instruction, summary_instruction(None));
}

fn rewriting() -> HookScript {
    replying(vec![
        ("one", Reply::Rewrite("Summarise briefly.".into())),
        ("two", Reply::Append(vec!["Keep the plan.".into()])),
    ])
}

fn rewriters() -> String {
    hook_body("one", "before-compact", "")
        + &hook_body("two", "before-compact", "after = [\"aa/one\"]\n")
}

const HOOKED: &str = "Summarise briefly.\nKeep the plan.";

fn applied(store: &MemoryStore) -> Vec<(String, String, String)> {
    log_events(store)
        .into_iter()
        .filter_map(|e| match e {
            LogEvent::HookApplied {
                hook,
                point,
                change: HookChange::Text { before, after },
                ..
            } if point == "before-compact" => Some((hook, before, after)),
            _ => None,
        })
        .collect()
}

fn instructions_by(store: &MemoryStore) -> Vec<String> {
    log_events(store)
        .into_iter()
        .find_map(|e| match e {
            LogEvent::ContextCompacted {
                instructions_by, ..
            } => Some(instructions_by),
            _ => None,
        })
        .unwrap()
}

#[tokio::test]
async fn a_rewritten_instruction_is_sent_by_wisps_engine_without_the_section_check() {
    let mut bodies = Vec::new();
    for hooked in [false, true] {
        let calls = Calls::default();
        let store = MemoryStore::default();
        let reply = if hooked { "A short summary." } else { WRITTEN };
        let provider = scripted(vec![answer("Hello.", 1_000), answer(reply, 60_000)]);
        let script = if hooked { rewriting() } else { kit_none() };
        let agent = heard_by(wisp_engine(&provider, &store), &rewriters(), script, &calls).await;
        let mut live = Live::start(agent);
        live.turn("u1", "first").await;
        let answered = live.compact(2, None).await;
        live.close().await;
        assert!(
            matches!(answered, AgentEvent::Compacted { .. }),
            "{answered:?}"
        );
        let body = provider.body(1);
        let text = body.to_string();
        if hooked {
            assert!(
                text.contains("Summarise briefly.\\nKeep the plan."),
                "{text}"
            );
            assert!(!text.contains("six sections"), "{text}");
            let default = summary_instruction(None);
            assert_eq!(
                applied(&store),
                [
                    (
                        "aa/one".into(),
                        default.clone(),
                        "Summarise briefly.".into()
                    ),
                    ("aa/two".into(), "Summarise briefly.".into(), HOOKED.into()),
                ]
            );
            assert_eq!(instructions_by(&store), ["aa/one", "aa/two"]);
            let seen = infos(&calls, "two");
            assert_eq!(seen[0].instruction, "Summarise briefly.");
        } else {
            assert!(instructions_by(&store).is_empty());
            assert!(applied(&store).is_empty());
        }
        let instruction = if hooked {
            HOOKED.to_owned()
        } else {
            summary_instruction(None)
        };
        let swapped = text.replace(&serde_json::to_string(&instruction).unwrap(), "\"I\"");
        bodies.push(crate::agent::testing::without_stamps(&swapped));
    }
    assert_eq!(bodies[0], bodies[1]);
}

#[tokio::test]
async fn a_rewritten_instruction_keeps_the_other_summary_checks() {
    let calls = Calls::default();
    let store = MemoryStore::default();
    let provider = scripted(vec![answer("Hello.", 1_000), answer("A short summary.", 5)]);
    let agent = heard_by(
        wisp_engine(&provider, &store),
        &rewriters(),
        rewriting(),
        &calls,
    )
    .await;
    let mut live = Live::start(agent);
    live.turn("u1", "first").await;
    let answered = live.compact(2, None).await;
    live.close().await;
    assert!(
        matches!(
            answered,
            AgentEvent::CompactionFailed {
                kind: CompactionFailure::CheckFailed,
                ..
            }
        ),
        "{answered:?}"
    );
}

#[tokio::test]
async fn a_rewritten_instruction_is_sent_as_the_native_instructions() {
    let calls = Calls::default();
    let store = MemoryStore::default();
    let provider = scripted(vec![answer("Hello.", 1_000), compaction(SUMMARY)]);
    let agent = heard_by(
        logged(provider.clone(), &store),
        &rewriters(),
        rewriting(),
        &calls,
    )
    .await;
    let mut live = Live::start(agent);
    live.turn("u1", "first").await;
    live.compact(2, None).await;
    live.close().await;
    let text = provider.body(1).to_string();
    assert!(
        text.contains("Summarise briefly.\\nKeep the plan."),
        "{text}"
    );
    assert_eq!(instructions_by(&store), ["aa/one", "aa/two"]);
}

#[tokio::test]
async fn a_blank_rewrite_is_rejected_and_the_default_instruction_sent() {
    let calls = Calls::default();
    let store = MemoryStore::default();
    let provider = scripted(vec![answer("Hello.", 1_000), compaction(SUMMARY)]);
    let script = replying(vec![("gate", Reply::Rewrite("   ".into()))]);
    let agent = heard_by(
        logged(provider.clone(), &store),
        &gate_body(""),
        script,
        &calls,
    )
    .await;
    let mut live = Live::start(agent);
    live.turn("u1", "first").await;
    live.compact(2, None).await;
    live.close().await;
    assert_eq!(
        ran(&store, GATE),
        [HookOutcome::Rejected {
            rule: "blank".into()
        }]
    );
    let text = provider.body(1).to_string();
    let default = serde_json::to_string(&summary_instruction(None)).unwrap();
    assert!(text.contains(default.trim_matches('"')), "{text}");
    assert!(applied(&store).is_empty());
}

#[tokio::test]
async fn an_automatic_veto_holds_for_the_turn_then_the_ceiling_compaction_runs() {
    let calls = Calls::default();
    let store = MemoryStore::default();
    let ceiling = ceiling();
    let provider = scripted(vec![
        answer("Hello.", 5_000),
        tool_call(ceiling + 1_000, "Working."),
        compaction(SUMMARY),
        answer("Done.", 1_500),
        answer("Three.", 300),
    ]);
    let agent = heard_by(
        on(&provider, &store, LOW),
        &gate_then_later(""),
        blocking(),
        &calls,
    )
    .await;
    let mut live = Live::start(agent);
    assert!(completed(&live.turn("u1", "first").await));
    assert!(completed(&live.turn("u2", "second").await));
    assert!(completed(&live.turn("u3", &"x".repeat(4_000)).await));
    let (_, seen) = live.close().await;
    let blocked = HookOutcome::Blocked {
        reason: "not now".into(),
    };
    let ceilinged = HookOutcome::Rejected {
        rule: "ceiling".into(),
    };
    assert_eq!(ran(&store, GATE), [blocked.clone(), ceilinged, blocked]);
    assert_eq!(calls.count("later"), 1);
    assert_eq!(
        blocked_records(&store),
        [
            (CompactionTrigger::Auto, NOT_NOW.to_owned()),
            (CompactionTrigger::Auto, NOT_NOW.to_owned())
        ]
    );
    assert_eq!(compacted_triggers(&store), [CompactionTrigger::Auto]);
    let points: Vec<Option<u64>> = infos(&calls, "gate").iter().map(|i| i.point).collect();
    assert_eq!(points, [Some(2_000), Some(ceiling), Some(2_000)]);
    let estimates: Vec<u64> = infos(&calls, "gate").iter().map(|i| i.estimate).collect();
    assert!(
        estimates[1] >= ceiling && estimates[0] < ceiling,
        "{estimates:?}"
    );
    assert!(
        !seen
            .iter()
            .any(|e| matches!(e, AgentEvent::CompactionFailed { paused: true, .. }))
    );
    assert_eq!(provider.sent().len(), 5);
}

fn failing_hooks() -> HookScript {
    Arc::new(|_, hook, _| {
        Box::pin(async move {
            match hook.as_str() {
                "boom" => panic!("hook payload"),
                "slow" => {
                    tokio::time::sleep(Duration::from_secs(3600)).await;
                    Reply::None
                }
                _ => Reply::None,
            }
        })
    })
}

fn failing_body() -> String {
    let extra = "on_failure = \"block\"\ntimeout_ms = 20\n";
    hook_body("boom", "before-compact", extra)
        + &hook_body(
            "slow",
            "before-compact",
            &format!("after = [\"aa/boom\"]\n{extra}"),
        )
        + &hook_body("later", "before-compact", "after = [\"aa/slow\"]\n")
}

async fn failing_at_the_ceiling(boundary: bool) {
    let calls = Calls::default();
    let store = MemoryStore::default();
    let ceiling = ceiling();
    let steps = if boundary {
        vec![
            answer("Hello.", 1_000),
            tool_call(ceiling + 1_000, "Working."),
            compaction(SUMMARY),
            answer("Done.", 900),
        ]
    } else {
        vec![
            answer("Hello.", ceiling + 1_000),
            compaction(SUMMARY),
            answer("Done.", 900),
        ]
    };
    let provider = scripted(steps);
    let agent = heard_by(
        on(&provider, &store, LOW),
        &failing_body(),
        failing_hooks(),
        &calls,
    )
    .await;
    let mut live = Live::start(agent);
    live.turn("u1", "first").await;
    assert!(completed(&live.turn("u2", "second").await));
    live.close().await;
    assert!(matches!(
        ran(&store, "aa/boom").last(),
        Some(HookOutcome::Failed { .. })
    ));
    assert_eq!(ran(&store, "aa/slow").last(), Some(&HookOutcome::TimedOut));
    assert_eq!(ran(&store, LATER).last(), Some(&HookOutcome::None));
    assert!(blocked_records(&store).is_empty());
    assert!(compacted_triggers(&store).contains(&CompactionTrigger::Auto));
}

#[tokio::test]
async fn at_the_ceiling_a_failing_blocking_hook_lets_the_compaction_before_a_turn_run() {
    failing_at_the_ceiling(false).await;
}

#[tokio::test]
async fn at_the_ceiling_a_failing_blocking_hook_lets_the_compaction_at_a_boundary_run() {
    failing_at_the_ceiling(true).await;
}

#[tokio::test]
async fn on_failure_block_vetoes_a_compact_on_a_failure_and_a_timeout() {
    for (hook, outcome) in [("boom", "failed"), ("slow", "timed_out")] {
        let calls = Calls::default();
        let store = MemoryStore::default();
        let provider = scripted(vec![answer("Hello.", 1_000)]);
        let body = hook_body(
            hook,
            "before-compact",
            "on_failure = \"block\"\ntimeout_ms = 20\n",
        );
        let agent = heard_by(
            logged(provider.clone(), &store),
            &body,
            failing_hooks(),
            &calls,
        )
        .await;
        let mut live = Live::start(agent);
        live.turn("u1", "first").await;
        let answered = live.compact(2, None).await;
        live.close().await;
        assert!(
            matches!(
                answered,
                AgentEvent::CompactionFailed {
                    kind: CompactionFailure::Blocked,
                    ..
                }
            ),
            "{outcome}: {answered:?}"
        );
        assert_eq!(provider.sent().len(), 1);
    }
}

#[tokio::test]
async fn a_vetoed_overflow_after_a_rejected_request_ends_with_the_overflows_own_error() {
    let calls = Calls::default();
    let store = MemoryStore::default();
    let mut steps = three_answers();
    steps.push(too_long(TOO_LONG));
    let provider = scripted(steps);
    let agent = heard_by(off(&provider, &store), &gate_body(""), blocking(), &calls).await;
    let mut live = Live::start(agent);
    three_turns(&mut live).await;
    let outcome = live.turn("u4", "fourth").await;
    let (_, seen) = live.close().await;
    assert_eq!(
        error_of(&outcome),
        (ErrorKind::ContextOverflow, TOO_LONG.to_owned())
    );
    assert_eq!(provider.sent().len(), 4);
    let notice = position(&seen, blocked_event(CompactionTrigger::Overflow));
    let ended = seen
        .iter()
        .rposition(|e| matches!(e, AgentEvent::TurnEnded { .. }))
        .unwrap();
    assert!(notice < ended);
    assert_eq!(seen.iter().filter(|e| overflow_started(e)).count(), 1);
    assert_eq!(
        seen.iter()
            .filter(|e| matches!(e, AgentEvent::TurnEnded { .. }))
            .count(),
        4
    );
}

#[tokio::test]
async fn a_vetoed_cut_off_compaction_sends_on_and_an_overflow_then_ends_the_turn() {
    let calls = Calls::default();
    let store = MemoryStore::default();
    let provider = scripted(vec![
        answer("One.", 1_000),
        cut_off(2_000, "Looking around.", true),
        too_long(TOO_LONG),
    ]);
    let agent = heard_by(off(&provider, &store), &gate_body(""), blocking(), &calls).await;
    let mut live = Live::start(agent);
    assert!(completed(&live.turn("u1", "first").await));
    let outcome = live.turn("u2", "second").await;
    let (agent, seen) = live.close().await;
    assert_eq!(error_of(&outcome).0, ErrorKind::ContextOverflow);
    assert_eq!(provider.sent().len(), 3);
    assert!(seen.iter().any(blocked_event(CompactionTrigger::Overflow)));
    assert_eq!(seen.iter().filter(|e| overflow_started(e)).count(), 1);
    assert_eq!(agent.compaction.overflow.cut_off, None);
    assert_eq!(blocked_records(&store).len(), 1);
}

#[tokio::test]
async fn a_vetoed_overflow_before_a_turn_lets_the_turn_send() {
    let calls = Calls::default();
    let store = MemoryStore::default();
    let provider = scripted(vec![
        answer("One.", 1_000),
        cut_off(2_000, "A long answer", false),
        answer("Three.", 900),
    ]);
    let agent = heard_by(off(&provider, &store), &gate_body(""), blocking(), &calls).await;
    let mut live = Live::start(agent);
    assert!(completed(&live.turn("u1", "first").await));
    live.turn("u2", "second").await;
    assert!(completed(&live.turn("u3", "third").await));
    let (_, seen) = live.close().await;
    assert_eq!(provider.sent().len(), 3);
    assert_eq!(
        blocked_records(&store),
        [(CompactionTrigger::Overflow, NOT_NOW.to_owned())]
    );
    assert!(seen.iter().any(blocked_event(CompactionTrigger::Overflow)));
}

#[tokio::test]
async fn a_vetoed_overflow_before_a_turn_then_a_vetoed_automatic_one_gives_two_records() {
    let calls = Calls::default();
    let store = MemoryStore::default();
    let provider = scripted(vec![
        answer("One.", 1_000),
        cut_off(5_000, "A long answer", false),
        answer("Three.", 900),
    ]);
    let agent = heard_by(
        on(&provider, &store, LOW),
        &gate_body(""),
        blocking(),
        &calls,
    )
    .await;
    let mut live = Live::start(agent);
    live.turn("u1", "first").await;
    live.turn("u2", "second").await;
    assert!(completed(&live.turn("u3", "third").await));
    live.close().await;
    assert_eq!(
        blocked_records(&store),
        [
            (CompactionTrigger::Overflow, NOT_NOW.to_owned()),
            (CompactionTrigger::Auto, NOT_NOW.to_owned())
        ]
    );
}

#[tokio::test]
async fn a_vetoed_overflow_before_a_turn_at_the_ceiling_is_followed_by_the_ceiling_compaction() {
    let calls = Calls::default();
    let store = MemoryStore::default();
    let ceiling = ceiling();
    let provider = scripted(vec![
        answer("One.", 1_000),
        cut_off(ceiling + 1_000, "A long answer", false),
        compaction(SUMMARY),
        answer("Three.", 900),
    ]);
    let agent = heard_by(
        on(&provider, &store, LOW),
        &gate_body(""),
        blocking(),
        &calls,
    )
    .await;
    let mut live = Live::start(agent);
    live.turn("u1", "first").await;
    live.turn("u2", "second").await;
    assert!(completed(&live.turn("u3", "third").await));
    live.close().await;
    assert_eq!(
        blocked_records(&store),
        [(CompactionTrigger::Overflow, NOT_NOW.to_owned())]
    );
    assert_eq!(compacted_triggers(&store), [CompactionTrigger::Auto]);
    assert_eq!(
        ran(&store, GATE).last(),
        Some(&HookOutcome::Rejected {
            rule: "ceiling".into()
        })
    );
}

async fn switched(long: bool) -> (Agent, Vec<AgentEvent>, MemoryStore, Arc<Fake>) {
    let calls = Calls::default();
    let store = MemoryStore::default();
    let provider = scripted(vec![
        answer("Hello.", 1_000),
        compaction(SUMMARY),
        answer("Second.", 300_000),
        answer("Third.", 2_000),
    ]);
    let agent = heard_by(
        windows(&store, &provider, WINDOWS_NO_NATIVE),
        &gate_body(""),
        blocking_trigger("switch"),
        &calls,
    )
    .await;
    let mut live = Live::start(agent);
    live.turn("u1", "first").await;
    live.compact(2, None).await;
    assert!(completed(&live.turn("u2", "second").await));
    change(&mut live, 3, Some("other"), None).await;
    let text = if long {
        "word ".repeat(200_000)
    } else {
        "third".to_owned()
    };
    assert!(completed(&live.turn("u3", &text).await));
    let (agent, seen) = live.close().await;
    (agent, seen, store, provider)
}

#[tokio::test]
async fn a_vetoed_switch_compaction_goes_ahead_when_the_conversation_fits() {
    let (agent, seen, store, provider) = switched(false).await;
    assert!(withdrawal(&seen).is_none());
    assert_eq!(agent.endpoint_name(), "other");
    assert_eq!(provider.sent().len(), 4);
    assert_eq!(
        blocked_records(&store),
        [(CompactionTrigger::Switch, NOT_NOW.to_owned())]
    );
    assert!(seen.iter().any(blocked_event(CompactionTrigger::Switch)));
}

#[tokio::test]
async fn a_vetoed_switch_compaction_is_withdrawn_when_the_conversation_doesnt_fit() {
    let (agent, seen, _, _) = switched(true).await;
    assert!(matches!(
        &seen[withdrawal(&seen).unwrap()],
        AgentEvent::SettingsChanged {
            withdrawn: Some(Withdrawal::SwitchCompactionFailed),
            ..
        }
    ));
    assert_eq!(agent.endpoint_name(), "env");
}

#[tokio::test]
async fn esc_during_the_gate_of_a_compact_cancels_it_with_nothing_sent() {
    let calls = Calls::default();
    let store = MemoryStore::default();
    let gate = Arc::new(Notify::new());
    let started = Arc::new(Notify::new());
    let provider = scripted(vec![answer("Hello.", 1_000)]);
    let script = gated(gate.clone(), started.clone());
    let agent = heard_by(
        logged(provider.clone(), &store),
        &gate_body(""),
        script,
        &calls,
    )
    .await;
    let mut live = Live::start(agent);
    live.turn("u1", "first").await;
    live.send(compact(2, None)).await;
    within("the hook's start", started.notified()).await;
    live.send(ClientCommand::Cancel).await;
    let answered = live.until(|e| answers(e, 2)).await;
    live.close().await;
    assert!(
        matches!(
            answered,
            AgentEvent::CompactionFailed {
                kind: CompactionFailure::Cancelled,
                ..
            }
        ),
        "{answered:?}"
    );
    assert_eq!(provider.sent().len(), 1);
    assert_eq!(ran(&store, GATE), [HookOutcome::Cancelled]);
    assert!(
        !log_events(&store)
            .iter()
            .any(|e| matches!(e, LogEvent::TurnStarted { turn: 2, .. }))
    );
}

#[tokio::test]
async fn esc_during_the_gate_of_an_automatic_compaction_ends_the_turn_cancelled() {
    let calls = Calls::default();
    let store = MemoryStore::default();
    let gate = Arc::new(Notify::new());
    let started = Arc::new(Notify::new());
    let provider = scripted(vec![answer("Hello.", 5_000)]);
    let script = gated(gate.clone(), started.clone());
    let agent = heard_by(on(&provider, &store, LOW), &gate_body(""), script, &calls).await;
    let mut live = Live::start(agent);
    live.turn("u1", "first").await;
    live.send(send("u2", "second")).await;
    within("the hook's start", started.notified()).await;
    live.send(ClientCommand::Cancel).await;
    let ended = live
        .until(|e| matches!(e, AgentEvent::TurnEnded { .. }))
        .await;
    live.close().await;
    assert!(matches!(
        ended,
        AgentEvent::TurnEnded {
            outcome: TurnOutcome::Cancelled { .. },
            ..
        }
    ));
    assert_eq!(provider.sent().len(), 1);
}

#[tokio::test]
async fn a_summarise_that_converts_asks_no_gate() {
    let calls = Calls::default();
    let store = MemoryStore::default();
    let provider = scripted(Vec::new());
    let agent = wisp_engine(&provider, &store);
    let mut agent = heard_by(agent, &gate_body(""), kit_none(), &calls).await;
    agent.history.push(Message {
        role: Role::User,
        content: vec![ContentBlock::Text {
            text: "first".into(),
        }],
    });
    agent.compaction.head = Some(Head {
        epoch: 1,
        from: 0,
        message: Message {
            role: Role::User,
            content: vec![ContentBlock::opaque("anthropic", raw_block(SUMMARY))],
        },
        summary: SUMMARY.into(),
        native: true,
        turns: (1, 1),
        cut: 0,
        placement: Placement::Head,
        additions: Vec::new(),
        join: None,
    });
    let (_commands, mut inbox) = mpsc::channel(4);
    let (sink, _events) = mpsc::channel(64);
    let gate = agent.gate(CompactionTrigger::Manual);
    let outcome = agent
        .summarise((gate, None), true, Fit::View, &mut inbox, &sink)
        .await;
    assert!(matches!(outcome, Outcome::Convert));
    assert_eq!(calls.count("gate"), 0);
}

pub(super) fn kit_none() -> HookScript {
    wisp_plugin_testkit::hooks::none()
}

#[tokio::test]
async fn the_gate_fires_once_across_a_fall_back_to_wisps_engine() {
    let calls = Calls::default();
    let store = MemoryStore::default();
    let provider = scripted(vec![
        answer("Hello.", 1_000),
        beta_refused(),
        answer(WRITTEN, 60_000),
    ]);
    let agent = heard_by(
        logged(provider.clone(), &store),
        &gate_body(""),
        kit_none(),
        &calls,
    )
    .await;
    let mut live = Live::start(agent);
    live.turn("u1", "first").await;
    let answered = live.compact(2, None).await;
    live.close().await;
    assert!(
        matches!(answered, AgentEvent::Compacted { .. }),
        "{answered:?}"
    );
    assert_eq!(provider.sent().len(), 3);
    assert_eq!(calls.count("gate"), 1);
}

#[tokio::test]
async fn a_child_fires_the_gate_and_the_head_point() {
    let calls = Calls::default();
    let store = MemoryStore::default();
    let provider = scripted(vec![
        answer("Hello.", 5_000),
        compaction(SUMMARY),
        answer("Again.", 300),
    ]);
    let body = gate_body("") + &hook_body("head", "compaction-head", "");
    let link = wisp_protocol::ParentLink {
        session_id: "parent".into(),
        agent: wisp_protocol::AgentPath::root(),
        depth: 1,
        config: wisp_protocol::SpawnChoices::default(),
        todos: Vec::new(),
    };
    let agent = heard_by(
        on(&provider, &store, LOW).with_parent(link),
        &body,
        kit_none(),
        &calls,
    )
    .await;
    let mut live = Live::start(agent);
    live.turn("u1", "first").await;
    assert!(completed(&live.turn("u2", "second").await));
    live.close().await;
    assert_eq!((calls.count("gate"), calls.count("head")), (1, 1));
}

#[tokio::test]
async fn a_veto_leaves_a_running_precompute_running() {
    let calls = Calls::default();
    let store = MemoryStore::default();
    let provider = documented(vec![
        answer("Hello.", super::background::NEAR),
        Step::hang(),
    ]);
    let agent = heard_by(
        background(&provider, &store),
        &gate_body(""),
        blocking_trigger("manual"),
        &calls,
    )
    .await;
    let mut live = Live::start(agent);
    assert!(completed(&live.turn("u1", "first").await));
    live.until(|e| {
        matches!(
            e,
            AgentEvent::CompactionStarted {
                trigger: CompactionTrigger::Background,
                ..
            }
        )
    })
    .await;
    let answered = live.compact(2, None).await;
    let (agent, seen) = live.close().await;
    assert!(matches!(
        answered,
        AgentEvent::CompactionFailed {
            kind: CompactionFailure::Blocked,
            ..
        }
    ));
    let discards = seen
        .iter()
        .filter(|e| {
            matches!(
                e,
                AgentEvent::CompactionFailed {
                    trigger: CompactionTrigger::Background,
                    ..
                }
            )
        })
        .count();
    assert!(discards <= 1);
    let blocked = position(&seen, blocked_event(CompactionTrigger::Manual));
    assert!(!seen[..blocked].iter().any(|e| matches!(
        e,
        AgentEvent::CompactionFailed {
            trigger: CompactionTrigger::Background,
            ..
        }
    )));
    assert_eq!(agent.background_phase(), "none");
    let reasons: Vec<String> = discarded(&seen).into_iter().map(|(r, _)| r).collect();
    assert_eq!(reasons, ["session_ended"]);
}

#[tokio::test]
async fn a_blocking_gate_starts_no_precompute_and_logs_only_its_run() {
    let calls = Calls::default();
    let store = MemoryStore::default();
    let near = super::background::NEAR;
    let provider = documented(vec![answer("Hello.", near), answer("Again.", near)]);
    let agent = heard_by(
        background(&provider, &store),
        &gate_body(""),
        blocking(),
        &calls,
    )
    .await;
    let mut live = Live::start(agent);
    assert!(completed(&live.turn("u1", "first").await));
    assert!(completed(&live.turn("u2", "second").await));
    live.until(|e| matches!(e, AgentEvent::HookRan { turn: Some(2), .. }))
        .await;
    let (agent, seen) = live.close().await;
    assert_eq!(provider.sent().len(), 2);
    let blocked = HookOutcome::Blocked {
        reason: "not now".into(),
    };
    assert_eq!(ran(&store, GATE), [blocked.clone(), blocked]);
    assert!(
        !log_events(&store)
            .iter()
            .any(|e| matches!(e, LogEvent::CompactionFailed { .. }))
    );
    assert!(!seen.iter().any(|e| matches!(
        e,
        AgentEvent::CompactionFailed { .. } | AgentEvent::CompactionStarted { .. }
    )));
    let triggers: Vec<String> = infos(&calls, "gate")
        .iter()
        .map(|i| i.trigger.clone())
        .collect();
    assert_eq!(triggers, ["background", "background"]);
    assert_eq!(infos(&calls, "gate")[0].turn, Some(1));
    assert_eq!(agent.background_phase(), "none");
}

fn sleeping() -> HookScript {
    Arc::new(|_, _, _| {
        Box::pin(async {
            tokio::time::sleep(Duration::from_secs(3600)).await;
            Reply::None
        })
    })
}

#[tokio::test]
async fn the_background_gates_two_second_cut_vetoes_the_precompute_under_on_failure_block() {
    let calls = Calls::default();
    let store = MemoryStore::default();
    let near = super::background::NEAR;
    let provider = documented(vec![answer("Hello.", near)]);
    let body = gate_body("on_failure = \"block\"\ntimeout_ms = 10000\n");
    let agent = heard_by(background(&provider, &store), &body, sleeping(), &calls).await;
    let mut live = Live::start(agent);
    let started = std::time::Instant::now();
    assert!(completed(&live.turn("u1", "first").await));
    live.until(|e| {
        matches!(
            e,
            AgentEvent::HookRan {
                outcome: HookOutcome::TimedOut,
                ..
            }
        )
    })
    .await;
    assert!(started.elapsed() < Duration::from_secs(5));
    let (_, seen) = live.close().await;
    assert!(!seen.iter().any(background_started));
    assert_eq!(provider.sent().len(), 1);
}

#[tokio::test]
async fn the_background_gate_is_not_asked_without_an_anchor() {
    let calls = Calls::default();
    let store = MemoryStore::default();
    let near = super::background::NEAR;
    let provider = documented(vec![
        answer("Hello.", near),
        answer("Again.", near + 100),
        compaction(SUMMARY),
    ]);
    let agent = heard_by(
        background(&provider, &store),
        &gate_body(""),
        kit_none(),
        &calls,
    )
    .await;
    let mut live = Live::start(agent);
    change(&mut live, 1, None, Some(wisp_protocol::Effort::Low)).await;
    assert!(completed(&live.turn("u1", "first").await));
    tokio::time::sleep(Duration::from_millis(100)).await;
    assert_eq!(calls.count("gate"), 0);
    assert!(completed(&live.turn("u2", "second").await));
    live.until(prepared).await;
    live.close().await;
    assert_eq!(calls.count("gate"), 1);
}

#[tokio::test]
async fn a_rewritten_background_instruction_is_sent_and_logged_once_it_sends() {
    let calls = Calls::default();
    let store = MemoryStore::default();
    let near = super::background::NEAR;
    let provider = documented(vec![
        answer("Hello.", near),
        compaction(SUMMARY),
        answer("Again.", 300),
    ]);
    let agent = heard_by(
        background(&provider, &store),
        &rewriters(),
        rewriting(),
        &calls,
    )
    .await;
    let mut live = Live::start(agent);
    assert!(completed(&live.turn("u1", "first").await));
    live.until(prepared).await;
    assert!(completed(&live.turn("u2", "second").await));
    let (_, seen) = live.close().await;
    let text = provider.body(1).to_string();
    assert!(
        text.contains("Summarise briefly.\\nKeep the plan."),
        "{text}"
    );
    assert_eq!(instructions_by(&store), ["aa/one", "aa/two"]);
    let logged = log_events(&store);
    let hooked = logged
        .iter()
        .position(|e| matches!(e, LogEvent::HookApplied { point, .. } if point == "before-compact"))
        .unwrap();
    let ran_two = logged
        .iter()
        .rposition(|e| matches!(e, LogEvent::HookRan { hook, .. } if hook == "aa/two"))
        .unwrap();
    assert!(ran_two < hooked);
    assert_eq!(applied(&store).len(), 2);
    let started = position(&seen, background_started);
    let applied_event = position(&seen, |e| matches!(e, AgentEvent::HookApplied { .. }));
    assert!(applied_event < started);
}

#[tokio::test]
async fn a_background_rewrite_that_never_sends_logs_no_change() {
    let calls = Calls::default();
    let store = MemoryStore::default();
    let near = super::background::NEAR;
    let provider = documented(vec![answer("Hello.", near)]);
    let body = rewriters() + &hook_body("stop", "before-compact", "after = [\"aa/two\"]\n");
    let script = replying(vec![
        ("one", Reply::Rewrite("Summarise briefly.".into())),
        ("stop", Reply::Block("not now".into())),
    ]);
    let agent = heard_by(background(&provider, &store), &body, script, &calls).await;
    let mut live = Live::start(agent);
    assert!(completed(&live.turn("u1", "first").await));
    live.until(ran_at_hook("aa/stop")).await;
    live.close().await;
    assert!(applied(&store).is_empty());
    assert_eq!(provider.sent().len(), 1);
}

fn ran_at_hook(hook: &'static str) -> impl Fn(&AgentEvent) -> bool {
    move |e| matches!(e, AgentEvent::HookRan { hook: h, .. } if h == hook)
}

fn blocks_then_holds(gate: Arc<Notify>, started: Arc<Notify>) -> HookScript {
    Arc::new(move |_, _, event| {
        let gate = gate.clone();
        let started = started.clone();
        Box::pin(async move {
            match event {
                Event::BeforeCompact(info) if info.turn == Some(1) => {
                    Reply::Block("not now".into())
                }
                _ => {
                    started.notify_one();
                    gate.notified().await;
                    Reply::None
                }
            }
        })
    })
}

#[tokio::test]
async fn a_rewind_run_during_the_background_gate_starts_no_precompute() {
    let calls = Calls::default();
    let store = MemoryStore::default();
    let host = Arc::new(Held {
        gate: Semaphore::new(0),
    });
    let reply = Arc::new(Semaphore::new(0));
    let gate = Arc::new(Notify::new());
    let started = Arc::new(Notify::new());
    let near = super::background::NEAR;
    let provider = documented(vec![
        answer("One.", near),
        answer("Two.", near).gated(&reply),
        compaction(SUMMARY),
    ]);
    let script = blocks_then_holds(gate.clone(), started.clone());
    let agent = background(&provider, &store).with_settings_host(host.clone());
    let agent = heard_by(agent, &gate_body(""), script, &calls).await;
    let mut live = Live::start(agent);
    assert!(completed(&live.turn("u1", "first").await));
    live.send(send("u2", "second")).await;
    live.until(|e| matches!(e, AgentEvent::StepStarted { .. }))
        .await;
    live.send(ClientCommand::ChangeSettings {
        request: RequestId(9),
        changes: vec![
            wisp_protocol::settings::SettingChange::named(
                "effort",
                SettingValue::Text("low".into()),
            )
            .unwrap(),
        ],
        save: true,
    })
    .await;
    live.send(ClientCommand::Rewind {
        to: wisp_protocol::RewindPoint::Turn { turn: 2 },
        request: RequestId(4),
    })
    .await;
    reply.add_permits(1);
    within("the hook's start", started.notified()).await;
    host.gate.add_permits(4);
    live.until(|e| matches!(e, AgentEvent::Rewound { .. }))
        .await;
    gate.notify_one();
    live.until(|e| matches!(e, AgentEvent::HookRan { turn: Some(2), .. }))
        .await;
    tokio::time::sleep(Duration::from_millis(100)).await;
    let (_, seen) = live.close().await;
    assert!(!seen.iter().any(background_started), "{seen:#?}");
    assert_eq!(provider.sent().len(), 2);
}

#[tokio::test]
async fn a_long_hooked_instruction_is_counted_in_the_earlier_request_it_picks() {
    let store = MemoryStore::default();
    let provider = scripted(three_answers());
    let mut live = Live::start(off(&provider, &store));
    three_turns(&mut live).await;
    let (agent, _) = live.close().await;
    let default = agent.instruction_estimate();
    let room = u64::from(agent.compaction_max_tokens());
    let long = 40_000;
    let limit = 100_000 + room + default + 1;
    let plain = agent.earlier_request(limit, usize::MAX).unwrap();
    let hooked = agent.earlier_with(limit, usize::MAX, long).unwrap();
    assert!(hooked.index < plain.index, "{hooked:?} {plain:?}");
}

fn blocking_turn(turn: u64) -> HookScript {
    Arc::new(move |_, hook, event| {
        let reply = match event {
            Event::BeforeCompact(info) if hook == "gate" && info.turn == Some(turn) => {
                Reply::Block("not now".into())
            }
            _ => Reply::None,
        };
        Box::pin(async move { reply })
    })
}

const SMALL_WINDOW: &str = "[models.\"claude-opus-5-5\"]\ncontext_window = 104000\n";

async fn retried(live: &mut Live, turn: u64, endpoint: Option<&str>) -> TurnOutcome {
    live.send(ClientCommand::Retry {
        turn,
        endpoint: endpoint.map(str::to_owned),
        request: RequestId(9),
    })
    .await;
    match live
        .until(|e| matches!(e, AgentEvent::TurnEnded { .. }))
        .await
    {
        AgentEvent::TurnEnded { outcome, .. } => outcome,
        _ => unreachable!(),
    }
}

async fn rerun(
    body: &str,
    script: HookScript,
    calls: &Calls,
) -> (MemoryStore, Arc<Fake>, TurnOutcome) {
    let store = MemoryStore::default();
    let provider = scripted(vec![
        answer("Hello.", 100_000),
        server_failed(),
        compaction(SUMMARY),
        answer("Retried.", 2_000),
    ]);
    let agent = no_retries(on(&provider, &store, SMALL_WINDOW));
    let agent = heard_by(agent, body, script, calls).await;
    let mut live = Live::start(agent);
    assert!(completed(&live.turn("u1", "first").await));
    let failed = live.turn("u2", "second").await;
    assert!(
        matches!(failed, TurnOutcome::Error { offer: Some(_), .. }),
        "{failed:?}"
    );
    let outcome = retried(&mut live, 2, None).await;
    live.close().await;
    (store, provider, outcome)
}

#[tokio::test]
async fn a_vetoed_rerun_on_a_retry_sends_the_turn_without_compacting() {
    let calls = Calls::default();
    let (store, provider, outcome) = rerun(&gate_body(""), blocking_turn(3), &calls).await;
    assert!(completed(&outcome), "{outcome:?}");
    assert_eq!(
        blocked_records(&store),
        [(CompactionTrigger::Auto, NOT_NOW.to_owned())]
    );
    assert_eq!(provider.sent().len(), 3);
    assert!(provider.body(2)["messages"].to_string().contains("second"));
    assert_ne!(
        provider.body(2)["messages"][0]["content"][0]["type"],
        "compaction"
    );
}

#[tokio::test]
async fn a_retry_that_reruns_its_compaction_calls_both_points_again() {
    let calls = Calls::default();
    let body = gate_body("") + &hook_body("head", "compaction-head", "");
    let (store, provider, outcome) = rerun(&body, kit_none(), &calls).await;
    assert!(completed(&outcome), "{outcome:?}");
    assert_eq!(provider.sent().len(), 4);
    assert_eq!(calls.count("gate"), 2);
    assert_eq!(calls.count("head"), 1);
    let turns: Vec<Option<u64>> = infos(&calls, "gate").iter().map(|i| i.turn).collect();
    assert_eq!(turns, [Some(2), Some(3)]);
    assert_eq!(compacted_triggers(&store), [CompactionTrigger::Auto]);
}

#[tokio::test]
async fn a_switching_retry_whose_compaction_is_vetoed_converts_and_resends() {
    let calls = Calls::default();
    let store = MemoryStore::default();
    let provider = scripted(vec![
        answer("Hello.", 1_000),
        compaction(SUMMARY),
        answer("Second.", 2_000),
        failing(server_error()),
        answer("Retried.", 1_000),
    ]);
    let agent = heard_by(
        windows(&store, &provider, WINDOWS_NO_NATIVE),
        &gate_body(""),
        blocking_trigger("switch"),
        &calls,
    )
    .await;
    let mut live = Live::start(agent);
    live.turn("u1", "first").await;
    live.compact(2, None).await;
    assert!(completed(&live.turn("u2", "second").await));
    let failed = live.turn("u3", "third").await;
    assert!(
        matches!(failed, TurnOutcome::Error { offer: Some(_), .. }),
        "{failed:?}"
    );
    let outcome = retried(&mut live, 3, Some("other")).await;
    let (agent, seen) = live.close().await;
    assert!(completed(&outcome), "{outcome:?}");
    assert_eq!(
        blocked_records(&store),
        [(CompactionTrigger::Switch, NOT_NOW.to_owned())]
    );
    assert!(seen.iter().any(blocked_event(CompactionTrigger::Switch)));
    assert_eq!(provider.sent().len(), 5);
    assert_eq!(agent.endpoint_name(), "other");
    let resent = provider.body(4).to_string();
    assert!(
        resent.contains("compacted_context") || resent.contains(SUMMARY),
        "{resent}"
    );
}

#[tokio::test]
async fn compact_info_gives_each_triggers_estimate_point_and_epoch() {
    let calls = Calls::default();
    let store = MemoryStore::default();
    let mut steps = vec![
        answer("Hello.", 1_000),
        compaction(SUMMARY),
        answer("Two.", 3_000),
    ];
    steps.push(too_long(TOO_LONG));
    steps.push(compaction(SUMMARY));
    steps.push(answer("Three.", 900));
    let provider = scripted(steps);
    let agent = heard_by(off(&provider, &store), &gate_body(""), kit_none(), &calls).await;
    let mut live = Live::start(agent);
    live.turn("u1", "first").await;
    live.compact(2, None).await;
    live.turn("u2", "second").await;
    live.turn("u3", "third").await;
    let (agent, _) = live.close().await;
    let seen = infos(&calls, "gate");
    let rows: Vec<(&str, u64, Option<u64>)> = seen
        .iter()
        .map(|i| (i.trigger.as_str(), i.epoch, i.point))
        .collect();
    let point = agent.window_and_point().1;
    assert_eq!(
        rows,
        [("manual", 0, Some(ceiling())), ("overflow", 1, point)]
    );
    assert!(seen[0].estimate >= 1_000, "{:?}", seen[0]);
    assert!(seen[1].estimate >= 3_000, "{:?}", seen[1]);
}

pub(super) async fn switch_aa(live: &mut Live, key: &str, request: u64) {
    live.send(crate::plugin::testing::plugin_switch(
        request, "aa", key, false,
    ))
    .await;
    let done = live
        .until(|e| {
            matches!(
                e,
                AgentEvent::SettingsChanged { answers: Some(RequestId(r)), .. }
                    | AgentEvent::Rejected { request: Some(RequestId(r)), .. }
                    if *r == request
            )
        })
        .await;
    assert!(
        matches!(done, AgentEvent::SettingsChanged { .. }),
        "{done:?}"
    );
}

#[tokio::test]
async fn a_plugins_veto_stops_from_the_turn_after_it_is_switched_off_or_removed() {
    use wisp_protocol::settings::{ENABLED_KEY, INSTALLED_KEY};
    for key in [ENABLED_KEY, INSTALLED_KEY] {
        let calls = Calls::default();
        let store = MemoryStore::default();
        let provider = scripted(vec![
            answer("Hello.", 5_000),
            answer("Two.", 5_100),
            compaction(SUMMARY),
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
        assert!(completed(&live.turn("u1", "first").await));
        assert!(completed(&live.turn("u2", "second").await));
        switch_aa(&mut live, key, 81).await;
        assert!(completed(&live.turn("u3", "third").await));
        live.close().await;
        assert_eq!(calls.count("gate"), 1, "{key}");
        assert_eq!(
            blocked_records(&store),
            [(CompactionTrigger::Auto, NOT_NOW.to_owned())],
            "{key}"
        );
        assert_eq!(
            compacted_triggers(&store),
            [CompactionTrigger::Auto],
            "{key}"
        );
        assert_eq!(provider.sent().len(), 4, "{key}");
    }
}

#[tokio::test]
async fn a_compact_after_an_idle_switch_off_runs_none_of_its_before_compact_hooks() {
    use wisp_protocol::settings::ENABLED_KEY;
    let calls = Calls::default();
    let store = MemoryStore::default();
    let provider = scripted(vec![answer("Hello.", 1_000), compaction(SUMMARY)]);
    let agent = heard_by(
        logged(provider.clone(), &store),
        &gate_body(""),
        blocking(),
        &calls,
    )
    .await;
    let mut live = Live::start(agent);
    assert!(completed(&live.turn("u1", "first").await));
    switch_aa(&mut live, ENABLED_KEY, 81).await;
    let done = live.compact(2, None).await;
    assert!(matches!(done, AgentEvent::Compacted { .. }), "{done:?}");
    live.close().await;
    assert_eq!(calls.count("gate"), 0);
    assert!(blocked_records(&store).is_empty());
    assert_eq!(compacted_triggers(&store), [CompactionTrigger::Manual]);
}

async fn overflow_compaction_source(rewrite: Option<String>) -> Arc<Fake> {
    let probe = Agent::new(scripted(Vec::new()), default_session());
    let room = u64::from(probe.compaction_max_tokens());
    let limit = 100_000 + room + crate::context::text_tokens(&summary_instruction(None)) + 1;
    let stated = format!("prompt is too long: 250000 tokens > {limit} maximum");
    let calls = Calls::default();
    let store = MemoryStore::default();
    let mut steps = three_answers();
    steps.extend([too_long(&stated), compaction(SUMMARY), answer("Four.", 900)]);
    let provider = scripted(steps);
    let script = match rewrite {
        Some(text) => replying(vec![("gate", Reply::Rewrite(text))]),
        None => kit_none(),
    };
    let agent = heard_by(off(&provider, &store), &gate_body(""), script, &calls).await;
    let mut live = Live::start(agent);
    three_turns(&mut live).await;
    let outcome = live.turn("u4", "fourth").await;
    assert!(completed(&outcome), "{outcome:?}");
    live.close().await;
    assert_eq!(calls.count("gate"), 1);
    provider
}

#[tokio::test]
async fn an_overflow_with_a_long_rewritten_instruction_compacts_an_earlier_request_that_fits_it() {
    let plain = overflow_compaction_source(None).await;
    let sent = plain.sent();
    assert_eq!(
        strip_cache_control(&sent[4].body()["messages"]),
        strip_cache_control(&sent[1].body()["messages"])
    );
    let long = "Summarise in full detail. ".repeat(600);
    let hooked = overflow_compaction_source(Some(long.clone())).await;
    let sent = hooked.sent();
    assert_eq!(
        strip_cache_control(&sent[4].body()["messages"]),
        strip_cache_control(&sent[0].body()["messages"])
    );
    assert!(sent[4].body().to_string().contains(long.trim_end()));
}

fn blocks_once_then_rewrites() -> HookScript {
    let calls = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    Arc::new(move |_, _, _| {
        let first = calls.fetch_add(1, std::sync::atomic::Ordering::SeqCst) == 0;
        let reply = if first {
            Reply::Block("not now".into())
        } else {
            Reply::Rewrite("Summarise briefly.".into())
        };
        Box::pin(async move { reply })
    })
}

#[tokio::test]
async fn a_background_start_the_layout_check_refuses_writes_none_of_its_hooks_changes() {
    let calls = Calls::default();
    let store = MemoryStore::default();
    let near = super::background::NEAR;
    let provider = documented(vec![
        answer("Hello.", 1_000),
        answer("Again.", near),
        compaction(SUMMARY),
    ]);
    let agent = heard_by(
        background(&provider, &store),
        &gate_body(""),
        blocks_once_then_rewrites(),
        &calls,
    )
    .await;
    let mut live = Live::start(agent);
    assert!(completed(&live.turn("u1", "first").await));
    let outcome = live.turn("u2", "second").await;
    let (mut agent, _) = live.close().await;
    assert_eq!(calls.count("gate"), 1);
    agent.history[1].role = Role::User;
    agent.sync_history();
    let (sink, mut events) = mpsc::channel(64);
    agent.background_after_turn(&outcome, &sink).await;
    drop(sink);
    let mut seen = Vec::new();
    while let Some(event) = events.recv().await {
        seen.push(event);
    }
    assert_eq!(calls.count("gate"), 2);
    assert!(
        !seen
            .iter()
            .any(|e| matches!(e, AgentEvent::HookApplied { .. })),
        "{seen:#?}"
    );
    assert!(!seen.iter().any(background_started), "{seen:#?}");
    assert!(!agent.compaction.background.running());
    assert_eq!(provider.sent().len(), 2);
}

#[tokio::test]
async fn a_point_raised_during_the_background_gate_starts_no_precompute() {
    let calls = Calls::default();
    let store = MemoryStore::default();
    let host = Arc::new(Held {
        gate: Semaphore::new(0),
    });
    let reply = Arc::new(Semaphore::new(0));
    let gate = Arc::new(Notify::new());
    let started = Arc::new(Notify::new());
    let near = super::background::NEAR;
    let provider = documented(vec![
        answer("One.", near),
        answer("Two.", near).gated(&reply),
        compaction(SUMMARY),
    ]);
    let script = blocks_then_holds(gate.clone(), started.clone());
    let agent = background(&provider, &store).with_settings_host(host.clone());
    let agent = heard_by(agent, &gate_body(""), script, &calls).await;
    let mut live = Live::start(agent);
    assert!(completed(&live.turn("u1", "first").await));
    live.send(send("u2", "second")).await;
    live.until(|e| matches!(e, AgentEvent::StepStarted { .. }))
        .await;
    live.send(ClientCommand::ChangeSettings {
        request: RequestId(9),
        changes: vec![
            wisp_protocol::settings::SettingChange::named(
                "effort",
                SettingValue::Text("low".into()),
            )
            .unwrap(),
        ],
        save: true,
    })
    .await;
    live.send(ClientCommand::ChangeSettings {
        request: RequestId(10),
        changes: vec![
            wisp_protocol::settings::SettingChange::named(
                "compact_at",
                SettingValue::Tokens(150_000),
            )
            .unwrap(),
        ],
        save: false,
    })
    .await;
    reply.add_permits(1);
    within("the hook's start", started.notified()).await;
    host.gate.add_permits(4);
    live.until(|e| {
        matches!(
            e,
            AgentEvent::SettingsChanged {
                answers: Some(RequestId(10)),
                ..
            }
        )
    })
    .await;
    gate.notify_one();
    live.until(|e| matches!(e, AgentEvent::HookRan { turn: Some(2), .. }))
        .await;
    tokio::time::sleep(Duration::from_millis(100)).await;
    let (_, seen) = live.close().await;
    assert_eq!(calls.count("gate"), 2);
    assert!(!seen.iter().any(background_started), "{seen:#?}");
    assert_eq!(provider.sent().len(), 2);
}

#[tokio::test]
async fn compact_info_gives_an_automatic_compactions_estimate_with_the_turns_message() {
    let calls = Calls::default();
    let store = MemoryStore::default();
    let provider = scripted(vec![
        answer("Hello.", 5_000),
        compaction(SUMMARY),
        answer("Two.", 300),
    ]);
    let agent = heard_by(
        on(&provider, &store, LOW),
        &gate_body(""),
        kit_none(),
        &calls,
    )
    .await;
    let mut live = Live::start(agent);
    assert!(completed(&live.turn("u1", "first").await));
    let (agent, _) = live.close().await;
    let typed = "q".repeat(8_000);
    let expected = agent.message_estimate(
        &agent.provisional_head(),
        &[ContentBlock::Text {
            text: typed.clone(),
        }],
    );
    let fill = agent.fill_estimate();
    let mut live = Live::start(agent);
    assert!(completed(&live.turn("u2", &typed).await));
    live.close().await;
    let seen = infos(&calls, "gate");
    assert_eq!(seen.len(), 1);
    assert_eq!(seen[0].trigger, "auto");
    assert_eq!(seen[0].estimate, expected);
    assert!(
        seen[0].estimate > fill + 1_000,
        "{} {fill}",
        seen[0].estimate
    );
}

fn blocks_once() -> HookScript {
    let calls = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    Arc::new(move |_, _, _| {
        let first = calls.fetch_add(1, std::sync::atomic::Ordering::SeqCst) == 0;
        let reply = if first {
            Reply::Block("not now".into())
        } else {
            Reply::None
        };
        Box::pin(async move { reply })
    })
}

#[tokio::test]
async fn a_summarise_that_converts_runs_the_discard_it_was_handed_at_once() {
    let calls = Calls::default();
    let store = MemoryStore::default();
    let precompute = Arc::new(Semaphore::new(0));
    let provider = documented(vec![
        answer("Hello.", super::background::NEAR),
        compaction(SUMMARY).gated(&precompute),
    ]);
    let agent = heard_by(
        background(&provider, &store),
        &gate_body(""),
        blocks_once(),
        &calls,
    )
    .await;
    let mut live = Live::start(agent);
    let outcome = live.turn("u1", "first").await;
    assert!(completed(&outcome));
    let (agent, _) = live.close().await;
    let mut agent = agent;
    let (sink, mut events) = mpsc::channel(64);
    agent.background_after_turn(&outcome, &sink).await;
    assert!(agent.compaction.background.running());
    let mut agent = agent.with_model_settings(models(
        "[models.\"claude-opus-5-5\"]\nnative_compaction = false\n",
    ));
    agent.compaction.head = Some(Head {
        epoch: 1,
        from: 0,
        message: Message {
            role: Role::User,
            content: vec![ContentBlock::opaque("anthropic", raw_block(SUMMARY))],
        },
        summary: SUMMARY.into(),
        native: true,
        turns: (1, 1),
        cut: 0,
        placement: Placement::Head,
        additions: Vec::new(),
        join: None,
    });
    let (_commands, mut inbox) = mpsc::channel(4);
    let gate = agent
        .gate(CompactionTrigger::Manual)
        .discarding(crate::agent::compact::gate::Discard::Any(None));
    let gate = agent.hand(gate, &sink).await;
    assert!(agent.compaction.background.running());
    let outcome = agent
        .summarise((gate, None), true, Fit::View, &mut inbox, &sink)
        .await;
    drop(sink);
    let mut seen = Vec::new();
    while let Some(event) = events.recv().await {
        seen.push(event);
    }
    assert!(matches!(outcome, Outcome::Convert));
    assert_eq!(calls.count("gate"), 2);
    assert!(!agent.compaction.background.running());
    assert_eq!(super::background::discarded(&seen).len(), 1, "{seen:#?}");
}

fn held_at(trigger: &'static str, gate: Arc<Notify>, started: Arc<Notify>) -> HookScript {
    Arc::new(move |_, _, event| {
        let gate = gate.clone();
        let started = started.clone();
        let holds = matches!(&event, Event::BeforeCompact(info) if info.trigger == trigger);
        Box::pin(async move {
            if holds {
                started.notify_one();
                gate.notified().await;
            }
            Reply::None
        })
    })
}

async fn cancelled_turn(live: &mut Live, started: &Notify) -> AgentEvent {
    within("the hook's start", started.notified()).await;
    live.send(ClientCommand::Cancel).await;
    live.until(|e| matches!(e, AgentEvent::TurnEnded { .. }))
        .await
}

fn ended_cancelled(ended: &AgentEvent) -> bool {
    matches!(
        ended,
        AgentEvent::TurnEnded {
            outcome: TurnOutcome::Cancelled { .. },
            ..
        }
    )
}

#[tokio::test]
async fn esc_during_the_gate_of_an_overflow_compaction_ends_the_turn_cancelled() {
    let calls = Calls::default();
    let store = MemoryStore::default();
    let gate = Arc::new(Notify::new());
    let started = Arc::new(Notify::new());
    let mut steps = three_answers();
    steps.push(too_long(TOO_LONG));
    let provider = scripted(steps);
    let script = held_at("overflow", gate.clone(), started.clone());
    let agent = heard_by(off(&provider, &store), &gate_body(""), script, &calls).await;
    let mut live = Live::start(agent);
    three_turns(&mut live).await;
    live.send(send("u4", "fourth")).await;
    let ended = cancelled_turn(&mut live, &started).await;
    live.close().await;
    assert!(ended_cancelled(&ended), "{ended:?}");
    assert_eq!(provider.sent().len(), 4);
    assert_eq!(ran(&store, GATE), [HookOutcome::Cancelled]);
}

#[tokio::test]
async fn esc_during_the_gate_of_a_switch_compaction_ends_the_turn_cancelled() {
    let calls = Calls::default();
    let store = MemoryStore::default();
    let gate = Arc::new(Notify::new());
    let started = Arc::new(Notify::new());
    let provider = scripted(vec![
        answer("Hello.", 1_000),
        compaction(SUMMARY),
        answer("Second.", 300_000),
    ]);
    let script = held_at("switch", gate.clone(), started.clone());
    let agent = heard_by(
        windows(&store, &provider, WINDOWS_NO_NATIVE),
        &gate_body(""),
        script,
        &calls,
    )
    .await;
    let mut live = Live::start(agent);
    live.turn("u1", "first").await;
    live.compact(2, None).await;
    assert!(completed(&live.turn("u2", "second").await));
    change(&mut live, 3, Some("other"), None).await;
    live.send(send("u3", "third")).await;
    let ended = cancelled_turn(&mut live, &started).await;
    live.close().await;
    assert!(ended_cancelled(&ended), "{ended:?}");
    assert_eq!(provider.sent().len(), 3);
    assert_eq!(ran(&store, GATE).last(), Some(&HookOutcome::Cancelled));
}

#[tokio::test]
async fn a_turn_cancelled_by_esc_asks_no_background_gate() {
    let calls = Calls::default();
    let store = MemoryStore::default();
    let reply = Arc::new(Semaphore::new(0));
    let provider = documented(vec![
        answer("Hello.", 1_000),
        answer("Again.", super::background::NEAR).gated(&reply),
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
    live.send(send("u2", "second")).await;
    live.until(|e| matches!(e, AgentEvent::StepStarted { .. }))
        .await;
    live.send(ClientCommand::Cancel).await;
    let ended = live
        .until(|e| matches!(e, AgentEvent::TurnEnded { .. }))
        .await;
    tokio::time::sleep(Duration::from_millis(50)).await;
    let (_, seen) = live.close().await;
    assert!(ended_cancelled(&ended), "{ended:?}");
    assert_eq!(calls.count("gate"), 0);
    assert!(!seen.iter().any(background_started), "{seen:#?}");
}

#[tokio::test]
async fn compact_info_gives_the_switch_and_background_triggers() {
    let calls = Calls::default();
    let store = MemoryStore::default();
    let provider = documented(vec![
        answer("Hello.", super::background::NEAR),
        compaction(SUMMARY),
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
    let (agent, _) = live.close().await;
    let seen = infos(&calls, "gate");
    let rows: Vec<(&str, Option<u64>, u64, Option<u64>)> = seen
        .iter()
        .map(|i| (i.trigger.as_str(), i.turn, i.epoch, i.point))
        .collect();
    assert_eq!(
        rows,
        [("background", Some(1), 0, agent.next_window_and_point().1)]
    );
    assert_eq!(seen[0].estimate, agent.fill_estimate());

    let calls = Calls::default();
    let store = MemoryStore::default();
    let provider = scripted(vec![
        answer("Hello.", 1_000),
        compaction(SUMMARY),
        answer("Second.", 300_000),
        compaction(SUMMARY),
        answer("Third.", 2_000),
    ]);
    let agent = heard_by(
        windows(&store, &provider, WINDOWS_NO_NATIVE),
        &gate_body(""),
        kit_none(),
        &calls,
    )
    .await;
    let mut live = Live::start(agent);
    live.turn("u1", "first").await;
    live.compact(2, None).await;
    assert!(completed(&live.turn("u2", "second").await));
    change(&mut live, 3, Some("other"), None).await;
    live.turn("u3", "third").await;
    live.close().await;
    let seen = infos(&calls, "gate");
    let rows: Vec<(&str, Option<u64>, u64)> = seen
        .iter()
        .map(|i| (i.trigger.as_str(), i.turn, i.epoch))
        .collect();
    assert_eq!(rows, [("manual", None, 0), ("switch", Some(3), 1)]);
    assert!(seen[1].estimate >= 300_000, "{:?}", seen[1]);
}
