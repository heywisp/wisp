use tokio::sync::Notify;
use wisp_plugin_api::hooks::{Event, FillInfo, Reply};
use wisp_plugin_testkit::hooks::{Calls, Script as HookScript};
use wisp_protocol::HookOutcome;

use super::background::{NEAR, background, swapped_in, tool_call};
use super::gate::{blocked_records, ceiling, infos, ran};

use super::hooks::heard_by;
use super::*;
use crate::agent::TurnTrack;
use crate::agent::testing::restamp;
use crate::agent::tests::{gated, held};
use crate::plugin::testing::hook_body;
use crate::test_guard::records;

pub(super) const ASK: &str = "aa/ask";
const LATER: &str = "aa/later";
const GATE: &str = "aa/gate";
pub(super) const KEEP: &str = "Keep the plan.";
const LOW: &str = "[models.\"claude-opus-5-5\"]\ncompact_at = 2000\n";
const TINY: &str = "[models.\"claude-opus-5-5\"]\ncompact_at = 10\n";
const WISP_LOW: &str =
    "[models.\"claude-opus-5-5\"]\ncompact_at = 2000\nnative_compaction = false\n";
const SHORT: &str = "A short summary.";

pub(super) fn ask_body(extra: &str) -> String {
    hook_body("ask", "context-threshold", extra)
}

fn ask_then_later() -> String {
    ask_body("") + &hook_body("later", "context-threshold", "after = [\"aa/ask\"]\n")
}

fn gate_body() -> String {
    hook_body("gate", "before-compact", "")
}

pub(super) fn answering(
    answer: impl Fn(&str, &Event) -> Reply + Send + Sync + 'static,
) -> HookScript {
    Arc::new(move |_, hook, event| {
        let reply = answer(&hook, &event);
        Box::pin(async move { reply })
    })
}

pub(super) fn asking(when: fn(&FillInfo) -> bool, focus: Option<&'static str>) -> HookScript {
    answering(move |hook, event| match event {
        Event::ContextThreshold(fill) if hook == "ask" && when(fill) => {
            Reply::Compact(focus.map(str::to_owned))
        }
        _ => Reply::None,
    })
}

fn always(_: &FillInfo) -> bool {
    true
}

fn on_turn_two(fill: &FillInfo) -> bool {
    fill.turn == 2
}

fn at_a_boundary(fill: &FillInfo) -> bool {
    !fill.before_turn
}

pub(super) fn fills(calls: &Calls, hook: &str) -> Vec<FillInfo> {
    calls
        .of(hook)
        .into_iter()
        .filter_map(|c| match c.event {
            Event::ContextThreshold(fill) => Some(fill),
            _ => None,
        })
        .collect()
}

pub(super) fn requests(store: &MemoryStore) -> Vec<(Option<String>, Option<String>)> {
    pick!(
        log_events(store),
        LogEvent::ContextCompacted {
            focus,
            requested_by,
            ..
        } => (focus, requested_by)
    )
}

fn failed_requests(store: &MemoryStore) -> Vec<(CompactionFailure, Option<String>)> {
    pick!(
        log_events(store),
        LogEvent::CompactionFailed {
            kind, requested_by, ..
        } => (kind, requested_by)
    )
}

fn started_by(seen: &[AgentEvent]) -> Vec<Option<String>> {
    pick!(
        seen,
        AgentEvent::CompactionStarted {
            trigger: CompactionTrigger::Auto,
            requested_by,
            ..
        } => requested_by.clone()
    )
}

fn paused_events(seen: &[AgentEvent]) -> usize {
    seen.iter()
        .filter(|e| {
            matches!(
                e,
                AgentEvent::Compacted { paused: true, .. }
                    | AgentEvent::CompactionFailed { paused: true, .. }
            )
        })
        .count()
}

fn request_lines(seen: &[AgentEvent]) -> Vec<String> {
    seen.iter()
        .filter_map(|e| match e {
            AgentEvent::CompactionStarted {
                requested_by: Some(hook),
                turn,
                ..
            } => Some(format!("started {hook} {turn:?}")),
            AgentEvent::CompactionFailed {
                requested_by: Some(hook),
                kind,
                paused,
                turn,
                ..
            } => Some(format!("failed {hook} {kind:?} {paused} {turn:?}")),
            _ => None,
        })
        .collect()
}

fn same_on_resume(store: &MemoryStore, seen: &[AgentEvent]) {
    let records = records(store);
    let replayed = crate::resume::transcript(&records, &crate::tool::Toolbox::default(), None);
    let live = request_lines(seen);
    assert_eq!(live.len(), 4, "{live:#?}");
    assert_eq!(request_lines(&replayed), live);
}

fn refused(rule: &str) -> HookOutcome {
    HookOutcome::Rejected { rule: rule.into() }
}

fn places(fills: &[FillInfo]) -> Vec<(u64, u32, bool)> {
    fills
        .iter()
        .map(|f| (f.turn, f.step, f.before_turn))
        .collect()
}

#[tokio::test]
async fn context_threshold_fires_before_a_turn_and_at_each_boundary_with_automatic_compaction_on_or_off()
 {
    for (config, auto_compact) in [("", true), ("", false), (LOW, true), (LOW, false)] {
        let calls = Calls::default();
        let store = MemoryStore::default();
        let provider = scripted(vec![
            answer("Hello.", 1_000),
            tool_call(1_100, "Working."),
            answer("Done.", 1_200),
        ]);
        let ceilings = Arc::new(Mutex::new(Vec::new()));
        let seen = ceilings.clone();
        let script: HookScript = Arc::new(move |host, _, _| {
            seen.lock().unwrap().push(host.current_fill().ceiling);
            Box::pin(async { Reply::None })
        });
        let agent = on(&provider, &store, config).with_auto_compact(auto_compact);
        let agent = heard_by(agent, &ask_body(""), script, &calls).await;
        let mut live = Live::start(agent);
        assert!(completed(&live.turn("u1", "first").await));
        assert!(completed(&live.turn("u2", "second").await));
        let (agent, _) = live.close().await;
        let fills = fills(&calls, "ask");
        assert_eq!(
            places(&fills),
            [(2, 0, true), (2, 1, false)],
            "{config} automatic compaction {auto_compact}"
        );
        let (window, point) = agent.window_and_point();
        for fill in &fills {
            assert_eq!(fill.auto_compact, auto_compact);
            assert_eq!(fill.window, window);
            assert_eq!(fill.point, point, "the point stays uncut");
            assert_eq!(fill.ceiling, Some(ceiling()));
            assert_eq!(fill.epoch, 0);
        }
        assert_eq!(*ceilings.lock().unwrap(), [Some(ceiling()); 2]);
        assert_eq!(fills[0].measured, Some(1_000));
        assert_eq!(fills[1].measured, Some(1_100));
        assert!(fills[1].estimate > 1_100, "{}", fills[1].estimate);
    }
}

#[tokio::test]
async fn a_compact_below_the_point_runs_one_compaction_with_its_focus_and_requested_by() {
    let calls = Calls::default();
    let store = MemoryStore::default();
    let provider = scripted(vec![
        answer("Hello.", 1_000),
        compaction(SUMMARY),
        answer("Two.", 400),
    ]);
    let body = ask_body("")
        + &hook_body("also", "context-threshold", "after = [\"aa/ask\"]\n")
        + &gate_body();
    let script = answering(|hook, event| match event {
        Event::ContextThreshold(_) if hook != "gate" => Reply::Compact(Some(KEEP.into())),
        _ => Reply::None,
    });
    let agent = heard_by(logged(provider.clone(), &store), &body, script, &calls).await;
    let mut live = Live::start(agent);
    assert!(completed(&live.turn("u1", "first").await));
    assert!(completed(&live.turn("u2", "second").await));
    let (_, seen) = live.close().await;
    assert_eq!(provider.sent().len(), 3);
    assert_eq!(requests(&store), [(Some(KEEP.into()), Some(ASK.into()))]);
    assert_eq!(started_by(&seen), [Some(ASK.to_owned())]);
    assert_eq!(ran(&store, ASK), [HookOutcome::Applied]);
    assert_eq!(calls.count("also"), 0);
    assert!(
        provider
            .body(1)
            .to_string()
            .contains("Focus: Keep the plan."),
        "{}",
        provider.body(1)
    );
    let fill = fills(&calls, "ask")[0];
    let info = &infos(&calls, "gate")[0];
    assert_eq!((info.estimate, info.point), (fill.estimate, fill.point));
    assert_eq!(info.trigger, "auto");
    assert_eq!(info.focus.as_deref(), Some(KEEP));
    assert_eq!(info.requested_by.as_deref(), Some(ASK));
    assert!(fill.estimate < fill.point.unwrap());
    assert_eq!(held(&seen), []);
    assert_eq!(paused_events(&seen), 0);
}

#[tokio::test]
async fn a_focus_of_two_thousand_characters_is_kept_and_one_more_is_too_large() {
    for (chars, compacts) in [(2_000, true), (2_001, false)] {
        let calls = Calls::default();
        let store = MemoryStore::default();
        let mut steps = vec![answer("Hello.", 1_000)];
        if compacts {
            steps.push(compaction(SUMMARY));
        }
        steps.push(answer("Two.", 400));
        let provider = scripted(steps);
        let focus = "f".repeat(chars);
        let asked = focus.clone();
        let script = answering(move |_, event| match event {
            Event::ContextThreshold(_) => Reply::Compact(Some(asked.clone())),
            _ => Reply::None,
        });
        let agent = heard_by(
            logged(provider.clone(), &store),
            &ask_body(""),
            script,
            &calls,
        )
        .await;
        let mut live = Live::start(agent);
        assert!(completed(&live.turn("u1", "first").await));
        assert!(completed(&live.turn("u2", "second").await));
        live.close().await;
        if compacts {
            assert_eq!(requests(&store), [(Some(focus), Some(ASK.into()))]);
            assert_eq!(ran(&store, ASK), [HookOutcome::Applied]);
        } else {
            assert_eq!(requests(&store), []);
            assert_eq!(ran(&store, ASK), [refused("too_large")]);
        }
    }
}

#[tokio::test]
async fn when_the_core_is_due_the_focus_rides_its_own_compaction_without_requested_by() {
    let calls = Calls::default();
    let store = MemoryStore::default();
    let provider = scripted(vec![
        answer("Hello.", 5_000),
        compaction(SUMMARY),
        answer("Two.", 400),
    ]);
    let agent = heard_by(
        on(&provider, &store, LOW),
        &ask_body(""),
        asking(on_turn_two, Some(KEEP)),
        &calls,
    )
    .await;
    let mut live = Live::start(agent);
    assert!(completed(&live.turn("u1", "first").await));
    assert!(completed(&live.turn("u2", "second").await));
    let (_, seen) = live.close().await;
    assert_eq!(requests(&store), [(Some(KEEP.into()), None)]);
    assert_eq!(started_by(&seen), [None]);
    assert_eq!(ran(&store, ASK), [HookOutcome::Applied]);
    assert_eq!(provider.sent().len(), 3);
}

#[tokio::test]
async fn under_the_veto_hold_at_the_ceiling_the_focus_rides_the_cores_compaction() {
    let calls = Calls::default();
    let store = MemoryStore::default();
    let ceiling = ceiling();
    let provider = scripted(vec![
        answer("Hello.", 5_000),
        tool_call(ceiling + 1_000, "Working."),
        compaction(SUMMARY),
        answer("Done.", 5_000),
    ]);
    let script = answering(|hook, event| match event {
        Event::ContextThreshold(fill) if hook == "ask" && !fill.before_turn => {
            Reply::Compact(Some(KEEP.into()))
        }
        Event::BeforeCompact(_) => Reply::Block("not now".into()),
        _ => Reply::None,
    });
    let body = ask_body("") + &gate_body();
    let agent = heard_by(on(&provider, &store, LOW), &body, script, &calls).await;
    let mut live = Live::start(agent);
    assert!(completed(&live.turn("u1", "first").await));
    assert!(completed(&live.turn("u2", "second").await));
    let (_, seen) = live.close().await;
    assert_eq!(blocked_records(&store).len(), 1);
    assert_eq!(requests(&store), [(Some(KEEP.into()), None)]);
    assert_eq!(started_by(&seen), [None, None]);
    assert_eq!(
        ran(&store, GATE),
        [
            HookOutcome::Blocked {
                reason: "not now".into()
            },
            refused("ceiling")
        ]
    );
}

#[tokio::test]
async fn a_request_while_a_precompute_runs_takes_its_swap_and_drops_the_focus() {
    let calls = Calls::default();
    let store = MemoryStore::default();
    let precompute = Arc::new(Semaphore::new(0));
    let provider = documented(vec![
        answer("Hello.", NEAR),
        compaction(SUMMARY).gated(&precompute),
        answer("Two.", 300),
    ]);
    let release = precompute.clone();
    let script: HookScript = Arc::new(move |_, _, event| {
        let release = release.clone();
        Box::pin(async move {
            match event {
                Event::ContextThreshold(_) => {
                    release.add_permits(1);
                    Reply::Compact(Some(KEEP.into()))
                }
                _ => Reply::None,
            }
        })
    });
    let agent = heard_by(background(&provider, &store), &ask_body(""), script, &calls).await;
    let mut live = Live::start(agent);
    assert!(completed(&live.turn("u1", "first").await));
    assert!(completed(&live.turn("u2", "second").await));
    let (_, seen) = live.close().await;
    assert!(swapped_in(&seen, 2));
    assert_eq!(started_by(&seen), []);
    assert_eq!(requests(&store), [(None, None)]);
    assert_eq!(ran(&store, ASK), [HookOutcome::Applied]);
    assert_eq!(provider.sent().len(), 3);
}

#[tokio::test]
async fn a_request_at_a_boundary_while_a_precompute_runs_takes_its_swap() {
    let calls = Calls::default();
    let store = MemoryStore::default();
    let precompute = Arc::new(Semaphore::new(0));
    let provider = documented(vec![
        answer("Hello.", NEAR),
        compaction(SUMMARY).gated(&precompute),
        tool_call(NEAR + 200, "Looking."),
        answer("Done.", 400),
    ]);
    let release = precompute.clone();
    let script: HookScript = Arc::new(move |_, _, event| {
        let release = release.clone();
        Box::pin(async move {
            match event {
                Event::ContextThreshold(fill) if at_a_boundary(&fill) => {
                    release.add_permits(1);
                    Reply::Compact(Some(KEEP.into()))
                }
                _ => Reply::None,
            }
        })
    });
    let agent = heard_by(background(&provider, &store), &ask_body(""), script, &calls).await;
    let mut live = Live::start(agent);
    assert!(completed(&live.turn("u1", "first").await));
    assert!(completed(&live.turn("u2", "second").await));
    let (_, seen) = live.close().await;
    assert_eq!(
        seen.iter()
            .filter(|e| matches!(
                e,
                AgentEvent::Compacted {
                    trigger: CompactionTrigger::Background,
                    turn: Some(2),
                    ..
                }
            ))
            .count(),
        1
    );
    assert_eq!(started_by(&seen), []);
    assert_eq!(requests(&store), [(None, None)]);
    assert_eq!(places(&fills(&calls, "ask")), [(2, 0, true), (2, 1, false)]);
    assert_eq!(provider.sent().len(), 4);
}

#[tokio::test]
async fn a_request_whose_precompute_fails_runs_its_own_compaction() {
    let calls = Calls::default();
    let store = MemoryStore::default();
    let precompute = Arc::new(Semaphore::new(0));
    let provider = documented(vec![
        answer("Hello.", NEAR),
        overloaded().gated(&precompute),
        overloaded().gated(&precompute),
        compaction(SUMMARY),
        answer("Two.", 300),
    ]);
    let release = precompute.clone();
    let script: HookScript = Arc::new(move |_, _, event| {
        let release = release.clone();
        Box::pin(async move {
            match event {
                Event::ContextThreshold(_) => {
                    release.add_permits(2);
                    Reply::Compact(Some(KEEP.into()))
                }
                _ => Reply::None,
            }
        })
    });
    let agent = heard_by(background(&provider, &store), &ask_body(""), script, &calls).await;
    let mut live = Live::start(agent);
    assert!(completed(&live.turn("u1", "first").await));
    assert!(completed(&live.turn("u2", "second").await));
    let (_, seen) = live.close().await;
    assert!(!swapped_in(&seen, 2));
    assert_eq!(started_by(&seen), [Some(ASK.to_owned())]);
    assert_eq!(requests(&store), [(Some(KEEP.into()), Some(ASK.into()))]);
    assert_eq!(provider.sent().len(), 5);
}

#[tokio::test]
async fn a_failed_request_refuses_the_rest_of_its_turn_and_the_core_still_compacts_at_its_point() {
    let calls = Calls::default();
    let store = MemoryStore::default();
    let provider = scripted(vec![
        tool_call(1_000, "Working."),
        answer(SHORT, 5),
        tool_call(1_100, "More."),
        tool_call(5_000, "Still."),
        super::wisp::written(WRITTEN, 1_200),
        answer("Done.", 1_300),
        answer(SHORT, 5),
        answer("Two.", 1_400),
    ]);
    let agent = heard_by(
        on(&provider, &store, WISP_LOW),
        &ask_then_later(),
        asking(always, None),
        &calls,
    )
    .await;
    let mut live = Live::start(agent);
    assert!(completed(&live.turn("u1", "first").await));
    assert!(completed(&live.turn("u2", "second").await));
    let (_, seen) = live.close().await;
    assert_eq!(
        ran(&store, ASK),
        [
            HookOutcome::Applied,
            refused("request_failed"),
            HookOutcome::Applied,
            HookOutcome::Applied
        ]
    );
    assert_eq!(ran(&store, LATER), [HookOutcome::None]);
    assert_eq!(requests(&store), [(None, None)]);
    assert_eq!(
        failed_requests(&store),
        [
            (CompactionFailure::CheckFailed, Some(ASK.into())),
            (CompactionFailure::CheckFailed, Some(ASK.into()))
        ]
    );
    let failed: Vec<(bool, Option<String>)> = pick!(
        &seen,
        AgentEvent::CompactionFailed {
            paused,
            requested_by,
            ..
        } => (*paused, requested_by.clone())
    );
    assert_eq!(
        failed,
        [(false, Some(ASK.into())), (false, Some(ASK.into()))]
    );
    assert_eq!(paused_events(&seen), 0);
    same_on_resume(&store, &seen);
    assert_eq!(provider.sent().len(), 8);
}

#[tokio::test]
async fn a_vetoed_request_refuses_the_rest_of_its_turn_and_the_core_still_compacts_at_its_point() {
    let calls = Calls::default();
    let store = MemoryStore::default();
    let provider = scripted(vec![
        tool_call(1_000, "Working."),
        tool_call(1_100, "More."),
        tool_call(5_000, "Still."),
        compaction(SUMMARY),
        answer("Done.", 1_300),
        answer("Two.", 1_400),
    ]);
    let script = answering(|hook, event| match event {
        Event::ContextThreshold(_) if hook == "ask" => Reply::Compact(None),
        Event::BeforeCompact(info) if info.requested_by.is_some() => Reply::Block("not now".into()),
        _ => Reply::None,
    });
    let body = ask_then_later() + &gate_body();
    let agent = heard_by(on(&provider, &store, LOW), &body, script, &calls).await;
    let mut live = Live::start(agent);
    assert!(completed(&live.turn("u1", "first").await));
    assert!(completed(&live.turn("u2", "second").await));
    let (_, seen) = live.close().await;
    assert_eq!(
        ran(&store, ASK),
        [
            HookOutcome::Applied,
            refused("request_blocked"),
            HookOutcome::Applied,
            HookOutcome::Applied
        ]
    );
    assert_eq!(ran(&store, LATER), [HookOutcome::None]);
    assert_eq!(requests(&store), [(None, None)]);
    assert_eq!(
        failed_requests(&store),
        [
            (CompactionFailure::Blocked, Some(ASK.into())),
            (CompactionFailure::Blocked, Some(ASK.into()))
        ]
    );
    assert_eq!(paused_events(&seen), 0);
    same_on_resume(&store, &seen);
    assert_eq!(provider.sent().len(), 6);
}

#[tokio::test]
async fn after_the_cores_own_compaction_fails_or_stays_over_the_point_requests_wait_for_the_next_turn()
 {
    for (config, summary) in [(WISP_LOW, answer(SHORT, 5)), (TINY, compaction(SUMMARY))] {
        let calls = Calls::default();
        let store = MemoryStore::default();
        let next = if config == TINY {
            compaction(SUMMARY)
        } else {
            super::wisp::written(WRITTEN, 1_200)
        };
        let provider = scripted(vec![
            tool_call(5_000, "Working."),
            summary,
            tool_call(5_100, "More."),
            answer("Done.", 5_200),
            next,
            answer("Two.", 1_000),
        ]);
        let agent = heard_by(
            on(&provider, &store, config),
            &ask_then_later(),
            asking(always, None),
            &calls,
        )
        .await;
        let mut live = Live::start(agent);
        assert!(completed(&live.turn("u1", "first").await));
        assert!(completed(&live.turn("u2", "second").await));
        let (_, seen) = live.close().await;
        assert_eq!(
            ran(&store, ASK),
            [
                HookOutcome::Applied,
                refused("auto_paused"),
                HookOutcome::Applied
            ],
            "{config}"
        );
        assert_eq!(ran(&store, LATER), [HookOutcome::None]);
        assert_eq!(paused_events(&seen), 1, "{config}");
        assert_eq!(provider.sent().len(), 6);
    }
}

#[tokio::test]
async fn a_request_runs_under_the_hold() {
    let calls = Calls::default();
    let store = MemoryStore::default();
    let provider = scripted(vec![
        answer("Hello.", 5_000),
        compaction(SUMMARY),
        answer("Two.", 3_500),
        compaction(SUMMARY),
        answer("Three.", 3_600),
    ]);
    let agent = heard_by(
        on(&provider, &store, LOW),
        &ask_body(""),
        asking(|fill| fill.turn == 3, Some(KEEP)),
        &calls,
    )
    .await;
    let mut live = Live::start(agent);
    for (id, text) in [("u1", "first"), ("u2", "second"), ("u3", "third")] {
        assert!(completed(&live.turn(id, text).await));
    }
    let (_, seen) = live.close().await;
    assert_eq!(held(&seen).len(), 1);
    assert_eq!(
        requests(&store),
        [(None, None), (Some(KEEP.into()), Some(ASK.into()))]
    );
    assert_eq!(provider.sent().len(), 5);
}

#[tokio::test]
async fn a_request_runs_under_the_veto_hold_below_the_ceiling() {
    let calls = Calls::default();
    let store = MemoryStore::default();
    let provider = scripted(vec![
        answer("Hello.", 5_000),
        tool_call(5_100, "Working."),
        compaction(SUMMARY),
        answer("Done.", 400),
    ]);
    let script = answering(|hook, event| match event {
        Event::ContextThreshold(fill) if hook == "ask" && !fill.before_turn => Reply::Compact(None),
        Event::BeforeCompact(info) if info.requested_by.is_none() => Reply::Block("not now".into()),
        _ => Reply::None,
    });
    let body = ask_body("") + &gate_body();
    let agent = heard_by(on(&provider, &store, LOW), &body, script, &calls).await;
    let mut live = Live::start(agent);
    assert!(completed(&live.turn("u1", "first").await));
    assert!(completed(&live.turn("u2", "second").await));
    live.close().await;
    assert_eq!(blocked_records(&store).len(), 1);
    assert_eq!(requests(&store), [(None, Some(ASK.into()))]);
    assert_eq!(provider.sent().len(), 4);
}

#[tokio::test]
async fn a_request_at_the_boundary_after_a_swap_that_kept_turns_runs() {
    let calls = Calls::default();
    let store = MemoryStore::default();
    let provider = documented(vec![
        answer("Hello.", NEAR),
        compaction(SUMMARY),
        tool_call(1_000, "Working."),
        compaction(SUMMARY),
        answer("Done.", 400),
    ]);
    let agent = heard_by(
        background(&provider, &store),
        &ask_body(""),
        asking(at_a_boundary, None),
        &calls,
    )
    .await;
    let mut live = Live::start(agent);
    assert!(completed(&live.turn("u1", "first").await));
    live.until(prepared).await;
    assert!(completed(&live.turn("u2", "second").await));
    let (_, seen) = live.close().await;
    assert!(swapped_in(&seen, 2));
    assert_eq!(requests(&store), [(None, None), (None, Some(ASK.into()))]);
    assert_eq!(provider.sent().len(), 5);
}

#[tokio::test]
async fn a_successful_request_over_the_point_refuses_nothing_and_arms_no_hold() {
    let calls = Calls::default();
    let store = MemoryStore::default();
    let provider = scripted(vec![
        tool_call(1_000, "Working."),
        compaction(SUMMARY),
        tool_call(1_100, "More."),
        compaction(SUMMARY),
        answer("Done.", 1_200),
        answer("Two.", 1_300),
    ]);
    let agent = heard_by(
        on(&provider, &store, TINY).with_auto_compact(false),
        &ask_body(""),
        asking(at_a_boundary, None),
        &calls,
    )
    .await;
    let mut live = Live::start(agent);
    assert!(completed(&live.turn("u1", "first").await));
    assert!(completed(&live.turn("u2", "second").await));
    let (_, seen) = live.close().await;
    assert_eq!(
        ran(&store, ASK),
        [
            HookOutcome::Applied,
            HookOutcome::Applied,
            HookOutcome::None
        ]
    );
    assert_eq!(requests(&store).len(), 2);
    assert_eq!(held(&seen), []);
    assert_eq!(paused_events(&seen), 0);
}

#[tokio::test]
async fn a_request_at_the_ceiling_runs_whatever_before_compact_says() {
    let calls = Calls::default();
    let store = MemoryStore::default();
    let ceiling = ceiling();
    let provider = scripted(vec![
        tool_call(ceiling + 1_000, "Working."),
        compaction(SUMMARY),
        answer("Done.", 400),
    ]);
    let script = answering(|hook, event| match event {
        Event::ContextThreshold(_) if hook == "ask" => Reply::Compact(None),
        Event::BeforeCompact(_) => Reply::Block("not now".into()),
        _ => Reply::None,
    });
    let body = ask_body("") + &gate_body();
    let agent = logged(provider.clone(), &store).with_auto_compact(false);
    let agent = heard_by(agent, &body, script, &calls).await;
    let mut live = Live::start(agent);
    assert!(completed(&live.turn("u1", "first").await));
    live.close().await;
    assert_eq!(ran(&store, GATE), [refused("ceiling")]);
    assert_eq!(requests(&store), [(None, Some(ASK.into()))]);
}

#[tokio::test]
async fn a_kept_native_request_logs_its_focus_and_requested_by() {
    let calls = Calls::default();
    let store = MemoryStore::default();
    let mut steps = super::overflow::three_answers();
    steps.extend([
        super::overflow::stopped_compaction(vec![super::overflow::compaction_iteration(
            10_000, 120_000,
        )]),
        compaction(SUMMARY),
        answer("Four.", 400),
    ]);
    let provider = scripted(steps);
    let agent = heard_by(
        super::overflow::off(&provider, &store),
        &ask_body(""),
        asking(|fill| fill.turn == 4, Some(KEEP)),
        &calls,
    )
    .await;
    let mut live = Live::start(agent);
    three_turns(&mut live).await;
    assert!(completed(&live.turn("u4", "fourth").await));
    let (agent, _) = live.close().await;
    assert_eq!(
        agent.compaction.head.as_ref().map(|h| h.placement),
        Some(Placement::Leading)
    );
    let logged: Vec<(Placement, Option<String>, Option<String>)> = pick!(
        log_events(&store),
        LogEvent::ContextCompacted {
            placement,
            focus,
            requested_by,
            ..
        } => (placement, focus, requested_by)
    );
    assert_eq!(
        logged,
        [(Placement::Leading, Some(KEEP.into()), Some(ASK.into()))]
    );
}

#[tokio::test]
async fn a_request_between_steps_reads_the_cached_prefix_and_the_next_request_extends_the_new_one()
{
    let calls = Calls::default();
    let store = MemoryStore::default();
    let provider = scripted(vec![
        tool_call(1_000, "Working."),
        compaction(SUMMARY),
        answer("Done.", 300),
        answer("More.", 400),
    ]);
    let agent = heard_by(
        logged(provider.clone(), &store),
        &ask_body(""),
        asking(at_a_boundary, None),
        &calls,
    )
    .await;
    let mut live = Live::start(agent);
    assert!(completed(&live.turn("u1", "first").await));
    assert!(completed(&live.turn("u2", "second").await));
    live.close().await;
    assert_eq!(requests(&store), [(None, Some(ASK.into()))]);
    super::background::extends(&provider, 0, 1);
    super::background::extends(&provider, 2, 3);
    super::auto::assert_prefix_kept(&provider);
}

#[tokio::test]
async fn esc_during_context_threshold_ends_the_turn_cancelled_with_nothing_sent() {
    for boundary in [false, true] {
        let calls = Calls::default();
        let store = MemoryStore::default();
        let gate = Arc::new(Notify::new());
        let started = Arc::new(Notify::new());
        let mut steps = vec![answer("Hello.", 1_000)];
        if boundary {
            steps = vec![tool_call(1_000, "Working.")];
        }
        let provider = scripted(steps);
        let script = gated(gate.clone(), started.clone());
        let agent = heard_by(
            logged(provider.clone(), &store),
            &ask_body(""),
            script,
            &calls,
        )
        .await;
        let mut live = Live::start(agent);
        if boundary {
            live.send(send("u1", "first")).await;
        } else {
            assert!(completed(&live.turn("u1", "first").await));
            live.send(send("u2", "second")).await;
        }
        within("the hook's start", started.notified()).await;
        live.send(ClientCommand::Cancel).await;
        let ended = live
            .until(|e| matches!(e, AgentEvent::TurnEnded { .. }))
            .await;
        live.close().await;
        assert!(
            matches!(
                ended,
                AgentEvent::TurnEnded {
                    outcome: TurnOutcome::Cancelled { .. },
                    ..
                }
            ),
            "{ended:?}"
        );
        assert_eq!(provider.sent().len(), 1);
        assert_eq!(ran(&store, ASK), [HookOutcome::Cancelled]);
    }
}

#[tokio::test]
async fn context_threshold_is_silent_without_a_window_or_after_every_input_was_blocked() {
    let calls = Calls::default();
    let store = MemoryStore::default();
    let provider = scripted(vec![answer("Hello.", 1_000), answer("Two.", 1_100)]);
    let session = wisp_protocol::SessionConfig {
        model: "claude-unknown-test".to_owned(),
        ..default_session()
    };
    let agent = Agent::new(provider.clone(), session).with_log(
        SessionLog::new(Box::new(store.clone()), "s1"),
        log_context(),
    );
    let agent = heard_by(agent, &ask_body(""), asking(always, None), &calls).await;
    let mut live = Live::start(agent);
    assert!(completed(&live.turn("u1", "first").await));
    assert!(completed(&live.turn("u2", "second").await));
    live.close().await;
    assert_eq!(calls.count("ask"), 0);

    let calls = Calls::default();
    let store = MemoryStore::default();
    let provider = scripted(vec![answer("Hello.", 1_000)]);
    let body = ask_body("") + &hook_body("input", "user-input", "");
    let script = answering(|hook, _| match hook {
        "input" => Reply::Block("no".into()),
        _ => Reply::None,
    });
    let agent = heard_by(logged(provider.clone(), &store), &body, script, &calls).await;
    let live = Live::start(agent);
    live.send(send("u1", "first")).await;
    tokio::time::sleep(Duration::from_millis(200)).await;
    live.close().await;
    assert_eq!(calls.count("input"), 1);
    assert_eq!(calls.count("ask"), 0);
    assert_eq!(provider.sent().len(), 0);
}

#[tokio::test]
async fn a_quote_and_a_stopped_track_fire_nothing() {
    let calls = Calls::default();
    let provider = scripted(Vec::new());
    let agent = Agent::new(provider, default_session()).with_model_settings(models(LOW));
    let mut agent = heard_by(agent, &ask_body(""), asking(always, None), &calls).await;
    agent.history.push(Message {
        role: Role::User,
        content: vec![ContentBlock::Text {
            text: "x".repeat(40_000),
        }],
    });
    assert!(agent.next_message_compaction().is_some());
    let mut track = TurnTrack::new(1);
    track.stopped = true;
    agent.track = Some(track);
    let (_, mut commands) = mpsc::channel(1);
    let (events, _keep) = mpsc::channel(64);
    let asked = agent
        .ask_threshold(50_000, None, false, &mut commands, &events)
        .await;
    assert!(matches!(asked, Ok(None)));
    assert_eq!(calls.count("ask"), 0);
}

#[tokio::test]
async fn a_before_end_continuation_counts_at_the_next_boundary() {
    let calls = Calls::default();
    let store = MemoryStore::default();
    let provider = scripted(vec![answer("Hello.", 1_000), answer("Again.", 1_100)]);
    let long = "word ".repeat(1_900);
    let more = long.clone();
    let script = answering(move |hook, event| match event {
        Event::BeforeEnd(end) if hook == "end" && end.continues == 0 => {
            Reply::Continue(more.clone())
        }
        _ => Reply::None,
    });
    let body = ask_body("") + &hook_body("end", "before-end", "");
    let agent = heard_by(logged(provider.clone(), &store), &body, script, &calls).await;
    let mut live = Live::start(agent);
    assert!(completed(&live.turn("u1", "first").await));
    live.close().await;
    let fills = fills(&calls, "ask");
    assert_eq!(places(&fills), [(1, 1, false)]);
    let counted = crate::context::text_tokens(&long);
    assert!(
        fills[0].estimate >= 1_000 + counted,
        "{} < 1000 + {counted}",
        fills[0].estimate
    );
}

#[tokio::test]
async fn from_skips_a_hook_below_it_and_writes_no_hook_ran() {
    let window = Agent::new(scripted(Vec::new()), default_session())
        .window_and_point()
        .0
        .unwrap();
    for from in [format!("{}", window / 10), "10%".to_owned()] {
        let calls = Calls::default();
        let store = MemoryStore::default();
        let provider = scripted(vec![
            answer("Hello.", window / 20),
            answer("Two.", window / 5),
            answer("Three.", 400),
        ]);
        let from = if from.ends_with('%') {
            format!("from = \"{from}\"\n")
        } else {
            format!("from = {from}\n")
        };
        let agent = heard_by(
            logged(provider.clone(), &store),
            &ask_body(&from),
            asking(|_| false, None),
            &calls,
        )
        .await;
        let mut live = Live::start(agent);
        for (id, text) in [("u1", "first"), ("u2", "second"), ("u3", "third")] {
            assert!(completed(&live.turn(id, text).await));
        }
        live.close().await;
        assert_eq!(places(&fills(&calls, "ask")), [(3, 0, true)], "{from}");
        assert_eq!(ran(&store, ASK), [HookOutcome::None], "{from}");
    }
}

#[tokio::test]
async fn a_wisp_request_logs_its_record_and_focus() {
    let calls = Calls::default();
    let store = MemoryStore::default();
    let provider = scripted(vec![
        answer("Hello.", 1_000),
        super::wisp::written(WRITTEN, 1_200),
        answer("Two.", 400),
    ]);
    let agent = heard_by(
        on(&provider, &store, WISP),
        &ask_body(""),
        asking(on_turn_two, Some(KEEP)),
        &calls,
    )
    .await;
    let mut live = Live::start(agent);
    assert!(completed(&live.turn("u1", "first").await));
    assert!(completed(&live.turn("u2", "second").await));
    live.close().await;
    assert_eq!(requests(&store), [(Some(KEEP.into()), Some(ASK.into()))]);
    assert!(
        provider
            .body(1)
            .to_string()
            .contains("Focus: Keep the plan.")
    );
}

#[tokio::test]
async fn context_threshold_is_silent_at_a_retrys_start_after_a_cut_off_reply_and_with_a_live_server_tail()
 {
    let calls = Calls::default();
    let store = MemoryStore::default();
    let provider = scripted(vec![
        answer("Hello.", 1_000),
        failing(server_error()),
        answer("Retried.", 1_100),
    ]);
    let agent = heard_by(
        super::retry::still(provider.clone(), &store),
        &ask_body(""),
        asking(|_| false, None),
        &calls,
    )
    .await;
    let mut live = Live::start(agent);
    super::retry::failed_second_turn(&mut live).await;
    live.send(super::retry::retry(2, 3)).await;
    live.until(|e| matches!(e, AgentEvent::TurnEnded { .. }))
        .await;
    live.close().await;
    assert_eq!(provider.sent().len(), 3);
    assert_eq!(places(&fills(&calls, "ask")), [(2, 0, true)]);

    let calls = Calls::default();
    let store = MemoryStore::default();
    let provider = scripted(vec![
        answer("Hello.", 1_000),
        super::overflow::cut_off(2_000, "Looking around.", true),
        compaction(SUMMARY),
        answer("Done.", 900),
    ]);
    let agent = heard_by(
        super::overflow::off(&provider, &store),
        &ask_body(""),
        asking(|_| false, None),
        &calls,
    )
    .await;
    let mut live = Live::start(agent);
    assert!(completed(&live.turn("u1", "first").await));
    assert!(completed(&live.turn("u2", "second").await));
    live.close().await;
    assert_eq!(provider.sent().len(), 4);
    assert_eq!(places(&fills(&calls, "ask")), [(2, 0, true)]);

    let calls = Calls::default();
    let store = MemoryStore::default();
    let provider = documented(vec![
        paused_search(1_000, 40),
        super::background::continued_search(1_100),
        answer("Done.", 1_200),
    ]);
    let agent = heard_by(
        logged(provider.clone(), &store),
        &ask_body(""),
        asking(|_| false, None),
        &calls,
    )
    .await;
    let mut live = Live::start(agent);
    assert!(completed(&live.turn("u1", "first").await));
    live.close().await;
    assert_eq!(provider.sent().len(), 3);
    assert_eq!(places(&fills(&calls, "ask")), [(1, 2, false)]);
}

#[tokio::test]
async fn a_hook_asking_before_every_turn_asks_none_from_the_turn_after_its_plugin_is_switched_off_or_removed()
 {
    use wisp_protocol::settings::{ENABLED_KEY, INSTALLED_KEY};
    for key in [ENABLED_KEY, INSTALLED_KEY] {
        let calls = Calls::default();
        let store = MemoryStore::default();
        let provider = scripted(vec![
            answer("Hello.", 1_000),
            compaction(SUMMARY),
            answer("Two.", 400),
            answer("Three.", 500),
        ]);
        let agent = heard_by(
            logged(provider.clone(), &store),
            &ask_body(""),
            asking(always, None),
            &calls,
        )
        .await;
        let mut live = Live::start(agent);
        assert!(completed(&live.turn("u1", "first").await));
        assert!(completed(&live.turn("u2", "second").await));
        super::gate::switch_aa(&mut live, key, 81).await;
        assert!(completed(&live.turn("u3", "third").await));
        live.close().await;
        assert_eq!(calls.count("ask"), 1, "{key}");
        assert_eq!(requests(&store), [(None, Some(ASK.into()))], "{key}");
        assert_eq!(provider.sent().len(), 4, "{key}");
    }
}

#[tokio::test]
async fn requested_compaction_keeps_plan_splits_in_events_logs_and_resumed_totals() {
    use wisp_protocol::Billing;
    let billed = |step: Step, billing| {
        Step::from(restamp(step.script, |usage, iterations| {
            usage.billing = billing;
            for iteration in iterations {
                iteration.usage.billing = billing;
            }
        }))
    };
    for (billing, native, fails) in [
        (Billing::AnthropicPlan, true, false),
        (Billing::AnthropicPlan, true, true),
        (Billing::AnthropicPlan, false, false),
        (Billing::AnthropicPlan, false, true),
        (Billing::ChatGptPlan, false, false),
        (Billing::ChatGptPlan, false, true),
    ] {
        let calls = Calls::default();
        let store = MemoryStore::default();
        let compact = if fails {
            Step::from(vec![
                start(1_200, 0),
                Err(
                    ProviderError::new(ErrorKind::InvalidRequest, "synthetic billed failure")
                        .possibly_billed(),
                ),
            ])
        } else if native {
            compaction(SUMMARY)
        } else {
            super::wisp::written(WRITTEN, 1_200)
        };
        let provider = scripted(vec![
            answer("Hello.", 1_000),
            billed(compact, billing),
            billed(answer("Two.", 400), billing),
        ]);
        let agent = heard_by(
            on(&provider, &store, if native { "" } else { WISP }),
            &ask_body(""),
            asking(on_turn_two, Some(KEEP)),
            &calls,
        )
        .await;
        let mut live = Live::start(agent);
        assert!(completed(&live.turn("u1", "first").await));
        let outcome = live.turn("u2", "second").await;
        assert!(completed(&outcome), "{outcome:?}");
        let (_, seen) = live.close().await;
        let cost =
            seen.iter()
                .find_map(|event| match event {
                    AgentEvent::Compacted { cost, .. }
                    | AgentEvent::CompactionFailed { cost, .. } => *cost,
                    _ => None,
                })
                .unwrap();
        assert_eq!(cost.turn_billing, billing);
        assert_eq!(cost.turn_plan_usd, Some(cost.turn_usd));
        assert_eq!(cost.session_billing, Billing::Mixed);
        assert_eq!(cost.session_plan_usd, Some(cost.turn_usd));
        assert_eq!(cost.turn_lower_bound, fails);
        let records = records(&store);
        let stored: Vec<_> = records
            .iter()
            .filter_map(|record| match &record.event {
                LogEvent::ContextCompacted {
                    requested_by,
                    usage,
                    cost_usd,
                    plan_usd,
                    ..
                }
                | LogEvent::CompactionFailed {
                    requested_by,
                    usage,
                    cost_usd,
                    plan_usd,
                    ..
                } => Some((requested_by, usage, cost_usd, plan_usd)),
                _ => None,
            })
            .collect();
        assert_eq!(stored.len(), 1);
        let (requested_by, usage, amount, plan) = stored[0];
        assert_eq!(requested_by.as_deref(), Some(ASK));
        assert_eq!(usage.unwrap().billing, billing);
        assert_eq!(*amount, Some(cost.turn_usd));
        assert_eq!(plan, amount);
        let reread: Vec<wisp_protocol::log::LogRecord> =
            serde_json::from_value(serde_json::to_value(&records).unwrap()).unwrap();
        assert_eq!(reread, records);
        assert!(
            records
                .iter()
                .all(|record| record.schema_version == wisp_protocol::log::LOG_SCHEMA_VERSION)
        );
        let logged = crate::resume::LoggedSession::read(&reread).unwrap();
        assert_eq!(logged.compaction_plan_usd, cost.turn_usd);
        let TurnOutcome::Completed {
            cost: Some(total), ..
        } = outcome
        else {
            panic!("expected total")
        };
        assert_eq!(Some(logged.plan_usd), total.session_plan_usd);
        assert_eq!(logged.session_usd, total.session_usd);
    }
}
