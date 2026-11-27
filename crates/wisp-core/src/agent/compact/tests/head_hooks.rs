use std::sync::atomic::{AtomicBool, Ordering};

use tokio::sync::Notify;
use wisp_plugin_api::hooks::{Event, HeadInfo, Reply};
use wisp_plugin_testkit::hooks::{Calls, Script as HookScript};
use wisp_protocol::{HookOutcome, Placement, RewindPoint};

use super::background::{NEAR, WIDE_POINT, agent_with, background, small_band, swapped_in};
use super::gate::{kit_none, ran, replying};
use super::hooks::heard_by;
use super::overflow::{TOO_LONG, cut_off, off, three_answers, too_long, wisp_off};
use super::rewind::assert_points_resume_as_live;
use super::sections::{
    cancelled, converted, converting, ends, overflow_at_step_two, swapping_for_an_overflow,
};
use super::*;
use crate::clock::StillClock;
use crate::plugin::testing::hook_body;
use crate::test_guard::records;

const HEAD: &str = "aa/head";
const NOTE: &str = "Head note.";

fn head_body() -> String {
    hook_body("head", "compaction-head", "")
}

fn noting() -> HookScript {
    replying(vec![("head", Reply::Append(vec![NOTE.into()]))])
}

fn heads(calls: &Calls) -> Vec<HeadInfo> {
    calls
        .of("head")
        .into_iter()
        .filter_map(|c| match c.event {
            Event::CompactionHead(info) => Some(info),
            _ => None,
        })
        .collect()
}

fn head_blocks(agent: &Agent) -> Vec<ContentBlock> {
    let head = agent.compaction.head.as_ref().unwrap();
    let mut blocks = head.message.content.clone();
    blocks.extend(head.additions.clone());
    blocks
}

fn last_text(agent: &Agent) -> String {
    match head_blocks(agent).last() {
        Some(ContentBlock::Text { text }) => text.clone(),
        other => panic!("{other:?}"),
    }
}

async fn resumed(store: &MemoryStore, provider: Arc<Fake>, calls: &Calls) -> Agent {
    let records = records(store);
    let seq = u64::try_from(records.len()).unwrap();
    let log = SessionLog::continuing(Box::new(store.clone()), "s1", seq);
    let agent = Agent::new(provider, default_session())
        .with_log(log, log_context())
        .with_model_settings(models(WISP));
    heard_by(agent, &head_body(), noting(), calls)
        .await
        .resume(&records, 0)
        .unwrap()
}

#[tokio::test]
async fn a_wisp_head_puts_the_hook_texts_after_its_compacted_context_and_resume_and_rewind_rebuild_it()
 {
    let calls = Calls::default();
    let store = MemoryStore::default();
    let provider = scripted(vec![
        answer("Hello.", 1_000),
        answer(WRITTEN, 1_200),
        answer("Again.", 300),
        answer("Back.", 300),
        answer("Rewound.", 300),
    ]);
    let agent = wisp_engine(&provider, &store).with_clock(Arc::new(StillClock));
    let agent = heard_by(agent, &head_body(), noting(), &calls).await;
    let mut live = Live::start(agent);
    assert!(completed(&live.turn("u1", "first").await));
    let done = live.compact(2, None).await;
    assert!(matches!(done, AgentEvent::Compacted { .. }), "{done:?}");
    assert!(completed(&live.turn("u2", "second").await));
    let (agent, _) = live.close().await;
    let records = records(&store);
    assert_points_resume_as_live(&agent, &records, &[2], |r| {
        super::rewind::resumed(scripted(Vec::new()), r)
    });
    let head = head_text(&agent);
    let context = head.find("<compacted_context").unwrap();
    let note = head.find(NOTE).unwrap();
    assert!(context < note, "{head}");
    assert_eq!(last_text(&agent), NOTE);
    assert_eq!(ran(&store, HEAD), [HookOutcome::Applied]);
    assert!(provider.body(2).to_string().contains(NOTE));
    assert_rebuilds(&store, &agent);
    let fired = calls.count("head");
    assert_eq!(fired, 1);

    let again = Calls::default();
    let resumed = resumed(&store, provider.clone(), &again).await;
    assert_eq!(head_blocks(&resumed), head_blocks(&agent));
    let mut live = Live::start(resumed);
    assert!(completed(&live.turn("u3", "third").await));
    live.send(ClientCommand::Rewind {
        to: RewindPoint::Turn { turn: 3 },
        request: RequestId(20),
    })
    .await;
    live.until(|e| matches!(e, AgentEvent::Rewound { .. }))
        .await;
    assert!(completed(&live.turn("u4", "fourth").await));
    let (rewound, _) = live.close().await;
    assert_eq!(head_blocks(&rewound), head_blocks(&agent));
    assert_eq!(again.count("head"), 0);
    let before = strip_cache_control(&provider.body(3)["messages"]).to_string();
    let after = strip_cache_control(&provider.body(4)["messages"]).to_string();
    let prefix = before.split("third").next().unwrap();
    assert!(after.starts_with(prefix), "{after}");
}

#[tokio::test]
async fn a_native_head_ends_with_the_hook_texts() {
    let calls = Calls::default();
    let store = MemoryStore::default();
    let provider = scripted(vec![
        answer("Hello.", 1_000),
        compaction(SUMMARY),
        answer("Again.", 300),
    ]);
    let agent = heard_by(
        logged(provider.clone(), &store),
        &head_body(),
        noting(),
        &calls,
    )
    .await;
    let mut live = Live::start(agent);
    assert!(completed(&live.turn("u1", "first").await));
    live.compact(2, None).await;
    assert!(completed(&live.turn("u2", "second").await));
    let (agent, _) = live.close().await;
    let blocks = head_blocks(&agent);
    assert!(
        matches!(blocks[0], ContentBlock::Opaque { .. }),
        "{blocks:?}"
    );
    assert_eq!(last_text(&agent), NOTE);
    let info = &heads(&calls)[0];
    assert_eq!(
        (
            info.epoch,
            info.trigger.as_str(),
            info.engine.as_str(),
            info.first_turn,
            info.last_turn
        ),
        (1, "manual", "native", 1, 1)
    );
    assert_rebuilds(&store, &agent);
}

#[tokio::test]
async fn a_kept_overflow_head_fires_the_head_point_once_and_carries_its_texts() {
    let calls = Calls::default();
    let store = MemoryStore::default();
    let mut steps = three_answers();
    steps.extend([
        too_long(TOO_LONG),
        compaction(SUMMARY),
        answer("Four.", 900),
        answer("Five.", 950),
    ]);
    let provider = scripted(steps);
    let agent = off(&provider, &store).with_clock(Arc::new(StillClock));
    let agent = heard_by(agent, &head_body(), noting(), &calls).await;
    let mut live = Live::start(agent);
    three_turns(&mut live).await;
    assert!(completed(&live.turn("u4", "fourth").await));
    assert!(completed(&live.turn("u5", "fifth").await));
    let (agent, _) = live.close().await;
    let records = records(&store);
    assert_points_resume_as_live(&agent, &records, &[5], |r| {
        super::rewind::resumed(scripted(Vec::new()), r)
    });
    let head = agent.compaction.head.as_ref().unwrap();
    assert_eq!(head.placement, Placement::Leading);
    assert_eq!(calls.count("head"), 1);
    assert_eq!(last_text(&agent), NOTE);
    let info = &heads(&calls)[0];
    assert_eq!((info.first_turn, info.last_turn), head.turns);
    assert_eq!(info.trigger, "overflow");
    assert!(provider.body(5).to_string().contains(NOTE));
    assert_rebuilds(&store, &agent);
}

#[tokio::test]
async fn a_swap_between_turns_carries_the_hook_texts() {
    let calls = Calls::default();
    let store = MemoryStore::default();
    let provider = documented(vec![
        answer("Hello.", NEAR),
        compaction(SUMMARY),
        answer("Again.", 300),
    ]);
    let agent = heard_by(
        background(&provider, &store),
        &head_body(),
        noting(),
        &calls,
    )
    .await;
    let mut live = Live::start(agent);
    assert!(completed(&live.turn("u1", "first").await));
    live.until(prepared).await;
    assert!(completed(&live.turn("u2", "second").await));
    let (agent, seen) = live.close().await;
    assert!(
        swapped_in(&seen, 2) || agent.compaction.head.is_some(),
        "{seen:#?}"
    );
    assert_eq!(calls.count("head"), 1);
    assert_eq!(last_text(&agent), NOTE);
    assert!(provider.body(2).to_string().contains(NOTE));
    assert_rebuilds(&store, &agent);
}

#[tokio::test]
async fn a_swap_for_an_overflow_carries_the_hook_texts() {
    let calls = Calls::default();
    let store = MemoryStore::default();
    let precompute = Arc::new(Semaphore::new(0));
    let step = Arc::new(Semaphore::new(0));
    let provider = documented(swapping_for_an_overflow(&precompute, &step));
    let agent = agent_with(&provider, &store, WIDE_POINT, small_band());
    let agent = heard_by(agent, &head_body(), noting(), &calls).await;
    let mut live = Live::start(agent);
    assert!(completed(&live.turn("u1", "first").await));
    let outcome = overflow_at_step_two(&mut live, &precompute, &step).await;
    let (agent, seen) = live.close().await;
    assert!(completed(&outcome), "{outcome:?}");
    assert!(swapped_in(&seen, 2), "{seen:#?}");
    assert_eq!(calls.count("head"), 1);
    assert_eq!(last_text(&agent), NOTE);
    assert!(provider.body(4).to_string().contains(NOTE));
    assert_rebuilds(&store, &agent);
}

#[tokio::test]
async fn a_convert_asks_the_head_hooks_again_and_keeps_a_text_no_smaller_rule_would_refuse() {
    let calls = Calls::default();
    let store = MemoryStore::default();
    let provider = scripted(converting());
    let big = "y".repeat(9_000);
    let script = replying(vec![("head", Reply::Append(vec![big.clone()]))]);
    let agent = heard_by(
        logged(provider.clone(), &store),
        &head_body(),
        script,
        &calls,
    )
    .await;
    let mut live = Live::start(agent);
    assert!(completed(&live.turn("u1", "first").await));
    live.compact(2, None).await;
    assert!(completed(&live.turn("u2", "second").await));
    let (agent, seen) = live.close().await;
    assert!(converted(&seen), "{seen:#?}");
    assert_eq!(calls.count("head"), 2);
    let infos = heads(&calls);
    assert_eq!(
        (
            infos[1].epoch,
            infos[1].trigger.as_str(),
            infos[1].engine.as_str()
        ),
        (2, "convert", "wisp")
    );
    assert_eq!((infos[1].first_turn, infos[1].last_turn), (1, 2));
    assert_eq!(last_text(&agent), big);
    assert_eq!(
        ran(&store, HEAD),
        [
            HookOutcome::Rejected {
                rule: "not_smaller".into()
            },
            HookOutcome::Applied
        ]
    );
    let head = head_text(&agent);
    assert!(head.find("<compacted_context").unwrap() < head.find(&big).unwrap());
    assert_rebuilds(&store, &agent);
}

#[tokio::test]
async fn a_convert_caps_the_hook_texts_at_twenty_thousand_characters() {
    let calls = Calls::default();
    let store = MemoryStore::default();
    let provider = scripted(converting());
    let script = replying(vec![
        ("head", Reply::Append(vec!["a".repeat(9_000)])),
        (
            "more",
            Reply::Append(vec!["b".repeat(9_000), "c".repeat(2_001)]),
        ),
    ]);
    let body = head_body() + &hook_body("more", "compaction-head", "after = [\"aa/head\"]\n");
    let agent = heard_by(logged(provider.clone(), &store), &body, script, &calls).await;
    let mut live = Live::start(agent);
    assert!(completed(&live.turn("u1", "first").await));
    live.compact(2, None).await;
    assert!(completed(&live.turn("u2", "second").await));
    let (agent, _) = live.close().await;
    assert_eq!(
        ran(&store, "aa/more").last(),
        Some(&HookOutcome::Rejected {
            rule: "too_large".into()
        })
    );
    assert_eq!(last_text(&agent), "a".repeat(9_000));
}

#[tokio::test]
async fn the_head_point_reports_a_written_heads_epoch_and_turns() {
    let calls = Calls::default();
    let store = MemoryStore::default();
    let provider = scripted(vec![
        answer("Hello.", 1_000),
        answer("Two.", 1_000),
        answer(WRITTEN, 1_200),
        answer("Three.", 300),
        answer(WRITTEN, 1_200),
    ]);
    let agent = heard_by(
        wisp_engine(&provider, &store),
        &head_body(),
        noting(),
        &calls,
    )
    .await;
    let mut live = Live::start(agent);
    live.turn("u1", "first").await;
    live.turn("u2", "second").await;
    live.compact(3, None).await;
    live.turn("u3", "third").await;
    live.compact(4, None).await;
    let (agent, _) = live.close().await;
    let rows: Vec<(u64, u64, u64, String)> = heads(&calls)
        .into_iter()
        .map(|i| (i.epoch, i.first_turn, i.last_turn, i.engine))
        .collect();
    let head = agent.compaction.head.as_ref().unwrap();
    assert_eq!((head.epoch, head.turns), (2, (3, 3)));
    assert_eq!(
        rows,
        [(1, 1, 2, "wisp".to_owned()), (2, 3, 3, "wisp".to_owned())]
    );
}

fn held(armed: Arc<AtomicBool>, started: Arc<Notify>) -> HookScript {
    Arc::new(move |_, hook, _| {
        let armed = armed.load(Ordering::SeqCst);
        let started = started.clone();
        Box::pin(async move {
            if hook == "held" && armed {
                started.notify_one();
                std::future::pending::<()>().await;
            }
            Reply::Append(vec![NOTE.into()])
        })
    })
}

fn held_body() -> String {
    head_body() + &hook_body("held", "compaction-head", "after = [\"aa/head\"]\n")
}

struct Esc {
    started: Arc<Notify>,
    armed: Arc<AtomicBool>,
    calls: Calls,
}

impl Esc {
    async fn on(agent: Agent) -> (Agent, Self) {
        let calls = Calls::default();
        let started = Arc::new(Notify::new());
        let armed = Arc::new(AtomicBool::new(true));
        let script = held(armed.clone(), started.clone());
        let agent = heard_by(agent, &held_body(), script, &calls).await;
        (
            agent,
            Self {
                started,
                armed,
                calls,
            },
        )
    }

    fn cancel(&self, live: &Live) -> tokio::task::JoinHandle<()> {
        let commands = live.commands.clone().unwrap();
        let started = self.started.clone();
        tokio::spawn(async move {
            within("the hook's start", started.notified()).await;
            commands.send(ClientCommand::Cancel).await.unwrap();
        })
    }
}

#[tokio::test]
async fn esc_during_the_head_point_at_a_compact_keeps_the_summary_with_the_finished_texts() {
    let store = MemoryStore::default();
    let provider = scripted(vec![
        answer("Hello.", 1_000),
        compaction(SUMMARY),
        answer("Again.", 300),
    ]);
    let (agent, esc) = Esc::on(logged(provider.clone(), &store)).await;
    let mut live = Live::start(agent);
    assert!(completed(&live.turn("u1", "first").await));
    let canceller = esc.cancel(&live);
    let done = live.compact(2, None).await;
    canceller.await.unwrap();
    assert!(
        matches!(done, AgentEvent::Compacted { turn: None, .. }),
        "{done:?}"
    );
    let (agent, seen) = live.close().await;
    assert_eq!(ends(&seen), 1);
    assert!(agent.track.is_none());
    assert_eq!(last_text(&agent), NOTE);
    assert_eq!(esc.calls.count("head"), 1);
    assert_eq!(provider.sent().len(), 2);
    assert!(
        !log_events(&store)
            .iter()
            .any(|e| matches!(e, LogEvent::TurnStarted { turn: 2, .. }))
    );
    assert_rebuilds(&store, &agent);
}

async fn esc_in_a_turn(agent: Agent, provider: &Arc<Fake>, sent: usize) {
    let (agent, esc) = Esc::on(agent).await;
    let mut live = Live::start(agent);
    assert!(completed(&live.turn("u1", "first").await));
    let canceller = esc.cancel(&live);
    let outcome = live.turn("u2", "second").await;
    canceller.await.unwrap();
    live.close().await;
    assert!(cancelled(&outcome), "{outcome:?}");
    assert_eq!(provider.sent().len(), sent);
}

#[tokio::test]
async fn esc_during_the_head_point_of_an_in_turn_compaction_cancels_the_turn() {
    let store = MemoryStore::default();
    let provider = scripted(vec![
        answer("One.", 1_000),
        cut_off(2_000, "Looking around.", true),
        compaction(SUMMARY),
        answer("unused", 900),
    ]);
    esc_in_a_turn(off(&provider, &store), &provider, 3).await;
}

#[tokio::test]
async fn esc_during_the_head_point_of_an_automatic_compaction_cancels_the_turn() {
    let store = MemoryStore::default();
    let provider = scripted(vec![
        answer("Hello.", 5_000),
        compaction(SUMMARY),
        answer("unused", 1),
    ]);
    let agent = on(&provider, &store, super::auto::LOW);
    esc_in_a_turn(agent, &provider, 2).await;
}

#[tokio::test]
async fn esc_during_the_head_point_of_a_convert_in_a_turn_cancels_the_turn() {
    let store = MemoryStore::default();
    let provider = scripted(converting());
    let (agent, esc) = Esc::on(logged(provider.clone(), &store)).await;
    esc.armed.store(false, Ordering::SeqCst);
    let mut live = Live::start(agent);
    assert!(completed(&live.turn("u1", "first").await));
    live.compact(2, None).await;
    esc.armed.store(true, Ordering::SeqCst);
    let canceller = esc.cancel(&live);
    let outcome = live.turn("u2", "second").await;
    canceller.await.unwrap();
    let (_, seen) = live.close().await;
    assert!(cancelled(&outcome), "{outcome:?}");
    assert!(converted(&seen), "{seen:#?}");
    assert_eq!(provider.sent().len(), 3);
}

#[tokio::test]
async fn esc_during_the_head_point_of_a_swap_for_an_overflow_cancels_the_turn() {
    let store = MemoryStore::default();
    let precompute = Arc::new(Semaphore::new(0));
    let step = Arc::new(Semaphore::new(0));
    let provider = documented(swapping_for_an_overflow(&precompute, &step));
    let agent = agent_with(&provider, &store, WIDE_POINT, small_band());
    let (agent, esc) = Esc::on(agent).await;
    let mut live = Live::start(agent);
    assert!(completed(&live.turn("u1", "first").await));
    let canceller = esc.cancel(&live);
    let outcome = overflow_at_step_two(&mut live, &precompute, &step).await;
    canceller.await.unwrap();
    let (_, seen) = live.close().await;
    assert!(cancelled(&outcome), "{outcome:?}");
    assert!(swapped_in(&seen, 2), "{seen:#?}");
    assert_eq!(provider.sent().len(), 4);
}

#[tokio::test]
async fn esc_during_the_head_point_of_a_switch_compaction_cancels_the_turn() {
    let store = MemoryStore::default();
    let provider = scripted(vec![
        answer("Hello.", 1_000),
        compaction(SUMMARY),
        answer("Second.", 300),
        summarised("Fresh.", 2_000, 0, vec![]),
        answer("unused", 400),
    ]);
    let agent = super::marker::other_without_native(&store, &provider);
    let calls = Calls::default();
    let started = Arc::new(Notify::new());
    let armed = Arc::new(std::sync::atomic::AtomicBool::new(false));
    let script: HookScript = {
        let started = started.clone();
        let armed = armed.clone();
        Arc::new(move |_, _, _| {
            let started = started.clone();
            let armed = armed.load(std::sync::atomic::Ordering::SeqCst);
            Box::pin(async move {
                if armed {
                    started.notify_one();
                    std::future::pending::<()>().await;
                }
                Reply::None
            })
        })
    };
    let agent = heard_by(agent, &head_body(), script, &calls).await;
    let mut live = Live::start(agent);
    assert!(completed(&live.turn("u1", "first").await));
    live.compact(2, None).await;
    assert!(completed(&live.turn("u2", "second").await));
    super::marker::change(&mut live, 3, Some("other"), None).await;
    armed.store(true, std::sync::atomic::Ordering::SeqCst);
    let commands = live.commands.clone().unwrap();
    let canceller = tokio::spawn(async move {
        within("the hook's start", started.notified()).await;
        commands.send(ClientCommand::Cancel).await.unwrap();
    });
    let outcome = live.turn("u3", "third").await;
    canceller.await.unwrap();
    let (_, seen) = live.close().await;
    assert!(cancelled(&outcome), "{outcome:?}");
    assert_eq!(provider.sent().len(), 4);
    assert!(
        seen.iter().any(|e| matches!(
            e,
            AgentEvent::Compacted {
                trigger: CompactionTrigger::Switch,
                ..
            }
        )),
        "{seen:#?}"
    );
}

fn two_sized(first: u64, second: u64) -> HookScript {
    let text = |tokens: u64, c: &str| c.repeat(usize::try_from(tokens * 3).unwrap());
    replying(vec![
        ("head", Reply::Append(vec![text(first, "a")])),
        ("more", Reply::Append(vec![text(second, "b")])),
    ])
}

fn two_body() -> String {
    head_body() + &hook_body("more", "compaction-head", "after = [\"aa/head\"]\n")
}

#[tokio::test]
async fn a_second_head_text_past_the_room_left_by_the_pending_blocks_is_rejected_not_smaller() {
    let calls = Calls::default();
    let store = MemoryStore::default();
    let mut steps = three_answers();
    steps.extend([
        too_long(TOO_LONG),
        answer(WRITTEN, 1_200),
        answer("Four.", 900),
    ]);
    let provider = scripted(steps);
    let agent = heard_by(
        wisp_off(&provider, &store),
        &two_body(),
        two_sized(200, 300),
        &calls,
    )
    .await;
    let mut live = Live::start(agent);
    three_turns(&mut live).await;
    let pending = "word ".repeat(PENDING_CHARS);
    assert!(completed(&live.turn("u4", &pending).await));
    let (agent, _) = live.close().await;
    assert_eq!(ran(&store, HEAD), [HookOutcome::Applied]);
    assert_eq!(
        ran(&store, "aa/more"),
        [HookOutcome::Rejected {
            rule: "not_smaller".into()
        }]
    );
    assert_eq!(last_text(&agent), "a".repeat(600));
}

const PENDING_CHARS: usize = 81_300;

const RESERVED: &str = "A reserved line for the head.";

async fn compacted_with(body: &str) -> Agent {
    let calls = Calls::default();
    let store = MemoryStore::default();
    let provider = scripted(vec![answer("Hello.", 1_000), compaction(SUMMARY)]);
    let agent = heard_by(logged(provider.clone(), &store), body, kit_none(), &calls).await;
    let mut live = Live::start(agent);
    live.turn("u1", "first").await;
    live.compact(2, None).await;
    live.close().await.0
}

#[tokio::test]
async fn a_head_reserve_is_counted_in_the_head_additions_and_the_convert_quote() {
    let plain = compacted_with(&head_body()).await;
    let reserved = compacted_with(&hook_body(
        "head",
        "compaction-head",
        &format!("reserve = \"{RESERVED}\"\n"),
    ))
    .await;
    let tokens = crate::context::text_tokens(RESERVED);
    assert_eq!(reserved.head_reserve_tokens(), tokens);
    assert_eq!(
        reserved.head_additions_for((1, 2)),
        plain.head_additions_for((1, 2)) + tokens
    );
    assert_eq!(
        reserved.converted_head_tokens(),
        plain.converted_head_tokens() + tokens
    );
    let quote = |agent: &Agent| match agent.switch_consequence(SwitchDue::Convert) {
        Consequence::CompactsBeforeSwitch { head_tokens, .. } => head_tokens,
        other => panic!("{other:?}"),
    };
    assert_eq!(quote(&reserved), quote(&plain) + tokens);
}

fn sized_at(trigger: &'static str, tokens: u64) -> HookScript {
    let text = "z".repeat(usize::try_from(tokens * 3).unwrap());
    Arc::new(move |_, hook, event| {
        let reply = match event {
            Event::CompactionHead(info) if hook == "head" && info.trigger == trigger => {
                Reply::Append(vec![text.clone()])
            }
            _ => Reply::None,
        };
        Box::pin(async move { reply })
    })
}

fn last_ran(store: &MemoryStore) -> HookOutcome {
    ran(store, HEAD)
        .into_iter()
        .find(|o| *o != HookOutcome::None)
        .unwrap()
}

fn big(tokens: u64) -> String {
    "w".repeat(usize::try_from(tokens * 3).unwrap())
}

const NARROW: &str = "[models.\"claude-opus-5-5\"]\ncontext_window = 110000\n";

async fn convert_in_turn_bound(tokens: u64) -> HookOutcome {
    let calls = Calls::default();
    let store = MemoryStore::default();
    let provider = scripted(converting());
    let agent = logged(provider.clone(), &store)
        .with_auto_compact(false)
        .with_model_settings(models(NARROW));
    let agent = heard_by(agent, &head_body(), sized_at("convert", tokens), &calls).await;
    let mut live = Live::start(agent);
    assert!(completed(&live.turn("u1", "first").await));
    live.compact(2, None).await;
    live.turn("u2", &big(CONVERT_PENDING)).await;
    let (_, seen) = live.close().await;
    assert!(converted(&seen), "{seen:#?}");
    last_ran(&store)
}

const CONVERT_PENDING: u64 = 100_888;

async fn switch_convert_bound(tokens: u64) -> HookOutcome {
    let calls = Calls::default();
    let store = MemoryStore::default();
    let provider = scripted(vec![
        answer("Hello.", 1_000),
        compaction(SUMMARY),
        answer("Two.", 300),
    ]);
    let agent = two_endpoints(logged(provider.clone(), &store), provider.clone())
        .with_auto_compact(false)
        .with_model_settings(models(
            "[endpoints.other.models.\"claude-opus-5-5\"]\nnative_compaction = false\ncontext_window = 110000\n",
        ));
    let agent = heard_by(agent, &head_body(), sized_at("convert", tokens), &calls).await;
    let mut live = Live::start(agent);
    assert!(completed(&live.turn("u1", "first").await));
    live.compact(2, None).await;
    super::marker::change(&mut live, 3, Some("other"), None).await;
    live.turn("u2", &big(SWITCH_PENDING)).await;
    live.close().await;
    last_ran(&store)
}

const SWITCH_PENDING: u64 = 100_888;

async fn swap_for_overflow_bound(tokens: u64) -> HookOutcome {
    let calls = Calls::default();
    let store = MemoryStore::default();
    let precompute = Arc::new(Semaphore::new(0));
    let step = Arc::new(Semaphore::new(0));
    let mut steps = swapping_for_an_overflow(&precompute, &step);
    let stated = format!("prompt is too long: 250000 tokens > {SWAP_LIMIT} maximum");
    steps[3] = Step::from(vec![Err(ProviderError::new(
        ErrorKind::ContextOverflow,
        stated,
    ))])
    .gated(&step);
    steps.push(Step::hang());
    let provider = documented(steps);
    let agent = agent_with(&provider, &store, WIDE_POINT, small_band());
    let agent = heard_by(agent, &head_body(), sized_at("background", tokens), &calls).await;
    let mut live = Live::start(agent);
    assert!(completed(&live.turn("u1", "first").await));
    live.send(send("u2", "second")).await;
    live.until(|e| matches!(e, AgentEvent::StepStarted { step: 2 }))
        .await;
    precompute.add_permits(1);
    tokio::time::sleep(std::time::Duration::from_millis(200)).await;
    step.add_permits(1);
    live.until(|e| matches!(e, AgentEvent::TurnEnded { .. }))
        .await;
    let (_, seen) = live.close().await;
    assert!(swapped_in(&seen, 2), "{seen:#?}");
    last_ran(&store)
}

const SWAP_LIMIT: u64 = 65_186;

async fn swap_before_turn_bound(tokens: u64) -> HookOutcome {
    let calls = Calls::default();
    let store = MemoryStore::default();
    let precompute = Arc::new(Semaphore::new(0));
    let provider = documented(vec![
        answer("One.", 37_000),
        compaction(SUMMARY).gated(&precompute),
        super::background::window_stop(38_000, &"word ".repeat(3_000)),
        compaction(SUMMARY),
        answer("Three.", 900),
        Step::hang(),
    ]);
    let config = "[models.\"claude-opus-5-5\"]\ncompact_at = 40000\ncontext_window = 110000\n";
    let agent = agent_with(&provider, &store, config, super::background::wide_band());
    let agent = heard_by(agent, &head_body(), sized_at("background", tokens), &calls).await;
    let mut live = Live::start(agent);
    assert!(completed(&live.turn("u1", "first").await));
    live.turn("u2", "second").await;
    precompute.add_permits(1);
    live.until(prepared).await;
    live.turn("u3", &big(BEFORE_PENDING)).await;
    let (_, seen) = live.close().await;
    assert!(swapped_in(&seen, 3), "{seen:#?}");
    last_ran(&store)
}

const BEFORE_PENDING: u64 = 39_874;

fn not_smaller() -> HookOutcome {
    HookOutcome::Rejected {
        rule: "not_smaller".into(),
    }
}

#[tokio::test]
async fn a_convert_in_a_turn_holds_the_head_texts_to_its_window() {
    assert_eq!(convert_in_turn_bound(UNDER).await, HookOutcome::Applied);
    assert_eq!(convert_in_turn_bound(OVER).await, not_smaller());
}

#[tokio::test]
async fn a_switchs_plain_convert_holds_the_head_texts_to_the_new_window() {
    assert_eq!(switch_convert_bound(UNDER).await, HookOutcome::Applied);
    assert_eq!(switch_convert_bound(OVER).await, not_smaller());
}

#[tokio::test]
async fn a_swap_for_an_overflow_holds_the_head_texts_to_the_overflows_limit() {
    assert_eq!(swap_for_overflow_bound(UNDER).await, HookOutcome::Applied);
    assert_eq!(swap_for_overflow_bound(OVER).await, not_smaller());
}

#[tokio::test]
async fn a_swap_before_a_turn_with_an_overflow_due_holds_the_head_texts_to_the_window() {
    assert_eq!(swap_before_turn_bound(UNDER).await, HookOutcome::Applied);
    assert_eq!(swap_before_turn_bound(OVER).await, not_smaller());
}

const UNDER: u64 = 900;
const OVER: u64 = 1_100;

#[tokio::test]
async fn a_compaction_with_a_head_addition_reads_the_cached_prefix_and_the_next_request_extends_the_new_one()
 {
    let calls = Calls::default();
    let store = MemoryStore::default();
    let provider = scripted(vec![
        answer("Hello.", 1_000),
        compaction(SUMMARY),
        answer("Again.", 300),
        answer("More.", 400),
    ]);
    let agent = heard_by(
        logged(provider.clone(), &store),
        &head_body(),
        noting(),
        &calls,
    )
    .await;
    let mut live = Live::start(agent);
    assert!(completed(&live.turn("u1", "first").await));
    live.compact(2, None).await;
    assert!(completed(&live.turn("u2", "second").await));
    assert!(completed(&live.turn("u3", "third").await));
    live.close().await;
    super::background::extends(&provider, 0, 1);
    assert!(provider.body(2).to_string().contains(NOTE));
    super::background::extends(&provider, 2, 3);
    super::auto::assert_prefix_kept(&provider);
}

#[tokio::test]
async fn a_compaction_request_fires_no_before_request_hook() {
    let calls = Calls::default();
    let store = MemoryStore::default();
    let provider = scripted(vec![
        answer("Hello.", 5_000),
        compaction(SUMMARY),
        answer("Again.", 300),
    ]);
    let agent = on(&provider, &store, super::auto::LOW);
    let body = hook_body("asked", "before-request", "");
    let agent = heard_by(agent, &body, kit_none(), &calls).await;
    let mut live = Live::start(agent);
    assert!(completed(&live.turn("u1", "first").await));
    assert!(completed(&live.turn("u2", "second").await));
    live.close().await;
    assert_eq!(provider.sent().len(), 3);
    assert_eq!(calls.count("asked"), 2);
}

async fn kept_overflow_bound(tokens: u64) -> HookOutcome {
    let calls = Calls::default();
    let store = MemoryStore::default();
    let mut steps = three_answers();
    let stated = format!("prompt is too long: 250000 tokens > {KEPT_LIMIT} maximum");
    steps.extend([
        too_long(&stated),
        summarised(SUMMARY, 50_000, 0, vec![]),
        answer("Four.", 900),
    ]);
    let provider = scripted(steps);
    let agent = heard_by(
        off(&provider, &store),
        &head_body(),
        sized_at("overflow", tokens),
        &calls,
    )
    .await;
    let mut live = Live::start(agent);
    assert!(completed(&live.turn("u1", "first").await));
    assert!(completed(&live.turn("u2", "second").await));
    assert!(completed(&live.turn("u3", &big(40_000)).await));
    let outcome = live.turn("u4", "fourth").await;
    assert!(completed(&outcome), "{outcome:?}");
    let (agent, _) = live.close().await;
    let head = agent.compaction.head.as_ref().unwrap();
    assert_eq!(head.placement, Placement::Leading);
    last_ran(&store)
}

const KEPT_LIMIT: u64 = 113_142;

#[tokio::test]
async fn a_kept_overflow_head_holds_the_head_texts_to_its_kept_room() {
    assert_eq!(kept_overflow_bound(UNDER).await, HookOutcome::Applied);
    assert_eq!(kept_overflow_bound(OVER).await, not_smaller());
}

async fn swap_between_turns_bound(tokens: u64) -> HookOutcome {
    let calls = Calls::default();
    let store = MemoryStore::default();
    let summary = "s".repeat(SWAP_SUMMARY_CHARS);
    let provider = documented(vec![
        answer("Hello.", NEAR),
        compaction(&summary),
        answer("Again.", 300),
    ]);
    let agent = heard_by(
        background(&provider, &store),
        &head_body(),
        sized_at("background", tokens),
        &calls,
    )
    .await;
    let mut live = Live::start(agent);
    assert!(completed(&live.turn("u1", "first").await));
    live.until(prepared).await;
    assert!(completed(&live.turn("u2", "second").await));
    let (_, seen) = live.close().await;
    assert!(swapped_in(&seen, 2), "{seen:#?}");
    last_ran(&store)
}

const SWAP_SUMMARY_CHARS: usize = 55_281;

#[tokio::test]
async fn a_swap_between_turns_holds_the_head_texts_to_what_it_replaced() {
    assert_eq!(swap_between_turns_bound(UNDER).await, HookOutcome::Applied);
    assert_eq!(swap_between_turns_bound(OVER).await, not_smaller());
}

const RETRY_WINDOWS: &str = "[models.\"claude-opus-5-5\"]\ncontext_window = 1000000\n\n[endpoints.other.models.\"claude-opus-5-5\"]\ncontext_window = 110000\nnative_compaction = false\n";

async fn retry_convert_bound(tokens: u64) -> HookOutcome {
    let calls = Calls::default();
    let store = MemoryStore::default();
    let provider = scripted(vec![
        answer("Hello.", 1_000),
        compaction(SUMMARY),
        answer("Second.", 2_000),
        failing(server_error()),
        answer("Retried.", 1_000),
    ]);
    let agent = super::smaller::windows(&store, &provider, RETRY_WINDOWS);
    let sized = sized_at("convert", tokens);
    let script: HookScript = Arc::new(move |plugin, hook, event| {
        if let Event::BeforeCompact(info) = &event
            && info.trigger == "switch"
        {
            return Box::pin(async { Reply::Block("not now".into()) });
        }
        sized(plugin, hook, event)
    });
    let body = head_body() + &super::gate::gate_body("");
    let agent = heard_by(agent, &body, script, &calls).await;
    let mut live = Live::start(agent);
    live.turn("u1", "first").await;
    live.compact(2, None).await;
    assert!(completed(&live.turn("u2", "second").await));
    let failed = live.turn("u3", &big(RETRY_PENDING)).await;
    assert!(
        matches!(failed, TurnOutcome::Error { offer: Some(_), .. }),
        "{failed:?}"
    );
    live.send(ClientCommand::Retry {
        turn: 3,
        endpoint: Some("other".to_owned()),
        request: RequestId(9),
    })
    .await;
    let outcome = live
        .until(|e| matches!(e, AgentEvent::TurnEnded { .. }))
        .await;
    let (agent, _) = live.close().await;
    assert!(
        matches!(
            outcome,
            AgentEvent::TurnEnded {
                outcome: TurnOutcome::Completed { .. },
                ..
            }
        ),
        "{outcome:?}"
    );
    assert_eq!(agent.endpoint_name(), "other");
    assert_eq!(
        heads(&calls).last().map(|h| h.trigger.as_str()),
        Some("convert")
    );
    last_ran(&store)
}

const RETRY_PENDING: u64 = 100_883;

#[tokio::test]
async fn a_retrys_convert_holds_the_head_texts_to_the_new_window_less_the_turns_message() {
    assert_eq!(retry_convert_bound(UNDER).await, HookOutcome::Applied);
    assert_eq!(retry_convert_bound(OVER).await, not_smaller());
}
