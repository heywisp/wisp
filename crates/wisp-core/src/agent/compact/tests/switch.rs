use wisp_protocol::{SwitchCompaction, SwitchCompactionCause};

use super::marker::{assert_rebuilds, change, other_without_native};
use super::usage::{cut_after_start, iteration};
use super::wisp::reply;
use super::*;
use crate::agent::tests::turn_started;
use crate::test_guard::records;

fn position(seen: &[AgentEvent], pick: impl Fn(&AgentEvent) -> bool) -> usize {
    seen.iter()
        .position(pick)
        .unwrap_or_else(|| panic!("{seen:#?}"))
}

fn logged_at(logged: &[LogEvent], pick: impl Fn(&LogEvent) -> bool) -> usize {
    logged
        .iter()
        .position(pick)
        .unwrap_or_else(|| panic!("{logged:#?}"))
}

fn started_on(logged: &[LogEvent], turn: u64) -> Option<String> {
    logged.iter().find_map(|e| match e {
        LogEvent::TurnStarted {
            turn: t,
            endpoint_name,
            ..
        } if *t == turn => endpoint_name.clone(),
        _ => None,
    })
}

fn sent_switch(seen: &[AgentEvent]) -> SwitchCompaction {
    seen.iter()
        .find_map(|e| match e {
            AgentEvent::CompactionStarted {
                trigger: CompactionTrigger::Switch,
                switch: Some(switch),
                ..
            } => Some(switch.clone()),
            _ => None,
        })
        .unwrap_or_else(|| panic!("{seen:#?}"))
}

fn first_text(body: &Value) -> String {
    body["messages"][0]["content"][0]["text"]
        .as_str()
        .unwrap_or_default()
        .to_owned()
}

async fn up_to_the_switch(live: &mut Live) {
    live.turn("u1", "first").await;
    live.compact(2, None).await;
    assert!(completed(&live.turn("u2", "second").await));
    change(live, 3, Some("other"), None).await;
}

fn stopped(stop: StopReason) -> Step {
    let mut script = vec![start(900, 0)];
    script.extend(finish(stop, 900, 0, 10));
    Step::from(script)
}

#[tokio::test]
async fn a_switch_compacts_on_the_old_pair_before_the_turn() {
    let store = MemoryStore::default();
    let provider = scripted(vec![
        answer("Hello.", 1_000),
        compaction(SUMMARY),
        answer("Second.", 300),
        summarised("Fresh.", 2_000, 0, vec![]),
        answer("Third.", 400),
    ]);
    let mut live = Live::start(other_without_native(&store, &provider));
    up_to_the_switch(&mut live).await;
    let outcome = live.turn("u3", "third").await;
    let (agent, seen) = live.close().await;
    assert_eq!(provider.sent().len(), 5);
    assert!(provider.beta(3).contains(COMPACTION_BETA));
    assert!(!provider.beta(4).contains(COMPACTION_BETA));
    let body = provider.body(4);
    assert_eq!(body["messages"].as_array().unwrap().len(), 1);
    assert!(first_text(&body).contains("Fresh."));
    assert_eq!(
        body["messages"][0]["content"]
            .as_array()
            .unwrap()
            .last()
            .unwrap()["text"],
        "third"
    );
    let switch = sent_switch(&seen);
    assert_eq!(switch.cause, SwitchCompactionCause::Convert);
    assert!(switch.estimate_micros > 0);
    assert!(switch.head_tokens > 0);
    let started = position(&seen, |e| {
        matches!(
            e,
            AgentEvent::CompactionStarted {
                trigger: CompactionTrigger::Switch,
                turn: Some(3),
                ..
            }
        )
    });
    let compacted = position(&seen, |e| {
        matches!(
            e,
            AgentEvent::Compacted {
                trigger: CompactionTrigger::Switch,
                engine: CompactionEngine::Native,
                turn: Some(3),
                partial: false,
                ..
            }
        )
    });
    let converted = position(&seen, |e| {
        matches!(
            e,
            AgentEvent::Compacted {
                trigger: CompactionTrigger::Convert,
                engine: CompactionEngine::Wisp,
                turn: Some(3),
                partial: false,
                ..
            }
        )
    });
    let turn = position(&seen, |e| {
        matches!(e, AgentEvent::TurnStarted { turn: 3, .. })
    });
    assert!(started < compacted && compacted < converted && converted < turn);
    let AgentEvent::Compacted {
        cost: Some(spent), ..
    } = &seen[compacted]
    else {
        panic!("{:?}", seen[compacted]);
    };
    let TurnOutcome::Completed {
        cost: Some(cost),
        usage,
        ..
    } = &outcome
    else {
        panic!("{outcome:?}");
    };
    assert!(cost.turn_usd > spent.turn_usd);
    assert_eq!(usage.input_tokens, 2_400);
    let logged = log_events(&store);
    let native = logged_at(&logged, |e| {
        matches!(
            e,
            LogEvent::ContextCompacted {
                trigger: CompactionTrigger::Switch,
                engine: CompactionEngine::Native,
                turn: Some(3),
                switch: Some(_),
                ..
            }
        )
    });
    let convert = logged_at(&logged, |e| {
        matches!(
            e,
            LogEvent::ContextCompacted {
                trigger: CompactionTrigger::Convert,
                engine: CompactionEngine::Wisp,
                turn: Some(3),
                switch: None,
                ..
            }
        )
    });
    assert!(native < convert && convert < logged_at(&logged, turn_started(3)));
    assert_eq!(started_on(&logged, 3).as_deref(), Some("other"));
    assert_eq!(agent.compaction.compactions, 3);
    assert_rebuilds(&store, &agent);
}

#[tokio::test]
async fn a_switch_queued_in_the_first_turn_after_compact_compacts_at_the_next() {
    let store = MemoryStore::default();
    let gate = Arc::new(Semaphore::new(0));
    let mut second = vec![start(300, 0)];
    second.extend(text(0, "Second."));
    second.extend(finish(StopReason::EndTurn, 300, 0, 40));
    let provider = scripted(vec![
        answer("Hello.", 1_000),
        compaction(SUMMARY),
        Step::from(second).gated(&gate),
        compaction("Fresh."),
        answer("Third.", 400),
    ]);
    let mut live = Live::start(other_without_native(&store, &provider));
    live.turn("u1", "first").await;
    live.compact(2, None).await;
    live.send(send("u2", "second")).await;
    live.until(|e| matches!(e, AgentEvent::StepStarted { step: 1 }))
        .await;
    change(&mut live, 3, Some("other"), None).await;
    gate.add_permits(1);
    live.until(|e| matches!(e, AgentEvent::TurnEnded { .. }))
        .await;
    assert!(completed(&live.turn("u3", "third").await));
    let (agent, seen) = live.close().await;
    assert_eq!(provider.sent().len(), 5);
    assert!(sent_switch(&seen).estimate_micros > 0);
    assert!(!seen.iter().any(|e| matches!(
        e,
        AgentEvent::CompactionStarted {
            trigger: CompactionTrigger::Convert,
            turn: Some(2),
            ..
        }
    )));
    assert_rebuilds(&store, &agent);
}

#[tokio::test]
async fn a_failed_switch_compaction_converts_the_head_and_switches() {
    let after_retries = (0..=crate::agent::retry::quick().step_retries.unwrap())
        .map(|_| server_failed())
        .collect();
    for failure in [
        after_retries,
        vec![bad_request()],
        vec![stopped(StopReason::Refusal)],
        vec![stopped(StopReason::EndTurn)],
    ] {
        let store = MemoryStore::default();
        let tries = failure.len();
        let mut script = vec![
            answer("Hello.", 1_000),
            compaction(SUMMARY),
            answer("Second.", 300),
        ];
        script.extend(failure);
        script.push(answer("Third.", 400));
        let provider = scripted(script);
        let agent =
            other_without_native(&store, &provider).with_retry_policy(crate::agent::retry::quick());
        let mut live = Live::start(agent);
        up_to_the_switch(&mut live).await;
        assert!(completed(&live.turn("u3", "third").await));
        let (agent, seen) = live.close().await;
        let turn_request = 3 + tries;
        assert_eq!(provider.sent().len(), turn_request + 1);
        let retried = seen
            .iter()
            .filter(|e| matches!(e, AgentEvent::Retrying { .. }))
            .count();
        assert_eq!(retried, tries - 1);
        assert!(!provider.beta(turn_request).contains(COMPACTION_BETA));
        let body = provider.body(turn_request);
        assert_eq!(body["messages"].as_array().unwrap().len(), 1);
        assert!(first_text(&body).contains(SUMMARY));
        let failed = position(&seen, |e| {
            matches!(
                e,
                AgentEvent::CompactionFailed {
                    trigger: CompactionTrigger::Switch,
                    turn: Some(3),
                    answers: None,
                    ..
                }
            )
        });
        let converted = position(&seen, |e| {
            matches!(
                e,
                AgentEvent::Compacted {
                    trigger: CompactionTrigger::Convert,
                    turn: Some(3),
                    partial: true,
                    ..
                }
            )
        });
        let turn = position(&seen, |e| {
            matches!(e, AgentEvent::TurnStarted { turn: 3, .. })
        });
        assert!(failed < converted && converted < turn);
        assert!(!seen.iter().any(|e| matches!(
            e,
            AgentEvent::SettingsChanged {
                withdrawn: Some(_),
                ..
            }
        )));
        let logged = log_events(&store);
        let failed = logged_at(&logged, |e| {
            matches!(
                e,
                LogEvent::CompactionFailed {
                    trigger: CompactionTrigger::Switch,
                    turn: Some(3),
                    switch: Some(SwitchCompaction {
                        cause: SwitchCompactionCause::Convert,
                        ..
                    }),
                    ..
                }
            )
        });
        let convert = logged_at(&logged, |e| {
            matches!(
                e,
                LogEvent::ContextCompacted {
                    trigger: CompactionTrigger::Convert,
                    turn: Some(3),
                    partial: true,
                    ..
                }
            )
        });
        assert!(failed < convert && convert < logged_at(&logged, turn_started(3)));
        assert_eq!(started_on(&logged, 3).as_deref(), Some("other"));
        assert!(!agent.head_holds_block());
        assert_rebuilds(&store, &agent);
    }
}

#[tokio::test]
async fn a_cancelled_switch_compaction_keeps_the_old_pair_for_the_turn() {
    let store = MemoryStore::default();
    let provider = scripted(vec![
        answer("Hello.", 1_000),
        compaction(SUMMARY),
        answer("Second.", 300),
        Step::hang(),
    ]);
    let mut live = Live::start(other_without_native(&store, &provider));
    up_to_the_switch(&mut live).await;
    live.send(send("u3", "third")).await;
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
    let ended = live
        .until(|e| matches!(e, AgentEvent::TurnEnded { .. }))
        .await;
    let (agent, seen) = live.close().await;
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
    assert_eq!(provider.sent().len(), 4);
    let failed = position(&seen, |e| {
        matches!(
            e,
            AgentEvent::CompactionFailed {
                kind: CompactionFailure::Cancelled,
                trigger: CompactionTrigger::Switch,
                turn: Some(3),
                ..
            }
        )
    });
    let turn = position(&seen, |e| {
        matches!(e, AgentEvent::TurnStarted { turn: 3, .. })
    });
    assert!(failed < turn);
    let ends = seen
        .iter()
        .filter(|e| matches!(e, AgentEvent::TurnEnded { .. }))
        .count();
    assert_eq!(ends, 3);
    let logged = log_events(&store);
    let failed = logged_at(&logged, |e| {
        matches!(
            e,
            LogEvent::CompactionFailed {
                kind: CompactionFailure::Cancelled,
                turn: Some(3),
                switch: Some(_),
                ..
            }
        )
    });
    assert!(failed < logged_at(&logged, turn_started(3)));
    assert_eq!(started_on(&logged, 3).as_deref(), Some("env"));
    assert_eq!(agent.endpoint_name(), "env");
    assert!(agent.head_holds_block());
    assert_eq!(agent.epoch_number(), 1);
    assert_rebuilds(&store, &agent);
}

#[tokio::test]
async fn a_cancelled_switch_compaction_over_the_point_sends_no_automatic_compaction() {
    let store = MemoryStore::default();
    let provider = scripted(vec![
        answer("Hello.", 1_000),
        compaction(SUMMARY),
        answer("Second.", 5_000),
        Step::hang(),
    ]);
    let config = "[models.\"claude-opus-5-5\"]\ncompact_at = 4000\n\n[endpoints.other.models.\"claude-opus-5-5\"]\nnative_compaction = false\n";
    let agent = two_endpoints(logged(provider.clone(), &store), provider.clone())
        .with_model_settings(models(config));
    let mut live = Live::start(agent);
    up_to_the_switch(&mut live).await;
    live.send(send("u3", "third")).await;
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
    let ended = live
        .until(|e| matches!(e, AgentEvent::TurnEnded { .. }))
        .await;
    let (agent, seen) = live.close().await;
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
    assert!(agent.auto_due(agent.fill_estimate(), true).is_some());
    assert_eq!(provider.sent().len(), 4);
    assert!(!seen.iter().any(|e| matches!(
        e,
        AgentEvent::CompactionStarted {
            trigger: CompactionTrigger::Auto,
            ..
        }
    )));
}

#[tokio::test]
async fn a_switch_to_a_pair_whose_trial_was_refused_compacts_on_the_old_pair_first() {
    let store = MemoryStore::default();
    let provider = scripted(vec![
        answer("Hello.", 1_000),
        compaction(SUMMARY),
        beta_refused(),
        answer("Other.", 300),
        answer("Back.", 400),
        compaction("Again."),
        answer("Fourth.", 500),
        compaction("Fresh."),
        answer("Fifth.", 600),
    ]);
    let agent = two_endpoints(logged(provider.clone(), &store), provider.clone());
    let mut live = Live::start(agent);
    live.turn("u1", "first").await;
    live.compact(2, None).await;
    change(&mut live, 3, Some("other"), None).await;
    assert!(completed(&live.turn("u2", "second").await));
    change(&mut live, 4, Some("env"), None).await;
    assert!(completed(&live.turn("u3", "third").await));
    live.compact(5, None).await;
    assert!(completed(&live.turn("u4", "fourth").await));
    change(&mut live, 6, Some("other"), None).await;
    assert!(completed(&live.turn("u5", "fifth").await));
    let (agent, seen) = live.close().await;
    assert_eq!(provider.sent().len(), 9);
    assert!(provider.beta(7).contains(COMPACTION_BETA));
    assert!(!provider.beta(8).contains(COMPACTION_BETA));
    assert!(first_text(&provider.body(8)).contains("Fresh."));
    assert_eq!(sent_switch(&seen).cause, SwitchCompactionCause::Convert);
    let started = position(&seen, |e| {
        matches!(
            e,
            AgentEvent::CompactionStarted {
                trigger: CompactionTrigger::Switch,
                turn: Some(5),
                ..
            }
        )
    });
    let turn = position(&seen, |e| {
        matches!(e, AgentEvent::TurnStarted { turn: 5, .. })
    });
    assert!(started < turn);
    assert_eq!(started_on(&log_events(&store), 5).as_deref(), Some("other"));
    assert_rebuilds(&store, &agent);
}

#[tokio::test]
async fn a_retry_with_nothing_after_the_head_only_converts_first() {
    let store = MemoryStore::default();
    let provider = scripted(vec![
        answer("Hello.", 1_000),
        compaction(SUMMARY),
        failing(server_error()),
        answer("Retried.", 400),
    ]);
    let mut live = Live::start(other_without_native(&store, &provider));
    live.turn("u1", "first").await;
    live.compact(2, None).await;
    let failed = live.turn("u2", "second").await;
    assert!(
        matches!(failed, TurnOutcome::Error { offer: Some(_), .. }),
        "{failed:?}"
    );
    live.send(ClientCommand::Retry {
        turn: 2,
        endpoint: Some("other".into()),
        request: RequestId(3),
    })
    .await;
    live.until(|e| {
        matches!(
            e,
            AgentEvent::TurnStarted {
                retry_of: Some(2),
                ..
            }
        )
    })
    .await;
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
        "{ended:?}"
    );
    let (agent, seen) = live.close().await;
    assert!(!seen.iter().any(|e| matches!(
        e,
        AgentEvent::CompactionStarted {
            trigger: CompactionTrigger::Switch,
            ..
        }
    )));
    let converted = position(&seen, |e| {
        matches!(
            e,
            AgentEvent::Compacted {
                trigger: CompactionTrigger::Convert,
                turn: Some(3),
                partial: false,
                ..
            }
        )
    });
    let retried = position(&seen, |e| {
        matches!(
            e,
            AgentEvent::TurnStarted {
                retry_of: Some(2),
                ..
            }
        )
    });
    assert!(converted < retried);
    assert_eq!(provider.sent().len(), 4);
    assert!(provider.beta(2).contains(COMPACTION_BETA));
    assert!(!provider.beta(3).contains(COMPACTION_BETA));
    let body = provider.body(3);
    assert!(first_text(&body).contains(SUMMARY));
    assert!(body["messages"].to_string().contains("second"));
    assert_eq!(started_on(&log_events(&store), 3).as_deref(), Some("other"));
    assert_rebuilds(&store, &agent);
}

fn assert_rebuilds_bytes(store: &MemoryStore, agent: &Agent) {
    assert_rebuilds(store, agent);
    let rebuilt = rebuild_history(log_events(store).iter(), no_images).unwrap();
    assert_eq!(
        serde_json::to_vec(&rebuilt.messages).unwrap(),
        serde_json::to_vec(&agent.history).unwrap()
    );
    assert_eq!(
        serde_json::to_vec(&rebuilt.view()).unwrap(),
        serde_json::to_vec(&agent.view_messages()).unwrap()
    );
}

fn native_text_stop() -> Step {
    reply(
        Some("Here is a summary."),
        false,
        StopReason::EndTurn,
        60_000,
        0,
    )
}

#[tokio::test]
async fn a_native_leg_that_falls_back_writes_a_wisp_summary_and_switches() {
    let store = MemoryStore::default();
    let provider = scripted(vec![
        answer("Hello.", 1_000),
        compaction(SUMMARY),
        answer("Second.", 300),
        native_text_stop(),
        reply(Some(WRITTEN), false, StopReason::EndTurn, 60_000, 0),
        answer("Third.", 400),
    ]);
    let mut live = Live::start(other_without_native(&store, &provider));
    up_to_the_switch(&mut live).await;
    assert!(completed(&live.turn("u3", "third").await));
    let (agent, seen) = live.close().await;
    let written = position(&seen, |e| {
        matches!(
            e,
            AgentEvent::Compacted {
                trigger: CompactionTrigger::Switch,
                engine: CompactionEngine::Wisp,
                converted: false,
                partial: false,
                turn: Some(3),
                ..
            }
        )
    });
    assert!(
        written
            < position(&seen, |e| matches!(
                e,
                AgentEvent::TurnStarted { turn: 3, .. }
            ))
    );
    assert!(!seen.iter().any(|e| matches!(
        e,
        AgentEvent::Compacted {
            trigger: CompactionTrigger::Convert,
            ..
        } | AgentEvent::CompactionFailed { .. }
    )));
    assert_eq!(provider.sent().len(), 6);
    assert!(provider.body(3).get("compaction").is_some());
    assert!(provider.body(4).get("compaction").is_none());
    assert!(!provider.beta(5).contains(COMPACTION_BETA));
    let body = provider.body(5);
    assert_eq!(body["messages"].as_array().unwrap().len(), 1);
    assert!(first_text(&body).contains("Say hello."));
    let logged = log_events(&store);
    let installed = logged_at(&logged, |e| {
        matches!(
            e,
            LogEvent::ContextCompacted {
                trigger: CompactionTrigger::Switch,
                engine: CompactionEngine::Wisp,
                block: None,
                switch: Some(_),
                ..
            }
        )
    });
    assert!(installed < logged_at(&logged, turn_started(3)));
    assert_eq!(started_on(&logged, 3).as_deref(), Some("other"));
    assert!(!agent.head_holds_block());
    assert_rebuilds_bytes(&store, &agent);
}

#[tokio::test]
async fn a_failed_wisp_leg_is_recorded_as_wisps_and_the_head_is_converted() {
    let store = MemoryStore::default();
    let provider = scripted(vec![
        answer("Hello.", 1_000),
        compaction(SUMMARY),
        answer("Second.", 300),
        native_text_stop(),
        reply(
            Some("No sections here."),
            false,
            StopReason::EndTurn,
            60_000,
            0,
        ),
        answer("Third.", 400),
    ]);
    let mut live = Live::start(other_without_native(&store, &provider));
    up_to_the_switch(&mut live).await;
    assert!(completed(&live.turn("u3", "third").await));
    let (agent, seen) = live.close().await;
    let failed = position(&seen, |e| {
        matches!(
            e,
            AgentEvent::CompactionFailed {
                trigger: CompactionTrigger::Switch,
                engine: Some(CompactionEngine::Wisp),
                kind: CompactionFailure::CheckFailed,
                turn: Some(3),
                usage: Some(_),
                ..
            }
        )
    });
    let converted = position(&seen, |e| {
        matches!(
            e,
            AgentEvent::Compacted {
                trigger: CompactionTrigger::Convert,
                turn: Some(3),
                partial: true,
                converted: true,
                ..
            }
        )
    });
    let turn = position(&seen, |e| {
        matches!(e, AgentEvent::TurnStarted { turn: 3, .. })
    });
    assert!(failed < converted && converted < turn);
    assert_eq!(provider.sent().len(), 6);
    assert!(!provider.beta(5).contains(COMPACTION_BETA));
    assert!(first_text(&provider.body(5)).contains(SUMMARY));
    let logged = log_events(&store);
    let record = logged_at(&logged, |e| {
        matches!(
            e,
            LogEvent::CompactionFailed {
                trigger: CompactionTrigger::Switch,
                engine: Some(CompactionEngine::Wisp),
                kind: CompactionFailure::CheckFailed,
                switch: Some(_),
                ..
            }
        )
    });
    let convert = logged_at(&logged, |e| {
        matches!(
            e,
            LogEvent::ContextCompacted {
                trigger: CompactionTrigger::Convert,
                turn: Some(3),
                partial: true,
                ..
            }
        )
    });
    assert!(record < convert && convert < logged_at(&logged, turn_started(3)));
    assert_eq!(started_on(&logged, 3).as_deref(), Some("other"));
    assert_rebuilds_bytes(&store, &agent);
}

#[tokio::test]
async fn a_switch_back_while_the_switch_compaction_runs_keeps_the_fresh_block() {
    let store = MemoryStore::default();
    let gate = Arc::new(Semaphore::new(0));
    let fresh = compaction("Fresh.").script;
    let provider = scripted(vec![
        answer("Hello.", 1_000),
        compaction(SUMMARY),
        answer("Second.", 300),
        Step::from(fresh).gated(&gate),
        answer("Third.", 400),
    ]);
    let mut live = Live::start(other_without_native(&store, &provider));
    up_to_the_switch(&mut live).await;
    live.send(send("u3", "third")).await;
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
    change(&mut live, 4, Some("env"), None).await;
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
        AgentEvent::Compacted {
            trigger: CompactionTrigger::Switch,
            engine: CompactionEngine::Native,
            ..
        }
    )));
    assert!(!seen.iter().any(|e| matches!(
        e,
        AgentEvent::Compacted {
            trigger: CompactionTrigger::Convert,
            ..
        }
    )));
    assert_eq!(provider.sent().len(), 5);
    assert!(provider.beta(4).contains(COMPACTION_BETA));
    assert_eq!(
        provider.body(4)["messages"][0]["content"][0],
        raw_block("Fresh.")
    );
    assert_eq!(started_on(&log_events(&store), 3).as_deref(), Some("env"));
    assert!(agent.head_holds_block());
    assert_rebuilds(&store, &agent);
}

#[tokio::test]
async fn a_refused_block_during_the_switch_compaction_converts_the_head() {
    let store = MemoryStore::default();
    let provider = scripted(vec![
        answer("Hello.", 1_000),
        compaction(SUMMARY),
        answer("Second.", 300),
        block_refused(),
        answer("Third.", 400),
    ]);
    let mut live = Live::start(other_without_native(&store, &provider));
    up_to_the_switch(&mut live).await;
    assert!(completed(&live.turn("u3", "third").await));
    let (agent, seen) = live.close().await;
    let converted = position(&seen, |e| {
        matches!(
            e,
            AgentEvent::Compacted {
                trigger: CompactionTrigger::Switch,
                engine: CompactionEngine::Wisp,
                converted: true,
                partial: true,
                turn: Some(3),
                ..
            }
        )
    });
    let turn = position(&seen, |e| {
        matches!(e, AgentEvent::TurnStarted { turn: 3, .. })
    });
    assert!(converted < turn);
    assert_eq!(
        seen.iter()
            .filter(|e| matches!(e, AgentEvent::Compacted { .. }))
            .count(),
        2
    );
    assert_eq!(provider.sent().len(), 5);
    assert!(!provider.beta(4).contains(COMPACTION_BETA));
    let head = first_text(&provider.body(4));
    assert!(head.contains(SUMMARY));
    assert!(head.contains("turns=\"1–2\""), "{head}");
    assert_eq!(started_on(&log_events(&store), 3).as_deref(), Some("other"));
    assert!(!agent.head_holds_block());
    assert_rebuilds_bytes(&store, &agent);
}

#[tokio::test]
async fn a_search_tail_left_open_is_closed_once_before_the_switch_compaction_installs() {
    let store = MemoryStore::default();
    let provider = scripted(vec![
        answer("Hello.", 1_000),
        compaction(SUMMARY),
        paused_search(300, 10),
        failing(server_error()),
        compaction("Fresh."),
        answer("Third.", 400),
    ]);
    let mut live = Live::start(other_without_native(&store, &provider));
    live.turn("u1", "first").await;
    live.compact(2, None).await;
    let failed = live.turn("u2", "second").await;
    assert!(matches!(failed, TurnOutcome::Error { .. }), "{failed:?}");
    change(&mut live, 3, Some("other"), None).await;
    assert!(completed(&live.turn("u3", "third").await));
    let (agent, _) = live.close().await;
    let logged = log_events(&store);
    let closed: Vec<usize> = logged
        .iter()
        .enumerate()
        .filter(
            |(_, e)| matches!(e, LogEvent::TailClosed { calls, .. } if calls == &["srvtoolu_1"]),
        )
        .map(|(i, _)| i)
        .collect();
    assert_eq!(closed.len(), 1, "{logged:#?}");
    let installed = logged_at(&logged, |e| {
        matches!(
            e,
            LogEvent::ContextCompacted {
                trigger: CompactionTrigger::Switch,
                ..
            }
        )
    });
    assert!(closed[0] < installed);
    assert_eq!(provider.sent().len(), 6);
    assert!(
        !provider.body(5)["messages"]
            .to_string()
            .contains("server_tool_use")
    );
    assert_rebuilds_bytes(&store, &agent);
}

fn resumed_on_two(provider: Arc<Fake>, records: &[wisp_protocol::log::LogRecord]) -> Agent {
    let (_, log) = continued(records);
    let agent = Agent::new(provider.clone(), default_session())
        .with_clock(Arc::new(crate::clock::StillClock))
        .with_log(log, log_context());
    two_endpoints(agent, provider)
        .with_model_settings(models(
            "[endpoints.other.models.\"claude-opus-5-5\"]\nnative_compaction = false\n",
        ))
        .resume(records, 0)
        .unwrap()
}

#[tokio::test]
async fn a_session_after_a_switch_compaction_resumes_with_the_same_bytes() {
    for failure in [Vec::new(), vec![bad_request()]] {
        let failed = !failure.is_empty();
        let store = MemoryStore::default();
        let mut script = vec![
            answer("Hello.", 1_000),
            compaction(SUMMARY),
            answer("Second.", 300),
        ];
        if failed {
            script.extend(failure);
        } else {
            script.push(summarised("Fresh.", 2_000, 0, vec![]));
        }
        script.extend([answer("Third.", 400), answer("Fourth.", 500)]);
        let provider = scripted(script);
        let agent = other_without_native(&store, &provider)
            .with_clock(Arc::new(crate::clock::StillClock))
            .with_retry_policy(crate::agent::retry::quick());
        let mut live = Live::start(agent);
        up_to_the_switch(&mut live).await;
        assert!(completed(&live.turn("u3", "third").await));
        let records = records(&store);
        assert_eq!(
            records.iter().any(|r| matches!(
                r.event,
                LogEvent::CompactionFailed {
                    switch: Some(_),
                    ..
                }
            )),
            failed
        );
        assert!(completed(&live.turn("u4", "fourth").await));
        live.close().await;

        let sent_resumed = scripted(vec![answer("Fourth.", 500)]);
        let mut again = Live::start(resumed_on_two(sent_resumed.clone(), &records));
        assert!(completed(&again.turn("u4", "fourth").await));
        again.close().await;
        let last = provider.sent().len() - 1;
        assert_eq!(
            sent_resumed.body(0),
            provider.body(last),
            "failed: {failed}"
        );
        assert_eq!(
            sent_resumed.beta(0),
            provider.beta(last),
            "failed: {failed}"
        );
    }
}

#[tokio::test]
async fn a_switch_compaction_that_may_have_been_billed_logs_a_lower_bound() {
    let store = MemoryStore::default();
    let provider = scripted(vec![
        answer("Hello.", 1_000),
        compaction(SUMMARY),
        answer("Second.", 300),
        cut_after_start(
            vec![iteration(100, 900, 50)],
            mid_stream(wisp_protocol::RetryClass::Never),
        ),
        answer("Third.", 400),
    ]);
    let mut live = Live::start(other_without_native(&store, &provider));
    up_to_the_switch(&mut live).await;
    assert!(completed(&live.turn("u3", "third").await));
    live.close().await;
    let failed: Vec<(bool, bool)> = pick!(
        &log_events(&store),
        LogEvent::CompactionFailed {
            lower_bound,
            switch,
            ..
        } => (*lower_bound, switch.is_some())
    );
    assert_eq!(failed, [(true, true)]);
}

fn miss_causes(seen: &[AgentEvent]) -> Vec<wisp_protocol::MissCause> {
    seen.iter()
        .filter_map(|e| match e {
            AgentEvent::CacheMiss { cause, .. } => cause.clone(),
            _ => None,
        })
        .collect()
}

#[test]
fn a_compaction_that_does_not_install_leaves_the_switch_expected_for_the_next_step() {
    crate::stack_alarm::on_alarm_stack(crate::stack_alarm::Timer::Real, || async {
        let store = MemoryStore::default();
        let provider = scripted(vec![
            answer("Hello.", 300),
            answer("Again.", 50_000),
            failing(ProviderError::new(
                ErrorKind::ContextOverflow,
                "prompt is too long",
            )),
            stopped(StopReason::MaxTokens),
            bad_request(),
            answer("Third.", 50_000),
        ]);
        let mut live = Live::start(logged(provider.clone(), &store).with_auto_compact(false));
        assert!(completed(&live.turn("u1", "first").await));
        assert!(completed(&live.turn("u2", "again").await));
        live.send(ClientCommand::shortcut(
            RequestId(2),
            None,
            Some("claude-sonnet-5-5".into()),
            None,
        ))
        .await;
        live.until(|e| {
            matches!(
                e,
                AgentEvent::SettingsChanged {
                    answers: Some(RequestId(2)),
                    ..
                }
            )
        })
        .await;
        let failed = live.turn("u3", "overflow").await;
        assert!(matches!(failed, TurnOutcome::Error { .. }), "{failed:?}");
        assert!(completed(&live.turn("u4", "fourth").await));
        let (_, seen) = live.close().await;
        assert_eq!(provider.sent().len(), 6, "{seen:#?}");
        let expected = wisp_protocol::MissCause::Expected {
            reason: "model_switch".into(),
        };
        assert_eq!(miss_causes(&seen), [expected.clone(), expected]);
    });
}
