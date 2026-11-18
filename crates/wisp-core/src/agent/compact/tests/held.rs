use super::attachments::named_call;
use super::auto::{LOW, auto_started, no_retries};
use super::background::{
    NEAR, WIDE_NEAR, WIDE_POINT, agent_with, background, background_starts, overflow_at_step_two,
    overflowing, small_band, swapped_in, tool_call,
};
use super::log::scratch;
use super::*;
use crate::agent::compact::auto::Measure;
use crate::agent::tests::held;
use crate::provider::Iteration;

pub(super) fn auto_starts(seen: &[AgentEvent]) -> usize {
    seen.iter().filter(|e| auto_started(e)).count()
}

async fn paste_after_compaction(typed: &str, next: Step) -> (Vec<AgentEvent>, usize) {
    let store = MemoryStore::default();
    let provider = scripted(vec![
        answer("Hello.", 5_000),
        compaction(SUMMARY),
        answer("Pasted.", 3_500),
        next,
        answer("Third.", 300),
    ]);
    let mut live = Live::start(on(&provider, &store, LOW));
    assert!(completed(&live.turn("u1", "first").await));
    assert!(completed(&live.turn("u2", typed).await));
    assert!(completed(&live.turn("u3", "third").await));
    let (_, seen) = live.close().await;
    (seen, provider.sent().len())
}

#[tokio::test]
async fn a_large_paste_right_after_a_compaction_does_not_hold_it() {
    let paste = "y".repeat(12_000);
    let (seen, sent) = paste_after_compaction(&paste, compaction(SUMMARY)).await;
    assert!(held(&seen).is_empty(), "{:?}", held(&seen));
    assert_eq!(auto_starts(&seen), 2);
    assert_eq!(sent, 5);
    let (seen, sent) = paste_after_compaction("second", answer("Third.", 300)).await;
    let notices = held(&seen);
    assert_eq!(notices.len(), 1, "{notices:?}");
    assert!(notices[0].0 >= 2_000 && notices[0].0 < 3_500, "{notices:?}");
    assert_eq!(notices[0].1, 2_000);
    assert_eq!(auto_starts(&seen), 1);
    assert_eq!(sent, 4);
}

#[tokio::test]
async fn large_tool_results_between_steps_do_not_hold_it() {
    let scratch = scratch();
    let store = MemoryStore::default();
    let provider = scripted(vec![
        answer("Hello.", 5_000),
        compaction(SUMMARY),
        named_call("t1", "echo", json!({"text": "z".repeat(12_000)}), 600),
        compaction(SUMMARY),
        answer("Done.", 900),
    ]);
    let agent = on(&provider, &store, LOW)
        .with_tools(crate::tool::test_toolbox(), scratch.workspace())
        .with_platform("Linux")
        .with_resources(None);
    let mut live = Live::start(agent);
    assert!(completed(&live.turn("u1", "first").await));
    assert!(completed(&live.turn("u2", "second").await));
    let (_, seen) = live.close().await;
    assert!(held(&seen).is_empty(), "{:?}", held(&seen));
    assert_eq!(auto_starts(&seen), 2);
    assert_eq!(provider.sent().len(), 5);
}

async fn swapped_before(typed: &str, third: Option<Step>) -> (Vec<AgentEvent>, Arc<Fake>) {
    let store = MemoryStore::default();
    let mut script = vec![
        answer("Hello.", NEAR),
        compaction(SUMMARY),
        answer("Again.", 21_000),
    ];
    let runs_third = third.is_some();
    script.push(third.unwrap_or_else(|| compaction(SUMMARY)));
    let provider = documented(script);
    let mut live = Live::start(background(&provider, &store));
    assert!(completed(&live.turn("u1", "first").await));
    live.until(prepared).await;
    assert!(completed(&live.turn("u2", typed).await));
    if runs_third {
        assert!(completed(&live.turn("u3", "third").await));
    }
    let (_, seen) = live.close().await;
    assert!(swapped_in(&seen, 2));
    (seen, provider)
}

#[tokio::test]
async fn the_request_after_a_swap_is_measured_the_same_way_and_holds_the_precompute() {
    let (seen, provider) = swapped_before("second", Some(answer("More.", 21_500))).await;
    assert_eq!(held(&seen).len(), 1, "{:?}", held(&seen));
    assert_eq!(held(&seen)[0].1, 20_000);
    assert_eq!(background_starts(&seen), 1);
    assert_eq!(auto_starts(&seen), 0);
    assert_eq!(provider.sent().len(), 4);
    let paste = "y".repeat(16_000);
    let (seen, _) = swapped_before(&paste, None).await;
    assert!(held(&seen).is_empty(), "{:?}", held(&seen));
    assert_eq!(background_starts(&seen), 2);
}

#[tokio::test]
async fn compact_runs_and_installs_while_held_and_a_request_under_the_point_releases_it() {
    let store = MemoryStore::default();
    let provider = scripted(vec![
        answer("Hello.", 5_000),
        compaction(SUMMARY),
        answer("Two.", 3_500),
        compaction(SUMMARY),
        answer("Three.", 500),
        answer("Four.", 2_500),
        compaction(SUMMARY),
        answer("Five.", 300),
    ]);
    let mut live = Live::start(on(&provider, &store, LOW));
    assert!(completed(&live.turn("u1", "first").await));
    assert!(completed(&live.turn("u2", "second").await));
    assert_eq!(held(&live.seen).len(), 1);
    assert!(matches!(
        live.compact(3, None).await,
        AgentEvent::Compacted {
            trigger: CompactionTrigger::Manual,
            answers: Some(RequestId(3)),
            ..
        }
    ));
    for (id, text) in [("u3", "third"), ("u4", "fourth"), ("u5", "fifth")] {
        assert!(completed(&live.turn(id, text).await));
    }
    let (agent, seen) = live.close().await;
    assert!(agent.compaction.head.is_some());
    assert_eq!(held(&seen).len(), 1);
    assert_eq!(auto_starts(&seen), 2);
    assert_eq!(provider.sent().len(), 8);
}

fn searched(words: &str, each: u64, rounds: usize) -> Step {
    let iteration = Iteration {
        kind: "message".into(),
        model: DEFAULT_MODEL.into(),
        usage: usage(each, 0, 10),
    };
    let total = each * u64::try_from(rounds).unwrap();
    let mut script = vec![start(total, 0)];
    script.extend(text(0, words));
    script.extend(ended(
        StopReason::EndTurn,
        usage(total, 0, 40),
        vec![iteration; rounds],
    ));
    Step::from(script)
}

#[tokio::test]
async fn a_reply_with_iterations_is_measured_at_its_last_iteration() {
    let store = MemoryStore::default();
    let provider = scripted(vec![
        answer("Hello.", 5_000),
        compaction(SUMMARY),
        searched("Searched.", 900, 3),
    ]);
    let mut live = Live::start(on(&provider, &store, LOW));
    assert!(completed(&live.turn("u1", "first").await));
    assert!(completed(&live.turn("u2", "second").await));
    let (_, seen) = live.close().await;
    assert_eq!(auto_starts(&seen), 1);
    assert!(held(&seen).is_empty(), "{:?}", held(&seen));
}

#[tokio::test]
async fn a_turn_that_fails_before_its_request_leaves_no_reserve_for_the_next() {
    let store = MemoryStore::default();
    let provider = scripted(vec![
        answer("Hello.", 5_000),
        compaction(SUMMARY),
        server_failed(),
        answer("Pasted.", 3_500),
    ]);
    let mut live = Live::start(no_retries(on(&provider, &store, LOW)));
    assert!(completed(&live.turn("u1", "first").await));
    assert!(!completed(&live.turn("u2", "second").await));
    assert!(completed(&live.turn("u3", &"y".repeat(12_000)).await));
    let (_, seen) = live.close().await;
    assert_eq!(auto_starts(&seen), 1);
    assert!(held(&seen).is_empty(), "{:?}", held(&seen));
    assert_eq!(provider.sent().len(), 4);
}

#[tokio::test]
async fn an_overflow_swap_measured_over_the_point_does_not_hold() {
    let store = MemoryStore::default();
    let precompute = Arc::new(Semaphore::new(0));
    let step = Arc::new(Semaphore::new(0));
    let provider = documented(vec![
        answer("Hello.", WIDE_NEAR),
        compaction(SUMMARY).gated(&precompute),
        tool_call(WIDE_NEAR + 100, "Looking."),
        overflowing(&step),
        answer("Done.", 200_500),
        compaction(SUMMARY),
    ]);
    let mut live = Live::start(agent_with(&provider, &store, WIDE_POINT, small_band()));
    let ended = overflow_at_step_two(&mut live, &precompute, &step).await;
    assert!(
        matches!(
            ended,
            AgentEvent::TurnEnded {
                outcome: TurnOutcome::Completed { .. },
                ..
            }
        ),
        "{ended:?}"
    );
    let (_, seen) = live.close().await;
    assert!(swapped_in(&seen, 2), "{seen:#?}");
    assert!(held(&seen).is_empty(), "{:?}", held(&seen));
    assert_eq!(background_starts(&seen), 2);
}

async fn set_point(live: &mut Live, request: u64, tokens: u64) {
    live.send(ClientCommand::ChangeSettings {
        request: RequestId(request),
        changes: vec![SettingChange::named("compact_at", SettingValue::Tokens(tokens)).unwrap()],
        save: false,
    })
    .await;
    live.until(|e| {
        matches!(e, AgentEvent::SettingsChanged { answers: Some(RequestId(r)), .. } if *r == request)
    })
    .await;
}

#[tokio::test]
async fn a_point_changed_and_changed_back_does_not_bring_the_hold_back() {
    let store = MemoryStore::default();
    let provider = scripted(vec![
        answer("Hello.", 5_000),
        compaction(SUMMARY),
        answer("Two.", 3_500),
        answer("Three.", 3_600),
        compaction(SUMMARY),
        answer("Four.", 300),
    ]);
    let mut live = Live::start(on(&provider, &store, LOW));
    assert!(completed(&live.turn("u1", "first").await));
    assert!(completed(&live.turn("u2", "second").await));
    assert_eq!(held(&live.seen).len(), 1);
    set_point(&mut live, 1, 900_000).await;
    assert!(completed(&live.turn("u3", "third").await));
    set_point(&mut live, 2, 2_000).await;
    assert!(completed(&live.turn("u4", "fourth").await));
    let (_, seen) = live.close().await;
    assert_eq!(auto_starts(&seen), 2);
    assert_eq!(provider.sent().len(), 6);
}

const BOTH_LOW: &str = "[models.\"claude-opus-5-5\"]\ncompact_at = 2000\n[models.\"claude-sonnet-4-6\"]\ncompact_at = 2000\n";

async fn held_agent() -> Agent {
    let mut agent = on(&scripted(vec![]), &MemoryStore::default(), BOTH_LOW);
    let (sink, mut events) = mpsc::channel(8);
    agent.measure_next(Measure::arming(0));
    agent.measure_auto(5_000, &sink).await;
    drop(sink);
    assert!(matches!(
        events.recv().await,
        Some(AgentEvent::AutoCompactionHeld {
            tokens: 5_000,
            point: 2_000
        })
    ));
    assert!(agent.auto_held(false));
    agent
}

#[tokio::test]
async fn a_model_or_fixed_prompt_change_releases_the_hold() {
    let mut agent = held_agent().await;
    agent.session.model = "claude-sonnet-4-6".into();
    assert_eq!(agent.window_and_point().1, Some(2_000));
    assert!(!agent.auto_held(false));
    let mut agent = held_agent().await;
    agent.session.system = Some("Another prompt.".into());
    assert!(!agent.auto_held(false));
    let mut agent = held_agent().await;
    agent.set_toolbox(crate::tool::test_toolbox());
    assert!(!agent.auto_held(false));
}
