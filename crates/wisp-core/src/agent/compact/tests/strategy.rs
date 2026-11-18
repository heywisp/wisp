use wisp_plugin_api::hooks::{Event, Reply};
use wisp_plugin_api::strategy::StrategyInfo;
use wisp_plugin_testkit::hooks::{
    self as kit, Answer, Calls, Script as HookScript, Strategy, StrategyCalled,
};
use wisp_protocol::settings::{Target, find};
use wisp_protocol::{CompactionFailure, HookOutcome};

use super::auto::assert_prefix_kept;
use super::background::{background, extends, swapped_in};
use super::gate::{ran, replying};
use super::hooks::with;
use super::overflow::{TOO_LONG, off, three_answers, too_long, wisp_off};
use super::*;
use crate::plugin::testing::hook_body;
use crate::plugin::{Make, PluginSwitches};
use crate::settings::Given;

const HOOK: &str = "aa/strategy";
const INSTRUCTION: &str = "Summarise in three short lines.";
const LAYOUT: &str = "<layout";
const NOTE: &str = "Head note.";
const REFUSAL: &str = "the summary names no files";

fn layout(info: &StrategyInfo, summary: Option<&str>) -> String {
    format!(
        "<layout epoch=\"{}\" turns=\"{}-{}\" native=\"{}\">{}</layout>",
        info.epoch,
        info.first_turn,
        info.last_turn,
        info.native,
        summary.unwrap_or("none")
    )
}

fn answered(called: StrategyCalled) -> Answer {
    match called {
        StrategyCalled::Instruction(_) => Answer::Instruction(Some(INSTRUCTION.to_owned())),
        StrategyCalled::Accept(..) => Answer::Accept(Ok(())),
        StrategyCalled::Head(info, _, summary) => {
            Answer::Head(vec![layout(&info, summary.as_deref())])
        }
    }
}

fn answering(answer: impl Fn(StrategyCalled) -> Answer + Send + Sync + 'static) -> Strategy {
    Arc::new(move |_, called| {
        let reply = answer(called);
        Box::pin(async move { reply })
    })
}

fn laying() -> Strategy {
    answering(answered)
}

fn choosing(mut agent: Agent, id: &str) -> Agent {
    agent.settings.session.push(Given {
        target: Target::Name(find("context.strategy").unwrap()),
        value: SettingValue::Text(id.to_owned()),
        origin: "session".to_owned(),
    });
    agent
}

async fn strategist(
    agent: Agent,
    (hooks, script): (&str, HookScript),
    section: Option<kit::Section>,
    strategy: Strategy,
    calls: &Calls,
) -> Agent {
    let make: Make = kit::strategising(script, section, strategy, calls.clone());
    let body = kit::strategy_body() + hooks;
    let agent = with(agent, vec![(kit::manifest_in("0.2.0", "aa", &body), make)]).await;
    choosing(agent, "aa")
}

async fn laid(agent: Agent, strategy: Strategy, calls: &Calls) -> Agent {
    strategist(agent, ("", kit::none()), None, strategy, calls).await
}

fn instructions(calls: &Calls) -> Vec<StrategyInfo> {
    let all = calls.strategies();
    let estimated = |at: usize, info: &StrategyInfo| {
        matches!(at.checked_sub(1).and_then(|before| all.get(before)),
            Some(StrategyCalled::Head(head, _, None)) if head == info)
    };
    all.iter()
        .enumerate()
        .filter_map(|(at, c)| match c {
            StrategyCalled::Instruction(info) if !estimated(at, info) => Some(info.clone()),
            _ => None,
        })
        .collect()
}

fn accepts(calls: &Calls) -> Vec<String> {
    pick!(calls.strategies(), StrategyCalled::Accept(_, summary) => summary)
}

fn built(calls: &Calls) -> Vec<(StrategyInfo, kit::Viewed, String)> {
    pick!(
        calls.strategies(),
        StrategyCalled::Head(info, viewed, Some(summary)) => (info, viewed, summary)
    )
}

fn native_heads(calls: &Calls, trigger: &str) -> Vec<StrategyInfo> {
    calls
        .strategies()
        .into_iter()
        .filter_map(|c| match c {
            StrategyCalled::Head(info, _, None) if info.compact.trigger == trigger => Some(info),
            _ => None,
        })
        .collect()
}

fn asked(calls: &Calls, trigger: &str) -> usize {
    calls
        .strategies()
        .into_iter()
        .filter(|c| {
            let (StrategyCalled::Instruction(info)
            | StrategyCalled::Accept(info, _)
            | StrategyCalled::Head(info, ..)) = c;
            info.compact.trigger == trigger
        })
        .count()
}

fn compacted(store: &MemoryStore) -> Vec<(Option<String>, Option<String>)> {
    pick!(
        log_events(store),
        LogEvent::ContextCompacted {
            strategy,
            instruction,
            ..
        } => (strategy, instruction)
    )
}

fn failed(store: &MemoryStore) -> Vec<(CompactionFailure, String, Option<String>, Option<String>)> {
    pick!(
        log_events(store),
        LogEvent::CompactionFailed {
            kind,
            message,
            strategy,
            instruction,
            ..
        } => (kind, message, strategy, instruction)
    )
}

fn sent(provider: &Fake, n: usize, text: &str) -> bool {
    let quoted = serde_json::to_string(text).unwrap();
    provider
        .body(n)
        .to_string()
        .contains(quoted.trim_matches('"'))
}

fn head_texts(agent: &Agent) -> Vec<String> {
    let head = agent.compaction.head.as_ref().unwrap();
    head.message
        .content
        .iter()
        .chain(&head.additions)
        .filter_map(|b| match b {
            ContentBlock::Text { text } => Some(text.clone()),
            _ => None,
        })
        .collect()
}

fn three() -> Vec<Step> {
    vec![
        answer("Hello.", 1_000),
        answer(WRITTEN, 1_200),
        answer("Again.", 300),
        answer("Once more.", 400),
    ]
}

async fn compacted_once(agent: Agent) -> (Agent, AgentEvent) {
    let mut live = Live::start(agent);
    assert!(completed(&live.turn("u1", "first").await));
    let done = live.compact(2, None).await;
    assert!(completed(&live.turn("u2", "second").await));
    let (agent, _) = live.close().await;
    (agent, done)
}

#[tokio::test]
async fn a_wisp_compact_sends_the_strategy_instruction_and_lays_out_the_head_with_its_texts() {
    let calls = Calls::default();
    let store = MemoryStore::default();
    let provider = scripted(three());
    let agent = laid(wisp_engine(&provider, &store), laying(), &calls).await;
    let mut live = Live::start(agent);
    assert!(completed(&live.turn("u1", "first").await));
    let done = live.compact(2, None).await;
    assert!(matches!(done, AgentEvent::Compacted { .. }), "{done:?}");
    assert!(completed(&live.turn("u2", "second").await));
    assert!(completed(&live.turn("u3", "third").await));
    let (agent, _) = live.close().await;
    let first = provider.body(1).to_string();
    assert!(first.contains(INSTRUCTION), "{first}");
    assert!(!sent(&provider, 1, &summary_instruction(None)), "{first}");
    let asked = instructions(&calls);
    assert_eq!(asked.len(), 1);
    assert_eq!(asked[0].compact.instruction, summary_instruction(None));
    assert!(!asked[0].native);
    let heads = built(&calls);
    assert_eq!(heads.len(), 1);
    assert_eq!(accepts(&calls), [heads[0].2.clone()]);
    let texts = head_texts(&agent);
    assert_eq!(texts, [layout(&heads[0].0, Some(&heads[0].2))]);
    assert!(!head_text(&agent).contains("<compacted_context"));
    assert_eq!(
        compacted(&store),
        [(Some("aa".to_owned()), Some(INSTRUCTION.to_owned()))]
    );
    assert_eq!(
        ran(&store, HOOK),
        [
            HookOutcome::Applied,
            HookOutcome::None,
            HookOutcome::Applied
        ]
    );
    assert!(sent(&provider, 2, &texts[0]));
    extends(&provider, 2, 3);
    assert_prefix_kept(&provider);
    assert_rebuilds(&store, &agent);
}

#[tokio::test]
async fn a_native_compact_sends_the_instruction_and_puts_the_layout_after_the_summary_block() {
    let calls = Calls::default();
    let store = MemoryStore::default();
    let provider = scripted(vec![
        answer("Hello.", 1_000),
        compaction(SUMMARY),
        answer("Again.", 300),
        answer("Once more.", 400),
    ]);
    let agent = laid(logged(provider.clone(), &store), laying(), &calls).await;
    let mut live = Live::start(agent);
    assert!(completed(&live.turn("u1", "first").await));
    let done = live.compact(2, None).await;
    assert!(matches!(done, AgentEvent::Compacted { .. }), "{done:?}");
    assert!(completed(&live.turn("u2", "second").await));
    assert!(completed(&live.turn("u3", "third").await));
    let (agent, _) = live.close().await;
    extends(&provider, 0, 1);
    extends(&provider, 2, 3);
    assert_prefix_kept(&provider);
    assert!(provider.body(1).to_string().contains(INSTRUCTION));
    assert!(instructions(&calls)[0].native);
    let heads = native_heads(&calls, "manual");
    assert_eq!(heads.len(), 1);
    assert!(heads[0].native);
    let blocks = &agent.compaction.head.as_ref().unwrap().message.content;
    assert!(
        matches!(blocks[0], ContentBlock::Opaque { .. }),
        "{blocks:?}"
    );
    assert_eq!(
        blocks[1..],
        [ContentBlock::Text {
            text: layout(&heads[0], None)
        }]
    );
    assert!(accepts(&calls).is_empty());
    assert_eq!(compacted(&store)[0].0.as_deref(), Some("aa"));
    assert_rebuilds(&store, &agent);
}

#[tokio::test]
async fn a_refused_summary_fails_the_compaction_and_logs_the_sentence_and_instruction_whole() {
    let calls = Calls::default();
    let store = MemoryStore::default();
    let provider = scripted(vec![
        answer("Hello.", 1_000),
        answer("A short summary.", 1_200),
        answer("Again.", 300),
    ]);
    let strategy = answering(|called| match called {
        StrategyCalled::Accept(..) => Answer::Accept(Err(REFUSAL.to_owned())),
        other => answered(other),
    });
    let agent = laid(wisp_engine(&provider, &store), strategy, &calls).await;
    let (agent, done) = compacted_once(agent).await;
    let message = format!(
        "The aa strategy refused the summary, so Wisp kept the conversation as it was: {REFUSAL}"
    );
    assert!(
        matches!(&done, AgentEvent::CompactionFailed { kind: CompactionFailure::CheckFailed, message: m, .. } if *m == message),
        "{done:?}"
    );
    assert_eq!(
        failed(&store),
        [(
            CompactionFailure::CheckFailed,
            message,
            Some("aa".to_owned()),
            Some(INSTRUCTION.to_owned())
        )]
    );
    assert!(built(&calls).is_empty());
    assert!(agent.compaction.head.is_none());
    assert_eq!(
        ran(&store, HOOK)[1],
        HookOutcome::Blocked {
            reason: REFUSAL.to_owned()
        }
    );
}

#[tokio::test]
async fn a_swap_between_turns_carries_the_strategy_instruction_and_layout() {
    let calls = Calls::default();
    let store = MemoryStore::default();
    let provider = documented(vec![
        answer("Hello.", super::background::NEAR),
        compaction(SUMMARY),
        answer("Again.", 300),
    ]);
    let agent = laid(background(&provider, &store), laying(), &calls).await;
    let mut live = Live::start(agent);
    assert!(completed(&live.turn("u1", "first").await));
    live.until(prepared).await;
    assert!(completed(&live.turn("u2", "second").await));
    let (agent, seen) = live.close().await;
    assert!(swapped_in(&seen, 2), "{seen:#?}");
    assert!(provider.body(1).to_string().contains(INSTRUCTION));
    assert_eq!(instructions(&calls).len(), 1);
    let heads = native_heads(&calls, "background");
    assert_eq!(heads.len(), 1);
    let texts = head_texts(&agent);
    assert_eq!(texts, [layout(&heads[0], None)]);
    assert!(sent(&provider, 2, &texts[0]));
    assert_eq!(
        compacted(&store),
        [(Some("aa".to_owned()), Some(INSTRUCTION.to_owned()))]
    );
    assert_rebuilds(&store, &agent);
}

#[tokio::test]
async fn a_kept_overflow_recovery_lays_out_its_head_with_the_strategy() {
    let calls = Calls::default();
    let store = MemoryStore::default();
    let mut steps = three_answers();
    steps.extend([
        too_long(TOO_LONG),
        compaction(SUMMARY),
        answer("Four.", 900),
    ]);
    let provider = scripted(steps);
    let agent = laid(off(&provider, &store), laying(), &calls).await;
    let mut live = Live::start(agent);
    three_turns(&mut live).await;
    assert!(completed(&live.turn("u4", "fourth").await));
    let (agent, _) = live.close().await;
    let heads = native_heads(&calls, "overflow");
    assert_eq!(heads.len(), 1);
    let last = &heads[0];
    assert_eq!(last.compact.trigger, "overflow");
    assert!(provider.body(4).to_string().contains(INSTRUCTION));
    assert!(head_texts(&agent).contains(&layout(last, None)));
    assert_eq!(compacted(&store)[0].0.as_deref(), Some("aa"));
    assert_rebuilds(&store, &agent);
}

#[tokio::test]
async fn a_wisp_overflow_recovery_lays_out_its_head_with_the_strategy() {
    let calls = Calls::default();
    let store = MemoryStore::default();
    let mut steps = three_answers();
    steps.extend([
        too_long(TOO_LONG),
        answer(WRITTEN, 1_200),
        answer("Four.", 900),
    ]);
    let provider = scripted(steps);
    let agent = laid(wisp_off(&provider, &store), laying(), &calls).await;
    let mut live = Live::start(agent);
    three_turns(&mut live).await;
    assert!(completed(&live.turn("u4", "fourth").await));
    let (agent, _) = live.close().await;
    let heads = built(&calls);
    assert_eq!(heads.len(), 1);
    assert_eq!(heads[0].0.compact.trigger, "overflow");
    assert!(provider.body(4).to_string().contains(INSTRUCTION));
    assert!(head_text(&agent).contains(&layout(&heads[0].0, Some(&heads[0].2))));
    assert!(!head_text(&agent).contains("<compacted_context"));
    assert_rebuilds(&store, &agent);
}

struct Rig {
    store: MemoryStore,
    provider: Arc<Fake>,
}

impl Rig {
    fn new() -> Self {
        Self {
            store: MemoryStore::default(),
            provider: scripted(three()),
        }
    }

    fn wisp(&self) -> Agent {
        wisp_engine(&self.provider, &self.store)
    }

    async fn falls_back(&self, agent: Agent, hook: &str) {
        let (agent, done) = compacted_once(agent).await;
        assert!(matches!(done, AgentEvent::Compacted { .. }), "{done:?}");
        let head = head_text(&agent);
        assert!(head.contains("<compacted_context"), "{head}");
        assert!(!head.contains(LAYOUT), "{head}");
        let ran = ran(&self.store, hook);
        assert_eq!(ran.len(), 1, "{ran:?}");
        assert_eq!(compacted(&self.store)[0].0, None);
    }
}

#[tokio::test]
async fn a_panicking_strategy_is_asked_once_and_wisp_uses_its_own_layout() {
    let calls = Calls::default();
    let rig = Rig::new();
    let strategy: Strategy = Arc::new(|_, _| Box::pin(async { panic!("boom") }));
    let agent = laid(rig.wisp(), strategy, &calls).await;
    rig.falls_back(agent, HOOK).await;
    assert!(matches!(
        ran(&rig.store, HOOK)[0],
        HookOutcome::Failed { .. }
    ));
    assert_eq!(asked(&calls, "manual"), 1);
    assert_eq!(asked(&calls, "auto"), 2);
    assert!(!rig.provider.body(1).to_string().contains(INSTRUCTION));
}

#[tokio::test(start_paused = true)]
async fn a_strategy_past_its_time_is_stopped_once_and_wisp_uses_its_own_layout() {
    let calls = Calls::default();
    let rig = Rig::new();
    let strategy: Strategy = Arc::new(|_, _| {
        Box::pin(async {
            tokio::time::sleep(Duration::from_secs(3_600)).await;
            Answer::Instruction(None)
        })
    });
    let agent = laid(rig.wisp(), strategy, &calls).await;
    rig.falls_back(agent, HOOK).await;
    assert_eq!(ran(&rig.store, HOOK), [HookOutcome::TimedOut]);
    assert_eq!(asked(&calls, "manual"), 1);
    assert_eq!(asked(&calls, "auto"), 2);
}

#[tokio::test]
async fn an_oversized_head_is_rejected_once_and_wisp_uses_its_own_layout_with_the_instruction() {
    let calls = Calls::default();
    let strategy = answering(|called| match called {
        StrategyCalled::Head(..) => Answer::Head(vec!["x".repeat(70_000)]),
        other => answered(other),
    });
    let store = MemoryStore::default();
    let provider = scripted(three());
    let agent = laid(wisp_engine(&provider, &store), strategy, &calls).await;
    let (agent, _) = compacted_once(agent).await;
    assert!(provider.body(1).to_string().contains(INSTRUCTION));
    let outcomes = ran(&store, HOOK);
    assert_eq!(
        outcomes.last(),
        Some(&HookOutcome::Rejected {
            rule: "too_large".into()
        }),
        "{outcomes:?}"
    );
    assert_eq!(outcomes.len(), 3, "{outcomes:?}");
    assert!(head_text(&agent).contains("<compacted_context"));
    assert_eq!(compacted(&store), [(None, Some(INSTRUCTION.to_owned()))]);
}

#[tokio::test]
async fn a_strategy_that_is_not_installed_or_is_off_gets_one_notice_and_wisp_uses_its_own_layout() {
    let rig = Rig::new();
    rig.falls_back(choosing(rig.wisp(), "zz"), "zz/strategy")
        .await;
    assert!(matches!(
        &ran(&rig.store, "zz/strategy")[0],
        HookOutcome::Failed { message } if message == "the zz plugin isn't installed, so Wisp used its own compaction layout"
    ));

    let calls = Calls::default();
    let rig = Rig::new();
    let switches = PluginSwitches {
        off: ["aa".to_owned()].into_iter().collect(),
        removed: std::collections::BTreeSet::new(),
    };
    let agent = laid(rig.wisp(), laying(), &calls)
        .await
        .with_plugin_switches(&switches);
    rig.falls_back(agent, HOOK).await;
    assert!(matches!(
        &ran(&rig.store, HOOK)[0],
        HookOutcome::Failed { message } if message == "the aa plugin is off, so Wisp used its own compaction layout"
    ));
    assert!(calls.strategies().is_empty());
}

fn c2_body() -> String {
    hook_body("gate", "before-compact", "") + &hook_body("head", "compaction-head", "")
}

#[tokio::test]
async fn the_compaction_hooks_see_the_strategy_instruction_and_add_after_its_layout() {
    let calls = Calls::default();
    let store = MemoryStore::default();
    let provider = scripted(three());
    let script = replying(vec![("head", Reply::Append(vec![NOTE.into()]))]);
    let agent = strategist(
        wisp_engine(&provider, &store),
        (&c2_body(), script),
        None,
        laying(),
        &calls,
    )
    .await;
    let (agent, _) = compacted_once(agent).await;
    let gate = calls
        .of("gate")
        .into_iter()
        .find_map(|c| match c.event {
            Event::BeforeCompact(info) => Some(info),
            _ => None,
        })
        .unwrap();
    assert_eq!(gate.instruction, INSTRUCTION);
    let texts = head_texts(&agent);
    let heads = built(&calls);
    assert_eq!(
        texts,
        [layout(&heads[0].0, Some(&heads[0].2)), NOTE.to_owned()]
    );
    assert_eq!(ran(&store, "aa/head"), [HookOutcome::Applied]);
    assert_eq!(accepts(&calls).len(), 1);
}

#[tokio::test]
async fn a_rewritten_instruction_skips_the_accept_call_and_keeps_the_layout() {
    let calls = Calls::default();
    let store = MemoryStore::default();
    let provider = scripted(vec![
        answer("Hello.", 1_000),
        answer("A short summary.", 1_200),
        answer("Again.", 300),
    ]);
    let script = replying(vec![("gate", Reply::Rewrite("Summarise briefly.".into()))]);
    let strategy = answering(|called| match called {
        StrategyCalled::Accept(..) => Answer::Accept(Err(REFUSAL.to_owned())),
        other => answered(other),
    });
    let body = hook_body("gate", "before-compact", "");
    let agent = strategist(
        wisp_engine(&provider, &store),
        (&body, script),
        None,
        strategy,
        &calls,
    )
    .await;
    let (agent, done) = compacted_once(agent).await;
    assert!(matches!(done, AgentEvent::Compacted { .. }), "{done:?}");
    assert!(provider.body(1).to_string().contains("Summarise briefly."));
    assert!(accepts(&calls).is_empty());
    assert!(head_text(&agent).contains(LAYOUT));
}

#[tokio::test]
async fn a_layout_no_smaller_than_the_conversation_fails_the_compaction_and_never_falls_back() {
    let calls = Calls::default();
    let store = MemoryStore::default();
    let provider = scripted(three());
    let strategy = answering(|called| match called {
        StrategyCalled::Head(..) => Answer::Head(vec!["word ".repeat(12_000)]),
        other => answered(other),
    });
    let agent = laid(wisp_engine(&provider, &store), strategy, &calls).await;
    let (agent, done) = compacted_once(agent).await;
    assert!(
        matches!(&done, AgentEvent::CompactionFailed { kind: CompactionFailure::CheckFailed, message, .. } if message.starts_with("The summary would leave about")),
        "{done:?}"
    );
    assert!(agent.compaction.head.is_none());
    let failures = failed(&store);
    assert_eq!(failures.len(), 1);
    assert_eq!(failures[0].2.as_deref(), Some("aa"));
    assert_eq!(ran(&store, HOOK).last(), Some(&HookOutcome::Applied));
}

#[tokio::test]
async fn the_head_sections_come_before_the_strategy_layout() {
    let calls = Calls::default();
    let store = MemoryStore::default();
    let provider = scripted(three());
    let knob = kit::knob("Lead note v1.");
    let body = kit::prompt_section("notes", "lead", "");
    let agent = strategist(
        wisp_engine(&provider, &store),
        (&body, kit::none()),
        Some(kit::changing(knob)),
        laying(),
        &calls,
    )
    .await;
    let (agent, _) = compacted_once(agent).await;
    let head = head_text(&agent);
    let section = head.find("Lead note v1.").unwrap();
    let laid = head.find(LAYOUT).unwrap();
    assert!(section < laid, "{head}");
}

#[tokio::test]
async fn every_head_call_gets_a_view_of_the_history_as_it_is_then() {
    let calls = Calls::default();
    let store = MemoryStore::default();
    let provider = scripted(vec![
        answer("Hello.", 1_000),
        answer(WRITTEN, 1_200),
        answer("Again.", 300),
        answer(WRITTEN, 1_200),
    ]);
    let agent = laid(wisp_engine(&provider, &store), laying(), &calls).await;
    let mut live = Live::start(agent);
    assert!(completed(&live.turn("u1", "first").await));
    live.compact(2, None).await;
    assert!(completed(&live.turn("u2", "second").await));
    live.compact(3, None).await;
    live.close().await;
    let heads = built(&calls);
    assert_eq!(heads.len(), 2);
    let said =
        |n: usize| -> Vec<String> { heads[n].1.said.iter().map(|s| s.text.clone()).collect() };
    assert!(said(0).iter().any(|t| t.contains("first")), "{:?}", said(0));
    assert!(
        !said(0).iter().any(|t| t.contains("second")),
        "{:?}",
        said(0)
    );
    assert!(
        said(1).iter().any(|t| t.contains("second")),
        "{:?}",
        said(1)
    );
    assert_eq!(heads[1].1.log_pointer, heads[0].1.log_pointer);
}

#[tokio::test]
async fn the_head_estimate_comes_from_a_layout_cached_at_the_last_refresh() {
    let calls = Calls::default();
    let store = MemoryStore::default();
    let provider = scripted(vec![answer("Hello.", 1_000)]);
    let agent = laid(wisp_engine(&provider, &store), laying(), &calls).await;
    let mut live = Live::start(agent);
    assert!(completed(&live.turn("u1", "first").await));
    let (agent, _) = live.close().await;
    let cached = agent.strategy_estimate().unwrap().to_vec();
    let estimates: Vec<StrategyInfo> = pick!(
        calls.strategies(),
        StrategyCalled::Head(info, _, None) => info
    );
    assert_eq!(cached, [layout(estimates.last().unwrap(), None)]);
    let asked = calls.strategies().len();
    let tokens = agent.layout_tokens(1, (1, 1));
    assert_eq!(tokens, crate::context::text_tokens(&cached[0]));
    assert_ne!(
        tokens,
        crate::context::text_tokens(&agent.compacted_context(1, (1, 1), None))
    );
    assert_eq!(calls.strategies().len(), asked);
    assert!(ran(&store, HOOK).is_empty());
}

#[tokio::test]
async fn the_strategy_setting_is_read_when_a_turn_begins() {
    let calls = Calls::default();
    let store = MemoryStore::default();
    let provider = scripted(vec![answer("Hello.", 1_000)]);
    let agent = laid(wisp_engine(&provider, &store), laying(), &calls).await;
    assert_eq!(agent.chosen_strategy(), None);
    let mut live = Live::start(agent);
    assert!(completed(&live.turn("u1", "first").await));
    let (agent, _) = live.close().await;
    assert_eq!(agent.chosen_strategy(), Some("aa"));
}

#[tokio::test]
async fn a_failed_accept_after_a_supplied_instruction_keeps_the_summary_with_wisps_layout() {
    let calls = Calls::default();
    let store = MemoryStore::default();
    let provider = scripted(vec![
        answer("Hello.", 1_000),
        answer("A short summary.", 1_200),
        answer("Again.", 300),
    ]);
    let strategy: Strategy = Arc::new(|_, called| match called {
        StrategyCalled::Accept(..) => Box::pin(async { panic!("boom") }),
        other => {
            let reply = answered(other);
            Box::pin(async move { reply })
        }
    });
    let agent = laid(wisp_engine(&provider, &store), strategy, &calls).await;
    let (agent, done) = compacted_once(agent).await;
    assert!(matches!(done, AgentEvent::Compacted { .. }), "{done:?}");
    let head = head_text(&agent);
    assert!(head.contains("A short summary."), "{head}");
    assert!(head.contains("<compacted_context"), "{head}");
    assert!(built(&calls).is_empty());
    let outcomes = ran(&store, HOOK);
    assert_eq!(outcomes.len(), 2, "{outcomes:?}");
    assert!(
        matches!(outcomes[1], HookOutcome::Failed { .. }),
        "{outcomes:?}"
    );
    assert_eq!(compacted(&store), [(None, Some(INSTRUCTION.to_owned()))]);
}

#[tokio::test]
async fn a_ready_precompute_whose_strategy_was_switched_off_swaps_in_with_wisps_layout() {
    let calls = Calls::default();
    let store = MemoryStore::default();
    let provider = documented(vec![
        answer("Hello.", super::background::NEAR),
        compaction(SUMMARY),
        answer("Again.", 300),
    ]);
    let agent = laid(background(&provider, &store), laying(), &calls).await;
    let mut live = Live::start(agent);
    assert!(completed(&live.turn("u1", "first").await));
    live.until(prepared).await;
    live.send(crate::plugin::testing::plugin_switch(
        9,
        "aa",
        wisp_protocol::settings::ENABLED_KEY,
        false,
    ))
    .await;
    live.until(|e| {
        matches!(
            e,
            AgentEvent::SettingsChanged {
                answers: Some(RequestId(9)),
                ..
            }
        )
    })
    .await;
    assert!(completed(&live.turn("u2", "second").await));
    let (agent, seen) = live.close().await;
    assert!(swapped_in(&seen, 2), "{seen:#?}");
    assert!(provider.body(1).to_string().contains(INSTRUCTION));
    assert!(native_heads(&calls, "background").is_empty());
    let texts = head_texts(&agent).join("");
    assert!(texts.contains("<compacted_context"), "{texts}");
    let outcomes = ran(&store, HOOK);
    assert_eq!(
        outcomes,
        [
            HookOutcome::Applied,
            HookOutcome::Failed {
                message: "the aa plugin is off, so Wisp used its own compaction layout".to_owned()
            }
        ]
    );
    assert_rebuilds(&store, &agent);
}

fn emptying() -> Strategy {
    answering(|called| match called {
        StrategyCalled::Head(..) => Answer::Head(Vec::new()),
        other => answered(other),
    })
}

#[tokio::test]
async fn an_empty_head_on_wisps_engine_falls_back_to_wisps_layout_with_a_notice() {
    let calls = Calls::default();
    let store = MemoryStore::default();
    let provider = scripted(three());
    let agent = laid(wisp_engine(&provider, &store), emptying(), &calls).await;
    let (agent, done) = compacted_once(agent).await;
    assert!(matches!(done, AgentEvent::Compacted { .. }), "{done:?}");
    let head = head_text(&agent);
    assert!(head.contains("<compacted_context"), "{head}");
    assert!(head.contains("Say hello."), "{head}");
    assert_eq!(
        ran(&store, HOOK).last(),
        Some(&HookOutcome::Failed {
            message: "the aa strategy gave no head texts, so Wisp used its own compaction layout"
                .to_owned()
        })
    );
    assert_eq!(compacted(&store)[0].0, None);
}

#[tokio::test]
async fn an_empty_head_on_the_native_engine_leaves_the_block_alone() {
    let calls = Calls::default();
    let store = MemoryStore::default();
    let provider = scripted(vec![
        answer("Hello.", 1_000),
        compaction(SUMMARY),
        answer("Again.", 300),
    ]);
    let agent = laid(logged(provider.clone(), &store), emptying(), &calls).await;
    let (agent, done) = compacted_once(agent).await;
    assert!(matches!(done, AgentEvent::Compacted { .. }), "{done:?}");
    let blocks = &agent.compaction.head.as_ref().unwrap().message.content;
    assert!(
        matches!(blocks[..], [ContentBlock::Opaque { .. }]),
        "{blocks:?}"
    );
    assert_eq!(ran(&store, HOOK).last(), Some(&HookOutcome::None));
    assert_eq!(compacted(&store)[0].0.as_deref(), Some("aa"));
}

#[tokio::test]
async fn esc_during_a_strategy_call_logs_it_cancelled() {
    let calls = Calls::default();
    let store = MemoryStore::default();
    let provider = scripted(three());
    let started = Arc::new(tokio::sync::Notify::new());
    let held = started.clone();
    let strategy: Strategy = Arc::new(move |_, called| match called {
        StrategyCalled::Head(_, _, Some(_)) => {
            let held = held.clone();
            Box::pin(async move {
                held.notify_one();
                std::future::pending::<Answer>().await
            })
        }
        other => {
            let reply = answered(other);
            Box::pin(async move { reply })
        }
    });
    let agent = laid(wisp_engine(&provider, &store), strategy, &calls).await;
    let mut live = Live::start(agent);
    assert!(completed(&live.turn("u1", "first").await));
    let commands = live.commands.clone().unwrap();
    let canceller = tokio::spawn(async move {
        within("the strategy call's start", started.notified()).await;
        commands.send(ClientCommand::Cancel).await.unwrap();
    });
    let done = live.compact(2, None).await;
    canceller.await.unwrap();
    let (agent, _) = live.close().await;
    assert!(matches!(done, AgentEvent::Compacted { .. }), "{done:?}");
    assert!(head_text(&agent).contains("<compacted_context"));
    assert_eq!(ran(&store, HOOK).last(), Some(&HookOutcome::Cancelled));
}

fn sized(tokens: usize, c: &str) -> String {
    c.repeat(tokens * 3)
}

async fn appended_beside_a_large_layout(note: usize) -> Vec<HookOutcome> {
    let calls = Calls::default();
    let store = MemoryStore::default();
    let provider = scripted(three());
    let strategy = answering(|called| match called {
        StrategyCalled::Head(..) => Answer::Head(vec![sized(600, "s")]),
        other => answered(other),
    });
    let script = replying(vec![("head", Reply::Append(vec![sized(note, "n")]))]);
    let body = hook_body("head", "compaction-head", "");
    let agent = strategist(
        wisp_engine(&provider, &store),
        (&body, script),
        None,
        strategy,
        &calls,
    )
    .await;
    let (_, done) = compacted_once(agent).await;
    assert!(matches!(done, AgentEvent::Compacted { .. }), "{done:?}");
    ran(&store, "aa/head")
}

#[tokio::test]
async fn the_head_hooks_room_is_corrected_by_how_far_the_strategy_layout_moved_from_wisps() {
    assert_eq!(
        appended_beside_a_large_layout(200).await,
        [HookOutcome::Applied]
    );
    assert_eq!(
        appended_beside_a_large_layout(800).await,
        [HookOutcome::Rejected {
            rule: "not_smaller".into()
        }]
    );
}

#[tokio::test]
async fn a_strategy_layout_smaller_than_wisps_would_be_passes_where_wisps_check_would_fail() {
    let calls = Calls::default();
    let store = MemoryStore::default();
    let long = "word ".repeat(1_500);
    let provider = scripted(vec![
        answer("Hello.", 1_000),
        answer(&long, 1_200),
        answer("Again.", 300),
    ]);
    let strategy = answering(|called| match called {
        StrategyCalled::Head(..) => Answer::Head(vec!["<layout>short</layout>".to_owned()]),
        other => answered(other),
    });
    let agent = laid(wisp_engine(&provider, &store), strategy, &calls).await;
    let (agent, done) = compacted_once(agent).await;
    assert!(matches!(done, AgentEvent::Compacted { .. }), "{done:?}");
    assert_eq!(head_texts(&agent), ["<layout>short</layout>"]);
}

#[tokio::test]
async fn a_swap_lays_out_the_sections_before_the_strategy_layout() {
    let calls = Calls::default();
    let store = MemoryStore::default();
    let provider = documented(vec![
        answer("Hello.", super::background::NEAR),
        compaction(SUMMARY),
        answer("Again.", 300),
    ]);
    let body = kit::prompt_section("notes", "lead", "");
    let agent = strategist(
        background(&provider, &store),
        (&body, kit::none()),
        Some(kit::changing(kit::knob("Lead note v1."))),
        laying(),
        &calls,
    )
    .await;
    let mut live = Live::start(agent);
    assert!(completed(&live.turn("u1", "first").await));
    live.until(prepared).await;
    assert!(completed(&live.turn("u2", "second").await));
    let (agent, seen) = live.close().await;
    assert!(swapped_in(&seen, 2), "{seen:#?}");
    let texts = head_texts(&agent);
    let section = texts
        .iter()
        .position(|t| t.contains("Lead note v1."))
        .unwrap();
    let laid = texts.iter().position(|t| t.starts_with(LAYOUT)).unwrap();
    assert!(section < laid, "{texts:?}");
}

#[tokio::test]
async fn a_precompute_whose_strategy_is_no_longer_chosen_swaps_in_with_wisps_layout() {
    let calls = Calls::default();
    let store = MemoryStore::default();
    let precompute = super::background::Landing::new();
    let step = Arc::new(Semaphore::new(0));
    let provider = documented(vec![
        answer("Hello.", super::background::NEAR),
        precompute.gated(compaction(SUMMARY)),
        super::background::tool_call(super::background::NEAR + 200, "Looking.").gated(&step),
        answer("Done.", 400),
    ]);
    let agent = laid(background(&provider, &store), laying(), &calls).await;
    let mut live = Live::start(agent);
    assert!(completed(&live.turn("u1", "first").await));
    live.send(ClientCommand::ChangeSettings {
        request: RequestId(9),
        changes: vec![SettingChange {
            target: wisp_protocol::settings::SettingTarget::Name(
                wisp_protocol::settings::SettingName::parse("context.strategy").unwrap(),
            ),
            value: Some(SettingValue::Text("default".to_owned())),
        }],
        save: false,
    })
    .await;
    live.until(|e| {
        matches!(
            e,
            AgentEvent::SettingsChanged {
                answers: Some(RequestId(9)),
                ..
            }
        )
    })
    .await;
    live.send(send("u2", "second")).await;
    live.until(|e| matches!(e, AgentEvent::StepStarted { step: 1 }))
        .await;
    precompute.land().await;
    step.add_permits(1);
    let ended = live
        .until(|e| matches!(e, AgentEvent::TurnEnded { .. }))
        .await;
    assert!(
        matches!(
            ended,
            AgentEvent::TurnEnded {
                outcome: TurnOutcome::Completed { .. },
                ..
            }
        ),
        "{ended:#?}"
    );
    let (agent, seen) = live.close().await;
    assert!(seen.iter().any(|e| matches!(
        e,
        AgentEvent::Compacted {
            trigger: CompactionTrigger::Background,
            turn: Some(2),
            ..
        }
    )));
    assert!(provider.body(1).to_string().contains(INSTRUCTION));
    assert!(native_heads(&calls, "background").is_empty());
    let texts = head_texts(&agent).join("");
    assert!(texts.contains("<compacted_context"), "{texts}");
    assert!(!texts.contains(LAYOUT), "{texts}");
    assert_eq!(
        ran(&store, HOOK),
        [
            HookOutcome::Applied,
            HookOutcome::Failed {
                message: "the aa plugin is no longer the chosen strategy, so Wisp used its own compaction layout".to_owned()
            }
        ]
    );
    assert_rebuilds(&store, &agent);
}

#[tokio::test]
async fn an_idle_compact_estimates_again_for_the_next_epoch() {
    let calls = Calls::default();
    let store = MemoryStore::default();
    let provider = scripted(vec![answer("Hello.", 1_000), answer(WRITTEN, 1_200)]);
    let agent = laid(wisp_engine(&provider, &store), laying(), &calls).await;
    let mut live = Live::start(agent);
    assert!(completed(&live.turn("u1", "first").await));
    let done = live.compact(2, None).await;
    assert!(matches!(done, AgentEvent::Compacted { .. }), "{done:?}");
    let (agent, _) = live.close().await;
    assert_eq!(agent.next_epoch(), 2);
    let cached = agent.strategy_estimate().unwrap().concat();
    assert!(cached.contains("epoch=\"2\""), "{cached}");
}

#[tokio::test]
async fn a_saved_default_strategy_compacts_through_the_compaction_plugin() {
    use crate::agent::compact::strategy::{DEFAULT_PLUGIN, DEFAULT_STRATEGY};
    use wisp_protocol::settings::{Place, SettingValue, SitePath, names};
    let store = MemoryStore::default();
    let provider = scripted(three());
    let mut layers = crate::settings::host::StartLayers::default();
    let spec = find(names::CONTEXT_STRATEGY).unwrap();
    layers.saved.insert(
        SitePath::new(Place::Global, spec),
        SettingValue::Text(DEFAULT_STRATEGY.to_owned()),
        1,
    );
    let agent = wisp_engine(&provider, &store).with_start_layers(layers);
    let (agent, done) = compacted_once(agent).await;
    assert!(matches!(done, AgentEvent::Compacted { .. }), "{done:?}");
    assert_eq!(agent.chosen_strategy(), Some(DEFAULT_PLUGIN));
    let instruction = wisp_plugin_compaction::summary_instruction(None);
    assert!(sent(&provider, 1, &instruction));
    assert!(!sent(&provider, 1, &summary_instruction(None)));
    assert_eq!(
        compacted(&store),
        [(Some(DEFAULT_PLUGIN.to_owned()), Some(instruction))]
    );
    let head = head_text(&agent);
    assert!(head.contains("<user_messages>"), "{head}");
    assert_eq!(
        ran(&store, "compaction/strategy"),
        [
            HookOutcome::Applied,
            HookOutcome::None,
            HookOutcome::Applied
        ]
    );
}

#[tokio::test]
async fn the_compaction_plugin_switched_off_gives_the_cores_minimal_head_with_a_notice() {
    use crate::agent::compact::strategy::DEFAULT_PLUGIN;
    use wisp_protocol::settings::ENABLED_KEY;
    let store = MemoryStore::default();
    let provider = scripted(three());
    let layers = crate::settings::host::StartLayers {
        flags: vec![crate::plugin::testing::plugin_flag(
            DEFAULT_PLUGIN,
            ENABLED_KEY,
            false,
        )],
        ..Default::default()
    };
    let agent = wisp_engine(&provider, &store).with_start_layers(layers);
    let (agent, done) = compacted_once(agent).await;
    assert!(matches!(done, AgentEvent::Compacted { .. }), "{done:?}");
    let fallback = summary_instruction(None);
    assert!(sent(&provider, 1, &fallback));
    assert!(!sent(
        &provider,
        1,
        &wisp_plugin_compaction::summary_instruction(None)
    ));
    let head = head_text(&agent);
    assert!(head.contains("<compacted_context"), "{head}");
    assert!(head.contains("<summary>"), "{head}");
    for gone in ["<user_messages>", "<tool_calls>", "<pointers>"] {
        assert!(!head.contains(gone), "{gone}: {head}");
    }
    assert_eq!(
        ran(&store, "compaction/strategy"),
        [HookOutcome::Failed {
            message: "the compaction plugin is off, so Wisp used its own compaction layout"
                .to_owned()
        }]
    );
    assert_eq!(compacted(&store), [(None, None)]);
}

#[tokio::test]
async fn a_hooks_compaction_request_runs_through_the_strategy_and_the_compaction_hooks() {
    let calls = Calls::default();
    let store = MemoryStore::default();
    let provider = scripted(three());
    let script = super::threshold::answering(|hook, event| match (hook, event) {
        ("ask", Event::ContextThreshold(_)) => Reply::Compact(Some(super::threshold::KEEP.into())),
        ("head", Event::CompactionHead(_)) => Reply::Append(vec![NOTE.into()]),
        _ => Reply::None,
    });
    let body = super::threshold::ask_body("") + &c2_body();
    let agent = strategist(
        wisp_engine(&provider, &store),
        (&body, script),
        None,
        laying(),
        &calls,
    )
    .await;
    let mut live = Live::start(agent);
    assert!(completed(&live.turn("u1", "first").await));
    assert!(completed(&live.turn("u2", "second").await));
    let (agent, _) = live.close().await;
    let asked = instructions(&calls);
    assert_eq!(asked.len(), 1);
    assert_eq!(asked[0].compact.trigger, "auto");
    assert_eq!(
        asked[0].compact.focus.as_deref(),
        Some(super::threshold::KEEP)
    );
    assert_eq!(
        asked[0].compact.requested_by.as_deref(),
        Some(super::threshold::ASK)
    );
    assert_eq!(
        asked[0].compact.instruction,
        summary_instruction(Some(super::threshold::KEEP))
    );
    assert!(provider.body(1).to_string().contains(INSTRUCTION));
    let gate = calls
        .of("gate")
        .into_iter()
        .find_map(|c| match c.event {
            Event::BeforeCompact(info) => Some(info),
            _ => None,
        })
        .unwrap();
    assert_eq!(gate.instruction, INSTRUCTION);
    assert_eq!(gate.requested_by.as_deref(), Some(super::threshold::ASK));
    let heads = built(&calls);
    assert_eq!(
        head_texts(&agent),
        [layout(&heads[0].0, Some(&heads[0].2)), NOTE.to_owned()]
    );
    assert_eq!(
        super::threshold::requests(&store),
        [(
            Some(super::threshold::KEEP.to_owned()),
            Some(super::threshold::ASK.to_owned())
        )]
    );
    assert_eq!(
        compacted(&store),
        [(Some("aa".to_owned()), Some(INSTRUCTION.to_owned()))]
    );
    assert_eq!(provider.sent().len(), 3);
}
