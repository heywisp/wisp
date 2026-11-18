use wisp_protocol::settings::{
    CompactionQuote, Consequence, SettingChange, SettingTarget, SettingValue, SwitchQuote,
    SwitchTarget, Withdrawal,
};
use wisp_protocol::{SwitchCompaction, SwitchCompactionCause};

use super::auto::{LOW, order};
use super::marker::change;
use super::quote::withdrawal;
use super::*;

pub(super) const WINDOWS: &str = "[models.\"claude-opus-5-5\"]\ncontext_window = 1000000\n\n[endpoints.other.models.\"claude-opus-5-5\"]\ncontext_window = 200000\n";
pub(super) const WINDOWS_NO_NATIVE: &str = "[models.\"claude-opus-5-5\"]\ncontext_window = 1000000\n\n[endpoints.other.models.\"claude-opus-5-5\"]\ncontext_window = 200000\nnative_compaction = false\n";

pub(super) fn windows(store: &MemoryStore, provider: &Arc<Fake>, config: &str) -> Agent {
    two_endpoints(logged(provider.clone(), store), provider.clone())
        .with_model_settings(models(config))
}

pub(super) fn endpoint_quote<'a>(quotes: &'a [SwitchQuote], name: &str) -> Option<&'a SwitchQuote> {
    quotes
        .iter()
        .find(|q| matches!(&q.target, SwitchTarget::Endpoint(n) if n == name))
}

pub(super) async fn snapshot(
    live: &mut Live,
    request: u64,
) -> wisp_protocol::settings::SettingsSnapshot {
    live.send(ClientCommand::DescribeSettings {
        request: RequestId(request),
    })
    .await;
    let answer = live
        .until(|e| matches!(e, AgentEvent::SettingsSnapshot(s) if s.answers == RequestId(request)))
        .await;
    match answer {
        AgentEvent::SettingsSnapshot(snapshot) => snapshot,
        other => panic!("{other:?}"),
    }
}

fn change_consequence(seen: &[AgentEvent], request: u64, name: &str) -> Option<Consequence> {
    seen.iter().find_map(|e| match e {
        AgentEvent::SettingsChanged {
            answers: Some(RequestId(r)),
            changes,
            ..
        } if *r == request => changes
            .iter()
            .find(|c| match &c.target {
                SettingTarget::Name(n) => n.spec().name == name,
                SettingTarget::Path(p) => p.spec().name == name,
            })
            .and_then(|c| c.consequence.clone()),
        _ => None,
    })
}

fn switch_started(seen: &[AgentEvent]) -> Option<SwitchCompaction> {
    seen.iter().find_map(|e| match e {
        AgentEvent::CompactionStarted {
            trigger: CompactionTrigger::Switch,
            switch,
            ..
        } => switch.clone(),
        _ => None,
    })
}

fn smaller_cause(consequence: Option<&Consequence>) -> bool {
    matches!(
        consequence,
        Some(Consequence::CompactsBeforeSwitch {
            estimate_micros,
            head_tokens,
            cause: SwitchCompactionCause::SmallerWindow,
        }) if *estimate_micros > 0 && *head_tokens > 0
    )
}

#[tokio::test]
async fn a_switch_to_a_smaller_window_under_the_fill_compacts_on_the_old_pair_first() {
    let store = MemoryStore::default();
    let provider = scripted(vec![
        answer("Hello.", 300_000),
        compaction(SUMMARY),
        answer("Second.", 2_000),
    ]);
    let mut live = Live::start(windows(&store, &provider, WINDOWS));
    assert!(completed(&live.turn("u1", "first").await));
    let quotes = snapshot(&mut live, 2).await.switches;
    let other = endpoint_quote(&quotes, "other").unwrap();
    assert!(smaller_cause(other.consequence.as_ref()), "{quotes:#?}");
    assert_eq!(other.retry_compaction, None);
    assert!(endpoint_quote(&quotes, "env").is_none());
    change(&mut live, 3, Some("other"), None).await;
    assert!(completed(&live.turn("u2", "second").await));
    let (agent, seen) = live.close().await;

    assert!(smaller_cause(
        change_consequence(&seen, 3, "endpoint").as_ref()
    ));
    let started = switch_started(&seen).unwrap();
    assert_eq!(started.cause, SwitchCompactionCause::SmallerWindow);
    assert!(started.estimate_micros > 0);
    let compacted = position(&seen, |e| {
        matches!(
            e,
            AgentEvent::Compacted {
                trigger: CompactionTrigger::Switch,
                turn: Some(2),
                converted: false,
                ..
            }
        )
    });
    let turn = position(&seen, |e| {
        matches!(e, AgentEvent::TurnStarted { turn: 2, .. })
    });
    assert!(compacted < turn);
    assert_eq!(provider.sent().len(), 3);
    assert!(provider.beta(1).contains(COMPACTION_BETA));
    assert!(provider.beta(2).contains(COMPACTION_BETA));
    assert_eq!(
        provider.body(2)["messages"][0]["content"][0]["type"],
        "compaction"
    );
    assert_eq!(agent.endpoint_name(), "other");
    assert_eq!(order(&store, 2), ["compacted", "started", "ended"]);
    assert_rebuilds(&store, &agent);
}

#[tokio::test]
async fn a_smaller_window_wins_the_cause_and_the_block_is_converted_after_it() {
    let store = MemoryStore::default();
    let provider = scripted(vec![
        answer("Hello.", 1_000),
        compaction(SUMMARY),
        answer("Second.", 300_000),
        summarised("Fresh.", 2_000, 0, vec![]),
        answer("Third.", 2_000),
    ]);
    let mut live = Live::start(windows(&store, &provider, WINDOWS_NO_NATIVE));
    live.turn("u1", "first").await;
    live.compact(2, None).await;
    assert!(completed(&live.turn("u2", "second").await));
    let quotes = snapshot(&mut live, 3).await.switches;
    let other = endpoint_quote(&quotes, "other").unwrap();
    assert!(smaller_cause(other.consequence.as_ref()), "{quotes:#?}");
    change(&mut live, 4, Some("other"), None).await;
    assert!(completed(&live.turn("u3", "third").await));
    let (agent, seen) = live.close().await;

    assert_eq!(
        switch_started(&seen).unwrap().cause,
        SwitchCompactionCause::SmallerWindow
    );
    let switched = position(&seen, |e| {
        matches!(
            e,
            AgentEvent::Compacted {
                trigger: CompactionTrigger::Switch,
                ..
            }
        )
    });
    let converted = position(&seen, |e| {
        matches!(
            e,
            AgentEvent::Compacted {
                trigger: CompactionTrigger::Convert,
                converted: true,
                ..
            }
        )
    });
    assert!(switched < converted);
    assert_eq!(provider.sent().len(), 5);
    assert!(provider.beta(3).contains(COMPACTION_BETA));
    assert!(!provider.beta(4).contains(COMPACTION_BETA));
    assert!(provider.body(4)["messages"].to_string().contains("Fresh."));
    assert_eq!(agent.endpoint_name(), "other");
    assert_rebuilds(&store, &agent);
}

#[tokio::test]
async fn a_switch_quoted_as_a_convert_alone_runs_as_a_smaller_window_compaction() {
    let store = MemoryStore::default();
    let gate = Arc::new(Semaphore::new(0));
    let second = answer("Second.", 300_000).script;
    let provider = scripted(vec![
        answer("Hello.", 1_000),
        compaction(SUMMARY),
        Step::from(second).gated(&gate),
        summarised("Fresh.", 2_000, 0, vec![]),
        answer("Third.", 2_000),
    ]);
    let mut live = Live::start(windows(&store, &provider, WINDOWS_NO_NATIVE));
    live.turn("u1", "first").await;
    live.compact(2, None).await;
    live.send(send("u2", "second")).await;
    live.until(|e| matches!(e, AgentEvent::StepStarted { .. }))
        .await;
    change(&mut live, 3, Some("other"), None).await;
    gate.add_permits(1);
    live.until(|e| matches!(e, AgentEvent::TurnEnded { .. }))
        .await;
    assert!(completed(&live.turn("u3", "third").await));
    let (agent, seen) = live.close().await;

    assert!(matches!(
        change_consequence(&seen, 3, "endpoint"),
        Some(Consequence::CompactsBeforeSwitch {
            estimate_micros: 0,
            cause: SwitchCompactionCause::Convert,
            ..
        })
    ));
    assert_eq!(
        switch_started(&seen).unwrap().cause,
        SwitchCompactionCause::SmallerWindow
    );
    assert_eq!(agent.endpoint_name(), "other");
    assert_rebuilds(&store, &agent);
}

#[tokio::test]
async fn esc_during_a_smaller_window_compaction_withdraws_the_switch() {
    let store = MemoryStore::default();
    let provider = scripted(vec![
        answer("Hello.", 300_000),
        Step::hang(),
        answer("Third.", 400),
    ]);
    let mut live = Live::start(windows(&store, &provider, WINDOWS));
    live.turn("u1", "first").await;
    change(&mut live, 2, Some("other"), None).await;
    live.send(send("u2", "second")).await;
    live.until(|e| {
        matches!(
            e,
            AgentEvent::CompactionStarted {
                trigger: CompactionTrigger::Switch,
                ..
            }
        )
    })
    .await;
    live.send(ClientCommand::Cancel).await;
    live.until(|e| withdrawal(std::slice::from_ref(e)).is_some())
        .await;
    assert!(completed(&live.turn("u3", "third").await));
    let (agent, seen) = live.close().await;
    let withdrawn = withdrawal(&seen).unwrap();
    assert!(matches!(
        &seen[withdrawn],
        AgentEvent::SettingsChanged {
            withdrawn: Some(Withdrawal::SwitchCompactionCancelled),
            ..
        }
    ));
    assert_eq!(agent.endpoint_name(), "env");
    assert_eq!(provider.sent().len(), 3);
}

#[tokio::test]
async fn a_failed_smaller_window_compaction_withdraws_the_switch_before_the_turn() {
    let store = MemoryStore::default();
    let provider = scripted(vec![
        answer("Hello.", 300_000),
        failing(server_error()),
        answer("Second.", 400),
    ]);
    let mut live = Live::start(windows(&store, &provider, WINDOWS));
    live.turn("u1", "first").await;
    change(&mut live, 2, Some("other"), None).await;
    assert!(completed(&live.turn("u2", "second").await));
    let (agent, seen) = live.close().await;
    let failed = position(&seen, |e| {
        matches!(
            e,
            AgentEvent::CompactionFailed {
                trigger: CompactionTrigger::Switch,
                ..
            }
        )
    });
    let withdrawn = withdrawal(&seen).unwrap();
    let started = position(&seen, |e| {
        matches!(e, AgentEvent::TurnStarted { turn: 2, .. })
    });
    assert!(failed < withdrawn && withdrawn < started);
    assert!(matches!(
        &seen[withdrawn],
        AgentEvent::SettingsChanged {
            withdrawn: Some(Withdrawal::SwitchCompactionFailed),
            ..
        }
    ));
    assert_eq!(agent.endpoint_name(), "env");
    assert_eq!(provider.sent().len(), 3);
    assert_rebuilds(&store, &agent);
}

#[tokio::test]
async fn a_failed_smaller_window_compaction_over_a_block_the_target_cant_take_converts() {
    let store = MemoryStore::default();
    let provider = scripted(vec![
        answer("Hello.", 1_000),
        compaction(SUMMARY),
        answer("Second.", 300_000),
        failing(server_error()),
        answer("Third.", 2_000),
    ]);
    let mut live = Live::start(windows(&store, &provider, WINDOWS_NO_NATIVE));
    live.turn("u1", "first").await;
    live.compact(2, None).await;
    assert!(completed(&live.turn("u2", "second").await));
    change(&mut live, 3, Some("other"), None).await;
    assert!(completed(&live.turn("u3", "third").await));
    let (agent, seen) = live.close().await;
    assert!(withdrawal(&seen).is_none());
    assert!(seen.iter().any(|e| matches!(
        e,
        AgentEvent::Compacted {
            trigger: CompactionTrigger::Convert,
            partial: true,
            ..
        }
    )));
    assert!(!provider.beta(4).contains(COMPACTION_BETA));
    assert_eq!(agent.endpoint_name(), "other");
    assert_rebuilds(&store, &agent);
}

#[tokio::test]
async fn a_failed_smaller_window_compaction_whose_converted_view_doesnt_fit_withdraws() {
    let store = MemoryStore::default();
    let provider = scripted(vec![
        answer("Hello.", 1_000),
        compaction(SUMMARY),
        answer("Second.", 300_000),
        failing(server_error()),
        answer("Third.", 2_000),
    ]);
    let mut live = Live::start(windows(&store, &provider, WINDOWS_NO_NATIVE));
    live.turn("u1", "first").await;
    live.compact(2, None).await;
    assert!(completed(&live.turn("u2", "second").await));
    change(&mut live, 3, Some("other"), None).await;
    assert!(completed(&live.turn("u3", &"word ".repeat(200_000)).await));
    let (agent, seen) = live.close().await;
    assert!(matches!(
        &seen[withdrawal(&seen).unwrap()],
        AgentEvent::SettingsChanged {
            withdrawn: Some(Withdrawal::SwitchCompactionFailed),
            ..
        }
    ));
    assert!(!seen.iter().any(|e| matches!(
        e,
        AgentEvent::Compacted {
            trigger: CompactionTrigger::Convert,
            ..
        }
    )));
    assert_eq!(agent.endpoint_name(), "env");
}

#[tokio::test]
async fn turning_automatic_compaction_on_over_the_point_reports_the_compaction_it_makes_due() {
    let store = MemoryStore::default();
    let provider = scripted(vec![answer("Hello.", 5_000)]);
    let agent = on(&provider, &store, LOW).with_auto_compact(false);
    let mut live = Live::start(agent);
    assert!(completed(&live.turn("u1", "first").await));
    let quote = snapshot(&mut live, 1).await.compaction.unwrap();
    let CompactionQuote {
        fill_tokens,
        max_tokens,
        estimate_micros,
        head_write_micros,
    } = quote;
    assert!(fill_tokens >= 5_000);
    assert!(max_tokens > 0);
    assert!(estimate_micros > 0 && head_write_micros > 0);
    for (request, on) in [(2, true), (3, false)] {
        live.send(ClientCommand::ChangeSettings {
            request: RequestId(request),
            changes: vec![
                SettingChange::named("context.auto_compact", SettingValue::Bool(on)).unwrap(),
            ],
            save: false,
        })
        .await;
        live.until(|e| {
            matches!(
                e,
                AgentEvent::SettingsChanged { answers: Some(RequestId(r)), .. } if *r == request
            )
        })
        .await;
    }
    let (_, seen) = live.close().await;
    assert_eq!(
        change_consequence(&seen, 2, "context.auto_compact"),
        Some(Consequence::CompactsBeforeNextMessage {
            estimate_micros,
            head_write_micros,
        })
    );
    assert_eq!(change_consequence(&seen, 3, "context.auto_compact"), None);
}

#[tokio::test]
async fn a_lower_window_under_the_fill_reports_a_compaction_and_a_higher_one_none() {
    let store = MemoryStore::default();
    let provider = scripted(vec![answer("Hello.", 90_000)]);
    let mut live = Live::start(logged(provider.clone(), &store));
    assert!(completed(&live.turn("u1", "first").await));
    for (request, window) in [(1, 104_000), (2, 1_000_000)] {
        live.send(ClientCommand::ChangeSettings {
            request: RequestId(request),
            changes: vec![
                SettingChange::named("context_window", SettingValue::Tokens(window)).unwrap(),
            ],
            save: false,
        })
        .await;
        live.until(|e| {
            matches!(
                e,
                AgentEvent::SettingsChanged { answers: Some(RequestId(r)), .. } if *r == request
            )
        })
        .await;
    }
    let (_, seen) = live.close().await;
    assert!(matches!(
        change_consequence(&seen, 1, "context_window"),
        Some(Consequence::CompactsBeforeNextMessage { estimate_micros, .. }) if estimate_micros > 0
    ));
    assert_eq!(change_consequence(&seen, 2, "context_window"), None);
}

#[tokio::test]
async fn no_compaction_quote_without_a_known_window() {
    let store = MemoryStore::default();
    let provider = scripted(vec![answer("Hello.", 5_000)]);
    let session = wisp_protocol::SessionConfig {
        model: "claude-unknown-test".to_owned(),
        ..default_session()
    };
    let agent = Agent::new(provider.clone(), session).with_log(
        SessionLog::new(Box::new(store.clone()), "s1"),
        log_context(),
    );
    let mut live = Live::start(agent);
    assert!(completed(&live.turn("u1", "first").await));
    assert_eq!(snapshot(&mut live, 1).await.compaction, None);
    live.close().await;
}

fn past_point(seen: &[AgentEvent]) -> Vec<&AgentEvent> {
    seen.iter()
        .filter(|e| matches!(e, AgentEvent::PastCompactionPoint { .. }))
        .collect()
}

fn switch_compactions(seen: &[AgentEvent]) -> usize {
    seen.iter()
        .filter(|e| matches!(e, AgentEvent::CompactionStarted { .. }))
        .count()
}

#[tokio::test]
async fn a_failed_smaller_window_compaction_after_a_switch_back_keeps_the_current_pair() {
    let store = MemoryStore::default();
    let gate = Arc::new(Semaphore::new(0));
    let provider = scripted(vec![
        answer("Hello.", 300_000),
        Step::from(vec![Err(server_error())]).gated(&gate),
        answer("Second.", 400),
    ]);
    let mut live = Live::start(windows(&store, &provider, WINDOWS));
    assert!(completed(&live.turn("u1", "first").await));
    change(&mut live, 2, Some("other"), None).await;
    live.send(send("u2", "second")).await;
    live.until(|e| {
        matches!(
            e,
            AgentEvent::CompactionStarted {
                trigger: CompactionTrigger::Switch,
                ..
            }
        )
    })
    .await;
    change(&mut live, 3, Some("env"), None).await;
    gate.add_permits(1);
    let ended = live
        .until(|e| matches!(e, AgentEvent::TurnEnded { .. }))
        .await;
    let (agent, seen) = live.close().await;
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
    assert!(seen.iter().any(|e| matches!(
        e,
        AgentEvent::CompactionFailed {
            trigger: CompactionTrigger::Switch,
            ..
        }
    )));
    assert_eq!(withdrawal(&seen), None);
    assert_eq!(agent.endpoint_name(), "env");
    assert_eq!(provider.sent().len(), 3);
    assert_rebuilds(&store, &agent);
}

#[tokio::test]
async fn the_turns_message_counts_toward_the_new_pairs_point() {
    let store = MemoryStore::default();
    let provider = scripted(vec![
        answer("Hello.", 100_000),
        compaction(SUMMARY),
        answer("Second.", 2_000),
    ]);
    let mut live = Live::start(windows(&store, &provider, WINDOWS));
    assert!(completed(&live.turn("u1", "first").await));
    let quotes = snapshot(&mut live, 2).await.switches;
    assert!(endpoint_quote(&quotes, "other").is_none(), "{quotes:#?}");
    change(&mut live, 3, Some("other"), None).await;
    assert!(completed(&live.turn("u2", &"z".repeat(300_000)).await));
    let (agent, seen) = live.close().await;
    assert_eq!(
        switch_started(&seen).map(|s| s.cause),
        Some(SwitchCompactionCause::SmallerWindow)
    );
    assert_eq!(agent.endpoint_name(), "other");
    assert_rebuilds(&store, &agent);
}

#[tokio::test]
async fn the_new_point_is_reached_at_the_point_itself() {
    let store = MemoryStore::default();
    let provider = scripted(vec![answer("Hello.", 100_000)]);
    let mut live = Live::start(windows(&store, &provider, WINDOWS));
    assert!(completed(&live.turn("u1", "first").await));
    let (agent, _) = live.close().await;
    let other = agent
        .connections
        .iter()
        .position(|c| c.name == "other")
        .unwrap();
    let model = agent.session.model.clone();
    let (_, point) = agent.pair_point(other, &model).unwrap();
    let short = point - agent.fill_estimate();
    assert!(agent.over_new_point(other, &model, short));
    assert!(!agent.over_new_point(other, &model, short - 1));
    assert!(!agent.over_new_point(agent.endpoint, &model, short));
}

#[tokio::test]
async fn a_smaller_window_switch_compaction_keeps_the_prefix() {
    let store = MemoryStore::default();
    let provider = scripted(vec![
        answer("Hello.", 300_000),
        compaction(SUMMARY),
        answer("Second.", 2_000),
        answer("Third.", 2_500),
    ]);
    let mut live = Live::start(windows(&store, &provider, WINDOWS));
    assert!(completed(&live.turn("u1", "first").await));
    change(&mut live, 2, Some("other"), None).await;
    assert!(completed(&live.turn("u2", "second").await));
    assert!(completed(&live.turn("u3", "third").await));
    let (agent, _) = live.close().await;
    super::auto::assert_prefix_kept(&provider);
    let before = strip_cache_control(&provider.body(0)["messages"]);
    let compacting = strip_cache_control(&provider.body(1)["messages"]);
    let before = before.as_array().unwrap();
    let compacting = compacting.as_array().unwrap();
    assert_eq!(&compacting[..before.len()], &before[..]);
    assert_eq!(compacting.len(), before.len() + 1);
    assert_eq!(compacting[before.len()]["role"], "assistant");
    assert_eq!(provider.body(1)["model"], provider.body(0)["model"]);
    let switched = strip_cache_control(&provider.body(2)["messages"]).to_string();
    let next = strip_cache_control(&provider.body(3)["messages"]).to_string();
    assert!(
        next.starts_with(switched.strip_suffix(']').unwrap()),
        "{next}"
    );
    assert_rebuilds(&store, &agent);
}

#[tokio::test]
async fn with_automatic_compaction_off_a_switch_past_the_point_only_tells_you() {
    let store = MemoryStore::default();
    let provider = scripted(vec![answer("Hello.", 300_000), answer("Second.", 2_000)]);
    let agent = windows(&store, &provider, WINDOWS).with_auto_compact(false);
    let mut live = Live::start(agent);
    assert!(completed(&live.turn("u1", "first").await));
    let quotes = snapshot(&mut live, 2).await.switches;
    assert!(endpoint_quote(&quotes, "other").is_none(), "{quotes:#?}");
    change(&mut live, 3, Some("other"), None).await;
    assert!(completed(&live.turn("u2", "second").await));
    let (agent, seen) = live.close().await;
    assert_eq!(
        past_point(&seen),
        [&AgentEvent::PastCompactionPoint {
            endpoint: "other".into(),
            model: "claude-opus-5-5".into(),
        }]
    );
    assert_eq!(change_consequence(&seen, 3, "endpoint"), None);
    assert_eq!(switch_compactions(&seen), 0);
    assert_eq!(provider.sent().len(), 2);
    assert!(provider.body(1)["messages"].to_string().contains("Hello."));
    assert_eq!(agent.endpoint_name(), "other");
    assert_rebuilds(&store, &agent);
}

#[tokio::test]
async fn with_automatic_compaction_off_a_switch_below_the_point_says_nothing() {
    let store = MemoryStore::default();
    let provider = scripted(vec![answer("Hello.", 1_000), answer("Second.", 2_000)]);
    let agent = windows(&store, &provider, WINDOWS).with_auto_compact(false);
    let mut live = Live::start(agent);
    assert!(completed(&live.turn("u1", "first").await));
    change(&mut live, 2, Some("other"), None).await;
    assert!(completed(&live.turn("u2", "second").await));
    let (_, seen) = live.close().await;
    assert!(past_point(&seen).is_empty());
    assert_eq!(switch_compactions(&seen), 0);
}

#[tokio::test]
async fn with_automatic_compaction_on_a_switch_past_the_point_compacts_without_the_notice() {
    let store = MemoryStore::default();
    let provider = scripted(vec![
        answer("Hello.", 300_000),
        compaction(SUMMARY),
        answer("Second.", 2_000),
    ]);
    let mut live = Live::start(windows(&store, &provider, WINDOWS));
    assert!(completed(&live.turn("u1", "first").await));
    change(&mut live, 2, Some("other"), None).await;
    assert!(completed(&live.turn("u2", "second").await));
    let (_, seen) = live.close().await;
    assert!(past_point(&seen).is_empty());
    assert_eq!(switch_compactions(&seen), 1);
}

async fn switch_and_auto(live: &mut Live, request: u64, on: bool) {
    let changes = wisp_protocol::settings::sorted_changes(vec![
        SettingChange::named("endpoint", SettingValue::Text("other".into())).unwrap(),
        SettingChange::named("context.auto_compact", SettingValue::Bool(on)).unwrap(),
    ]);
    live.send(ClientCommand::ChangeSettings {
        request: RequestId(request),
        changes,
        save: false,
    })
    .await;
    live.until(|e| {
        matches!(
            e,
            AgentEvent::SettingsChanged { answers: Some(RequestId(r)), .. } if *r == request
        )
    })
    .await;
}

#[tokio::test]
async fn a_batch_that_turns_automatic_compaction_off_with_the_switch_only_tells_you() {
    let store = MemoryStore::default();
    let provider = scripted(vec![answer("Hello.", 300_000), answer("Second.", 2_000)]);
    let mut live = Live::start(windows(&store, &provider, WINDOWS));
    assert!(completed(&live.turn("u1", "first").await));
    switch_and_auto(&mut live, 2, false).await;
    assert!(completed(&live.turn("u2", "second").await));
    let (agent, seen) = live.close().await;
    assert_eq!(past_point(&seen).len(), 1);
    assert_eq!(change_consequence(&seen, 2, "endpoint"), None);
    assert_eq!(switch_compactions(&seen), 0);
    assert_eq!(provider.sent().len(), 2);
    assert_eq!(agent.endpoint_name(), "other");
}

#[tokio::test]
async fn a_batch_that_turns_automatic_compaction_on_with_the_switch_quotes_and_compacts() {
    let store = MemoryStore::default();
    let provider = scripted(vec![
        answer("Hello.", 300_000),
        compaction(SUMMARY),
        answer("Second.", 2_000),
    ]);
    let agent = windows(&store, &provider, WINDOWS).with_auto_compact(false);
    let mut live = Live::start(agent);
    assert!(completed(&live.turn("u1", "first").await));
    switch_and_auto(&mut live, 2, true).await;
    assert!(completed(&live.turn("u2", "second").await));
    let (agent, seen) = live.close().await;
    assert!(past_point(&seen).is_empty());
    assert!(
        smaller_cause(change_consequence(&seen, 2, "endpoint").as_ref()),
        "{seen:#?}"
    );
    assert_eq!(
        switch_started(&seen).map(|s| s.cause),
        Some(SwitchCompactionCause::SmallerWindow)
    );
    assert_eq!(agent.endpoint_name(), "other");
    assert_rebuilds(&store, &agent);
}
