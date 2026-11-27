use std::collections::BTreeSet;

use tokio::sync::Notify;
use wisp_plugin_api::hooks::{CompactedInfo, Event, Reply, RewindInfo};
use wisp_plugin_testkit::hooks::{self as kit, Calls, Script as HookScript};
use wisp_protocol::{AgentPath, HookOutcome, ParentLink, SpawnChoices};

use super::auto::{LOW, no_retries};
use super::background::background;
use super::marker::{change, other_without_native};
use super::*;
use crate::agent::compact::CANCELLED_BEFORE_START;
use crate::agent::tool_tests::rewind;
use crate::plugin::{KernelHost, Make, PluginConfig, Registry};

const HEARD: &str = "\n[[hooks]]\nname = \"compacted\"\npoint = \"after-compact\"\n\n[[hooks]]\nname = \"back\"\npoint = \"rewound\"\n";

const COMPACTED: &str = "aa/compacted";
const BACK: &str = "aa/back";

pub(super) async fn with(agent: Agent, mut sources: Vec<(String, Make)>) -> Agent {
    let compaction = wisp_plugin_compaction::bundled();
    let make: Make = Arc::new(move |host| (compaction.instantiate)(host));
    sources.push((compaction.manifest.to_owned(), make));
    let (registry, problems) = Registry::from_sources(sources, &PluginConfig::default());
    assert!(problems.is_empty(), "{problems:?}");
    let started = registry
        .start(&BTreeSet::new(), Arc::new(KernelHost::new()))
        .await;
    agent.with_hooks(started.hooks, started.host)
}

pub(super) async fn heard_by(agent: Agent, body: &str, script: HookScript, calls: &Calls) -> Agent {
    let make: Make = kit::instantiate(script, calls.clone());
    with(agent, vec![(kit::manifest("aa", body), make)]).await
}

async fn heard(agent: Agent, calls: &Calls) -> Agent {
    heard_by(agent, HEARD, kit::none(), calls).await
}

fn compactions(calls: &Calls) -> Vec<CompactedInfo> {
    calls
        .of("compacted")
        .into_iter()
        .filter_map(|c| match c.event {
            Event::AfterCompact(info) => Some(info),
            _ => None,
        })
        .collect()
}

fn rewinds(calls: &Calls) -> Vec<RewindInfo> {
    calls
        .of("back")
        .into_iter()
        .filter_map(|c| match c.event {
            Event::Rewound(info) => Some(info),
            _ => None,
        })
        .collect()
}

fn sequence(store: &MemoryStore) -> Vec<String> {
    log_events(store)
        .into_iter()
        .filter_map(|e| match e {
            LogEvent::ContextCompacted { trigger, .. } => Some(trigger.as_str().to_owned()),
            LogEvent::CompactionFailed { trigger, .. } => {
                Some(format!("{} failed", trigger.as_str()))
            }
            LogEvent::Rewound { .. } => Some("rewound".to_owned()),
            LogEvent::HookRan { hook, .. } if hook == COMPACTED || hook == BACK => Some(hook),
            _ => None,
        })
        .collect()
}

fn one_per_record(store: &MemoryStore, calls: &Calls) -> Vec<CompactedInfo> {
    let mut owed = 0_usize;
    let mut logged = Vec::new();
    for entry in sequence(store) {
        match entry.as_str() {
            COMPACTED => {
                assert!(owed > 0, "{:?}", sequence(store));
                owed -= 1;
            }
            BACK | "rewound" => {}
            record => {
                owed += 1;
                logged.push(record.to_owned());
            }
        }
    }
    assert_eq!(owed, 0, "{:?}", sequence(store));
    let infos = compactions(calls);
    let fired: Vec<String> = infos
        .iter()
        .map(|i| match i.failed {
            Some(_) => format!("{} failed", i.trigger),
            None => i.trigger.clone(),
        })
        .collect();
    assert_eq!(fired, logged);
    infos
}

pub(super) fn ran_at(hook: &str) -> impl Fn(&AgentEvent) -> bool + '_ {
    move |e| matches!(e, AgentEvent::HookRan { hook: h, .. } if h == hook)
}

fn compacted_record(store: &MemoryStore) -> LogEvent {
    log_events(store)
        .into_iter()
        .find(|e| matches!(e, LogEvent::ContextCompacted { .. }))
        .unwrap()
}

#[tokio::test]
async fn a_manual_compact_fires_after_compact_once_with_the_records_figures_after_it() {
    let calls = Calls::default();
    let store = MemoryStore::default();
    let provider = scripted(vec![answer("Hello.", 1_000), compaction(SUMMARY)]);
    let mut live = Live::start(heard(logged(provider, &store), &calls).await);
    assert!(completed(&live.turn("u1", "first").await));
    live.compact(2, None).await;
    live.until(ran_at(COMPACTED)).await;
    let (_, seen) = live.close().await;
    let infos = one_per_record(&store, &calls);
    let LogEvent::ContextCompacted {
        before,
        after,
        cost_usd,
        ..
    } = compacted_record(&store)
    else {
        unreachable!()
    };
    assert!(cost_usd.is_some());
    assert_eq!(
        infos,
        [CompactedInfo {
            epoch: 1,
            trigger: "manual".into(),
            engine: "native".into(),
            before,
            after,
            summary: Some(SUMMARY.into()),
            cost_micros: cost_usd.map(crate::pricing::micros),
            partial: false,
            failed: None,
        }]
    );
    let done = position(&seen, |e| matches!(e, AgentEvent::Compacted { .. }));
    assert!(done < position(&seen, ran_at(COMPACTED)));
}

#[tokio::test]
async fn a_compact_by_wisps_engine_hands_its_summary() {
    let calls = Calls::default();
    let store = MemoryStore::default();
    let provider = scripted(vec![answer("Hello.", 1_000), answer(WRITTEN, 60_000)]);
    let agent = logged(provider, &store).with_model_settings(models(WISP));
    let mut live = Live::start(heard(agent, &calls).await);
    assert!(completed(&live.turn("u1", "first").await));
    live.compact(2, None).await;
    live.until(ran_at(COMPACTED)).await;
    live.close().await;
    let infos = one_per_record(&store, &calls);
    let LogEvent::ContextCompacted { summary, .. } = compacted_record(&store) else {
        unreachable!()
    };
    assert_eq!(infos[0].engine, "wisp");
    assert!(summary.is_some());
    assert_eq!(infos[0].summary, summary);
}

#[tokio::test]
async fn an_automatic_compaction_fires_it_in_the_turn_after_the_compaction() {
    let calls = Calls::default();
    let store = MemoryStore::default();
    let provider = scripted(vec![
        answer("Hello.", 5_000),
        compaction(SUMMARY),
        answer("Again.", 300),
    ]);
    let mut live = Live::start(heard(on(&provider, &store, LOW), &calls).await);
    assert!(completed(&live.turn("u1", "first").await));
    assert!(completed(&live.turn("u2", "second").await));
    let (_, seen) = live.close().await;
    let infos = one_per_record(&store, &calls);
    assert_eq!(infos.len(), 1);
    let done = position(&seen, |e| matches!(e, AgentEvent::Compacted { .. }));
    let hook = position(&seen, ran_at(COMPACTED));
    let ended = seen
        .iter()
        .rposition(|e| matches!(e, AgentEvent::TurnEnded { .. }))
        .unwrap();
    assert!(done < hook && hook < ended, "{seen:#?}");
}

#[tokio::test]
async fn a_failed_automatic_compaction_fires_it_with_the_fill_and_its_message() {
    let calls = Calls::default();
    let store = MemoryStore::default();
    let provider = scripted(vec![
        answer("Hello.", 5_000),
        server_failed(),
        answer("Again.", 300),
    ]);
    let agent = no_retries(on(&provider, &store, LOW));
    let mut live = Live::start(heard(agent, &calls).await);
    live.turn("u1", "first").await;
    assert!(completed(&live.turn("u2", "second").await));
    let (_, seen) = live.close().await;
    let infos = one_per_record(&store, &calls);
    let [info] = infos.as_slice() else {
        panic!("{infos:?}")
    };
    let message = log_events(&store)
        .into_iter()
        .find_map(|e| match e {
            LogEvent::CompactionFailed { message, .. } => Some(message),
            _ => None,
        })
        .unwrap();
    assert_eq!(
        (info.epoch, info.trigger.as_str(), info.engine.as_str()),
        (0, "auto", "native")
    );
    assert_eq!(info.failed.as_deref(), Some(message.as_str()));
    assert!(info.before > 0);
    assert_eq!(info.before, info.after);
    assert_eq!((info.summary.clone(), info.partial), (None, false));
    let failed = position(&seen, |e| matches!(e, AgentEvent::CompactionFailed { .. }));
    assert!(failed < position(&seen, ran_at(COMPACTED)));
}

#[tokio::test]
async fn an_overflow_compaction_fires_it_once() {
    let calls = Calls::default();
    let store = MemoryStore::default();
    let provider = scripted(vec![
        answer("One.", 50_000),
        answer("Two.", 100_000),
        answer("Three.", 160_000),
        failing(ProviderError::new(
            ErrorKind::ContextOverflow,
            "prompt is too long: 250000 tokens > 200000 maximum",
        )),
        compaction(SUMMARY),
        answer("Four.", 900),
    ]);
    let agent = logged(provider, &store).with_auto_compact(false);
    let mut live = Live::start(heard(agent, &calls).await);
    for (id, text) in [("u1", "first"), ("u2", "second"), ("u3", "third")] {
        assert!(completed(&live.turn(id, text).await));
    }
    assert!(completed(&live.turn("u4", "fourth").await));
    live.close().await;
    let infos = one_per_record(&store, &calls);
    assert_eq!(infos.len(), 1);
    assert_eq!(infos[0].trigger, "overflow");
}

#[tokio::test]
async fn a_switch_compaction_and_its_convert_each_fire_it_once() {
    let calls = Calls::default();
    let store = MemoryStore::default();
    let provider = scripted(vec![
        answer("Hello.", 1_000),
        compaction(SUMMARY),
        answer("Second.", 300),
        summarised("Fresh.", 2_000, 0, vec![]),
        answer("Third.", 400),
    ]);
    let agent = other_without_native(&store, &provider);
    let mut live = Live::start(heard(agent, &calls).await);
    live.turn("u1", "first").await;
    live.compact(2, None).await;
    assert!(completed(&live.turn("u2", "second").await));
    change(&mut live, 3, Some("other"), None).await;
    assert!(completed(&live.turn("u3", "third").await));
    live.close().await;
    let infos = one_per_record(&store, &calls);
    let triggers: Vec<&str> = infos.iter().map(|i| i.trigger.as_str()).collect();
    assert_eq!(triggers, ["manual", "switch", "convert"]);
}

#[tokio::test]
async fn a_compact_cancelled_before_it_starts_fires_it_with_no_engine() {
    let calls = Calls::default();
    let store = MemoryStore::default();
    let host = Arc::new(Held {
        gate: Semaphore::new(0),
    });
    let provider = scripted(vec![answer("Hello.", 1_000)]);
    let agent = logged(provider, &store).with_settings_host(host.clone());
    let mut live = Live::start(heard(agent, &calls).await);
    live.turn("u1", "first").await;
    live.send(ClientCommand::ChangeSettings {
        request: RequestId(9),
        changes: vec![SettingChange::named("effort", SettingValue::Text("low".into())).unwrap()],
        save: true,
    })
    .await;
    live.send(compact(2, None)).await;
    live.send(ClientCommand::Cancel).await;
    live.until(|e| answers(e, 2)).await;
    host.gate.add_permits(4);
    live.until(ran_at(COMPACTED)).await;
    live.close().await;
    let infos = one_per_record(&store, &calls);
    let [info] = infos.as_slice() else {
        panic!("{infos:?}")
    };
    assert_eq!(
        (info.trigger.as_str(), info.engine.as_str(), info.epoch),
        ("manual", "", 0)
    );
    assert_eq!(info.failed.as_deref(), Some(CANCELLED_BEFORE_START));
    assert_eq!(info.cost_micros, None);
}

#[tokio::test]
async fn a_background_swap_fires_it_once() {
    let calls = Calls::default();
    let store = MemoryStore::default();
    let provider = documented(vec![
        answer("Hello.", super::background::NEAR),
        compaction(SUMMARY),
        answer("Again.", 300),
    ]);
    let mut live = Live::start(heard(background(&provider, &store), &calls).await);
    assert!(completed(&live.turn("u1", "first").await));
    live.until(prepared).await;
    assert!(completed(&live.turn("u2", "second").await));
    live.close().await;
    let infos = one_per_record(&store, &calls);
    assert_eq!(infos.len(), 1);
    assert_eq!(
        (infos[0].trigger.as_str(), infos[0].summary.as_deref()),
        ("background", Some(SUMMARY))
    );
}

fn background_off(request: u64) -> ClientCommand {
    ClientCommand::ChangeSettings {
        request: RequestId(request),
        changes: SettingChange::named("context.background_compaction", SettingValue::Bool(false))
            .into_iter()
            .collect(),
        save: false,
    }
}

#[tokio::test]
async fn a_background_discard_fires_it_as_a_failure() {
    let calls = Calls::default();
    let store = MemoryStore::default();
    let provider = documented(vec![
        answer("Hello.", super::background::NEAR),
        compaction(SUMMARY),
    ]);
    let mut live = Live::start(heard(background(&provider, &store), &calls).await);
    assert!(completed(&live.turn("u1", "first").await));
    live.until(prepared).await;
    live.send(background_off(6)).await;
    live.until(ran_at(COMPACTED)).await;
    live.close().await;
    let infos = one_per_record(&store, &calls);
    let [info] = infos.as_slice() else {
        panic!("{infos:?}")
    };
    assert_eq!(info.trigger, "background");
    assert!(info.failed.is_some());
}

fn rewound(event: &AgentEvent) -> bool {
    matches!(event, AgentEvent::Rewound { .. })
}

fn refused(event: &AgentEvent) -> bool {
    matches!(event, AgentEvent::Rejected { .. })
}

#[tokio::test]
async fn a_rewind_fires_rewound_once_after_it_and_a_refused_one_fires_nothing() {
    let calls = Calls::default();
    let store = MemoryStore::default();
    let provider = scripted(vec![
        answer("One.", 1_000),
        answer("Two.", 1_100),
        answer("Three.", 1_200),
    ]);
    let mut live = Live::start(heard(logged(provider, &store), &calls).await);
    for (id, text) in [("u1", "first"), ("u2", "second"), ("u3", "third")] {
        assert!(completed(&live.turn(id, text).await));
    }
    live.send(rewind(1, 9)).await;
    live.until(refused).await;
    live.send(rewind(2, 2)).await;
    live.until(rewound).await;
    live.until(ran_at(BACK)).await;
    let (_, seen) = live.close().await;
    assert_eq!(rewinds(&calls), [RewindInfo { turn: 2, epoch: 0 }]);
    let back = position(&seen, ran_at(BACK));
    assert!(position(&seen, rewound) < back);
    assert_eq!(seen.iter().filter(|e| ran_at(BACK)(e)).count(), 1);
    assert_eq!(
        sequence(&store),
        ["rewound", BACK],
        "{:?}",
        sequence(&store)
    );
    assert!(compactions(&calls).is_empty());
}

#[tokio::test]
async fn a_rewind_after_a_compaction_hands_the_epoch_in_force() {
    let calls = Calls::default();
    let store = MemoryStore::default();
    let provider = scripted(vec![
        answer("One.", 1_000),
        compaction(SUMMARY),
        answer("Two.", 1_100),
        answer("Three.", 1_200),
    ]);
    let mut live = Live::start(heard(logged(provider, &store), &calls).await);
    assert!(completed(&live.turn("u1", "first").await));
    live.compact(2, None).await;
    assert!(completed(&live.turn("u2", "second").await));
    assert!(completed(&live.turn("u3", "third").await));
    live.send(rewind(3, 3)).await;
    live.until(ran_at(BACK)).await;
    live.close().await;
    assert_eq!(rewinds(&calls), [RewindInfo { turn: 3, epoch: 1 }]);
}

fn held_after_turn(turn: u64, gate: Arc<Notify>, started: Arc<Notify>) -> HookScript {
    Arc::new(move |_, _, event| {
        let gate = gate.clone();
        let started = started.clone();
        Box::pin(async move {
            if matches!(event, Event::AfterTurn(ref t) if t.turn == turn) {
                started.notify_one();
                gate.notified().await;
            }
            Reply::None
        })
    })
}

#[tokio::test]
async fn a_rewind_pumped_during_an_after_turn_hook_fires_rewound_once_after_that_hook() {
    let calls = Calls::default();
    let store = MemoryStore::default();
    let host = Arc::new(Held {
        gate: Semaphore::new(0),
    });
    let reply = Arc::new(Semaphore::new(0));
    let gate = Arc::new(Notify::new());
    let started = Arc::new(Notify::new());
    let provider = scripted(vec![
        answer("One.", 1_000),
        answer("Two.", 1_100).gated(&reply),
    ]);
    let body = HEARD.to_owned() + "\n[[hooks]]\nname = \"after\"\npoint = \"after-turn\"\n";
    let agent = logged(provider, &store).with_settings_host(host.clone());
    let script = held_after_turn(2, gate.clone(), started.clone());
    let mut live = Live::start(heard_by(agent, &body, script, &calls).await);
    assert!(completed(&live.turn("u1", "first").await));
    live.send(send("u2", "second")).await;
    live.until(|e| matches!(e, AgentEvent::StepStarted { .. }))
        .await;
    live.send(ClientCommand::ChangeSettings {
        request: RequestId(9),
        changes: vec![SettingChange::named("effort", SettingValue::Text("low".into())).unwrap()],
        save: true,
    })
    .await;
    live.send(rewind(4, 2)).await;
    reply.add_permits(1);
    within("the hook's start", started.notified()).await;
    host.gate.add_permits(4);
    live.until(rewound).await;
    gate.notify_one();
    live.until(ran_at(BACK)).await;
    let (_, seen) = live.close().await;
    assert_eq!(rewinds(&calls), [RewindInfo { turn: 2, epoch: 0 }]);
    let after = seen.iter().rposition(ran_at("aa/after")).unwrap();
    let back = position(&seen, ran_at(BACK));
    assert!(
        position(&seen, rewound) < after && after < back,
        "{seen:#?}"
    );
    assert_eq!(seen.iter().filter(|e| ran_at(BACK)(e)).count(), 1);
}

pub(super) fn child_link() -> ParentLink {
    ParentLink {
        session_id: "parent".into(),
        agent: AgentPath::root(),
        depth: 1,
        config: SpawnChoices::default(),
        todos: Vec::new(),
    }
}

#[tokio::test]
async fn a_child_fires_both_points() {
    let calls = Calls::default();
    let store = MemoryStore::default();
    let provider = scripted(vec![
        answer("One.", 1_000),
        compaction(SUMMARY),
        answer("Two.", 1_100),
    ]);
    let agent = logged(provider, &store).with_parent(child_link());
    let mut live = Live::start(heard(agent, &calls).await);
    assert!(completed(&live.turn("u1", "first").await));
    live.compact(2, None).await;
    live.until(ran_at(COMPACTED)).await;
    assert!(completed(&live.turn("u2", "second").await));
    live.send(rewind(3, 2)).await;
    live.until(ran_at(BACK)).await;
    live.close().await;
    assert_eq!(one_per_record(&store, &calls).len(), 1);
    assert_eq!(rewinds(&calls), [RewindInfo { turn: 2, epoch: 1 }]);
}

#[tokio::test]
async fn a_background_hook_at_each_point_is_logged_at_the_next_drain() {
    let calls = Calls::default();
    let store = MemoryStore::default();
    let provider = scripted(vec![
        answer("One.", 1_000),
        compaction(SUMMARY),
        answer("Two.", 1_100),
    ]);
    let body = "\n[[hooks]]\nname = \"compacted\"\npoint = \"after-compact\"\nbackground = true\n\n[[hooks]]\nname = \"back\"\npoint = \"rewound\"\nbackground = true\n";
    let agent = heard_by(logged(provider, &store), body, kit::none(), &calls).await;
    let mut live = Live::start(agent);
    assert!(completed(&live.turn("u1", "first").await));
    live.compact(2, None).await;
    live.until(ran_at(COMPACTED)).await;
    assert!(completed(&live.turn("u2", "second").await));
    live.send(rewind(3, 2)).await;
    live.until(ran_at(BACK)).await;
    live.close().await;
    assert_eq!(sequence(&store), ["manual", COMPACTED, "rewound", BACK]);
    let outcomes: Vec<HookOutcome> = log_events(&store)
        .into_iter()
        .filter_map(|e| match e {
            LogEvent::HookRan { hook, outcome, .. } if hook == COMPACTED || hook == BACK => {
                Some(outcome)
            }
            _ => None,
        })
        .collect();
    assert_eq!(outcomes, [HookOutcome::None, HookOutcome::None]);
}

fn held_request(turn: u64, gate: Arc<Notify>, started: Arc<Notify>) -> HookScript {
    Arc::new(move |_, _, event| {
        let gate = gate.clone();
        let started = started.clone();
        Box::pin(async move {
            if matches!(event, Event::BeforeRequest(ref s) if s.turn == turn) {
                started.notify_one();
                gate.notified().await;
            }
            Reply::None
        })
    })
}

fn fill_words() -> kit::Render {
    Arc::new(|host, _, _| {
        let fill = host.current_fill();
        let words = format!("epoch={} estimate={}", fill.epoch, fill.estimate);
        Box::pin(async move { Ok(kit::line(&words)) })
    })
}

fn slot_content(event: &AgentEvent) -> bool {
    matches!(event, AgentEvent::SlotContent { .. })
}

#[tokio::test]
async fn a_compaction_during_a_hook_marks_a_shown_section_once_the_hook_ends() {
    let calls = Calls::default();
    let store = MemoryStore::default();
    let gate = Arc::new(Notify::new());
    let started = Arc::new(Notify::new());
    let provider = documented(vec![
        answer("Hello.", super::background::NEAR),
        Step::hang(),
        answer("Again.", 300),
    ]);
    let body = "\n[[hooks]]\nname = \"asked\"\npoint = \"before-request\"\n".to_owned()
        + &kit::section("sec", "marks = [\"after-compact\"]\n");
    let script = held_request(2, gate.clone(), started.clone());
    let make: Make = kit::sections(script, fill_words(), calls.clone());
    let agent = with(
        background(&provider, &store),
        vec![(kit::slotted("aa", &body), make)],
    )
    .await;
    let mut live = Live::start(agent);
    live.send(ClientCommand::SlotsShown {
        sections: vec!["aa/sec".to_owned()],
        pages: Vec::new(),
    })
    .await;
    live.until(slot_content).await;
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
    assert!(
        until(|| provider.sent().len() >= 2).await,
        "no second request"
    );
    live.send(send("u2", "second")).await;
    within("the hook's start", started.notified()).await;
    live.send(background_off(6)).await;
    live.until(|e| {
        matches!(
            e,
            AgentEvent::CompactionFailed {
                trigger: CompactionTrigger::Background,
                ..
            }
        )
    })
    .await;
    tokio::time::sleep(Duration::from_millis(200)).await;
    gate.notify_one();
    live.until(slot_content).await;
    let (_, seen) = live.close().await;
    let failed = position(&seen, |e| matches!(e, AgentEvent::CompactionFailed { .. }));
    let asked = seen.iter().rposition(ran_at("aa/asked")).unwrap();
    let drawn = seen.iter().rposition(slot_content).unwrap();
    assert!(failed < asked && asked < drawn, "{seen:#?}");
    assert!(!seen[failed..asked].iter().any(slot_content), "{seen:#?}");
    assert_eq!(calls.renders().len(), 2);
}

#[tokio::test]
async fn a_compact_after_an_idle_switch_off_runs_none_of_its_after_compact_hooks() {
    let calls = Calls::default();
    let store = MemoryStore::default();
    let provider = scripted(vec![answer("Hello.", 1_000), compaction(SUMMARY)]);
    let mut live = Live::start(heard(logged(provider, &store), &calls).await);
    assert!(completed(&live.turn("u1", "first").await));
    live.send(crate::plugin::testing::plugin_switch(
        9,
        "aa",
        wisp_protocol::settings::ENABLED_KEY,
        false,
    ))
    .await;
    live.until(|e| matches!(e, AgentEvent::SettingsChanged { answers: Some(r), .. } if r.0 == 9))
        .await;
    live.compact(2, None).await;
    let (_, seen) = live.close().await;
    assert!(
        seen.iter()
            .any(|e| matches!(e, AgentEvent::Compacted { .. })),
        "{seen:#?}"
    );
    assert!(compactions(&calls).is_empty());
    assert!(!sequence(&store).contains(&COMPACTED.to_owned()));
}
