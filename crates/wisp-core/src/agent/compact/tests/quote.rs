use wisp_protocol::settings::{Consequence, SwitchQuote, SwitchTarget, Withdrawal};
use wisp_protocol::{SwitchCompactionCause, settings::SettingTarget};

use super::marker::{assert_rebuilds, change, other_without_native};
use super::*;

pub(super) fn big() -> String {
    "word ".repeat(100_000)
}

fn quote_for<'a>(quotes: &'a [SwitchQuote], endpoint: &str) -> Option<&'a Consequence> {
    quotes.iter().find_map(|q| match &q.target {
        SwitchTarget::Endpoint(name) if name == endpoint => q.consequence.as_ref(),
        _ => None,
    })
}

fn switch_quote(seen: &[AgentEvent], request: u64) -> Option<Consequence> {
    seen.iter().find_map(|e| match e {
        AgentEvent::SettingsChanged {
            answers: Some(RequestId(r)),
            changes,
            ..
        } if *r == request => changes
            .iter()
            .find(|c| matches!(&c.target, SettingTarget::Name(n) if n.spec().name == "endpoint"))
            .and_then(|c| c.consequence.clone()),
        _ => None,
    })
}

pub(super) fn withdrawal(seen: &[AgentEvent]) -> Option<usize> {
    seen.iter().position(|e| {
        matches!(
            e,
            AgentEvent::SettingsChanged {
                answers: None,
                withdrawn: Some(_),
                ..
            }
        )
    })
}

pub(super) async fn describe(live: &mut Live, request: u64) -> Vec<SwitchQuote> {
    live.send(ClientCommand::DescribeSettings {
        request: RequestId(request),
    })
    .await;
    let answer = live
        .until(|e| matches!(e, AgentEvent::SettingsSnapshot(s) if s.answers == RequestId(request)))
        .await;
    match answer {
        AgentEvent::SettingsSnapshot(snapshot) => snapshot.switches,
        other => panic!("{other:?}"),
    }
}

#[tokio::test]
async fn a_switch_is_quoted_on_its_change_and_in_the_snapshot() {
    let store = MemoryStore::default();
    let provider = scripted(vec![
        answer("Hello.", 1_000),
        compaction(SUMMARY),
        answer("Second.", 300),
    ]);
    let mut live = Live::start(other_without_native(&store, &provider));
    live.turn("u1", "first").await;
    live.compact(2, None).await;
    let alone = describe(&mut live, 3).await;
    let Some(Consequence::CompactsBeforeSwitch {
        estimate_micros: 0,
        head_tokens,
        cause: SwitchCompactionCause::Convert,
    }) = quote_for(&alone, "other")
    else {
        panic!("{alone:#?}");
    };
    assert!(*head_tokens > 0);
    assert!(quote_for(&alone, "env").is_none());
    assert!(completed(&live.turn("u2", "second").await));
    change(&mut live, 4, Some("other"), None).await;
    let quoted = describe(&mut live, 5).await;
    let (_, seen) = live.close().await;
    let Some(Consequence::CompactsBeforeSwitch {
        estimate_micros,
        head_tokens,
        cause: SwitchCompactionCause::Convert,
    }) = switch_quote(&seen, 4)
    else {
        panic!("{seen:#?}");
    };
    assert!(estimate_micros > 0);
    assert!(head_tokens > 0);
    assert!(matches!(
        quote_for(&quoted, "other"),
        Some(Consequence::CompactsBeforeSwitch { estimate_micros, .. }) if *estimate_micros > 0
    ));
}

#[tokio::test]
async fn a_cancelled_switch_compaction_withdraws_the_switch_after_the_turn() {
    let store = MemoryStore::default();
    let provider = scripted(vec![
        answer("Hello.", 1_000),
        compaction(SUMMARY),
        answer("Second.", 300),
        Step::hang(),
        answer("Fourth.", 400),
    ]);
    let mut live = Live::start(other_without_native(&store, &provider));
    live.turn("u1", "first").await;
    live.compact(2, None).await;
    assert!(completed(&live.turn("u2", "second").await));
    change(&mut live, 3, Some("other"), None).await;
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
    live.until(|e| matches!(e, AgentEvent::TurnEnded { .. }))
        .await;
    live.until(|e| withdrawal(std::slice::from_ref(e)).is_some())
        .await;
    assert!(completed(&live.turn("u4", "fourth").await));
    let (agent, seen) = live.close().await;
    let ended = seen
        .iter()
        .rposition(|e| {
            matches!(
                e,
                AgentEvent::TurnEnded {
                    outcome: TurnOutcome::Cancelled { .. },
                    ..
                }
            )
        })
        .unwrap();
    let withdrawn = withdrawal(&seen).unwrap();
    assert!(ended < withdrawn);
    let AgentEvent::SettingsChanged {
        endpoint,
        withdrawn: Some(Withdrawal::SwitchCompactionCancelled),
        restored: Some(restored),
        changes,
        cache,
        ..
    } = &seen[withdrawn]
    else {
        panic!("{:?}", seen[withdrawn]);
    };
    assert_eq!(endpoint, "env");
    assert_eq!(restored.effort, agent.session.effort);
    assert_eq!(*cache, CacheEffect::Kept);
    let named: Vec<&str> = changes.iter().map(|c| c.target.spec().name).collect();
    assert_eq!(named, ["model", "endpoint"]);
    assert_eq!(agent.endpoint_name(), "env");
    assert!(agent.pending_settings().is_none());
    assert!(provider.beta(4).contains(COMPACTION_BETA));
    let logged = log_events(&store);
    let turn_end = logged
        .iter()
        .position(|e| matches!(e, LogEvent::TurnEnded { turn: 3, .. }))
        .unwrap();
    let record = logged
        .iter()
        .position(|e| {
            matches!(
                e,
                LogEvent::SettingsChanged {
                    withdrawn: Some(Withdrawal::SwitchCompactionCancelled),
                    ..
                }
            )
        })
        .unwrap();
    assert!(turn_end < record);
    assert_rebuilds(&store, &agent);
}

#[tokio::test]
async fn a_failed_switch_compaction_whose_converted_view_doesnt_fit_withdraws_before_the_turn() {
    let store = MemoryStore::default();
    let provider = scripted(vec![
        answer("Hello.", 1_000),
        compaction(SUMMARY),
        answer("Second.", 300),
        failing(server_error()),
        answer("Third.", 400),
    ]);
    let mut live = Live::start(small_other(&store, &provider));
    live.turn("u1", "first").await;
    live.compact(2, None).await;
    assert!(completed(&live.turn("u2", "second").await));
    change(&mut live, 3, Some("other"), None).await;
    assert!(completed(&live.turn("u3", &big()).await));
    let (agent, seen) = live.close().await;
    let failed = seen
        .iter()
        .position(|e| {
            matches!(
                e,
                AgentEvent::CompactionFailed {
                    trigger: CompactionTrigger::Switch,
                    ..
                }
            )
        })
        .unwrap();
    let withdrawn = withdrawal(&seen).unwrap();
    let started = seen
        .iter()
        .position(|e| matches!(e, AgentEvent::TurnStarted { turn: 3, .. }))
        .unwrap();
    assert!(failed < withdrawn && withdrawn < started);
    assert!(matches!(
        &seen[withdrawn],
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
    assert_eq!(provider.sent().len(), 5);
    assert!(provider.beta(4).contains(COMPACTION_BETA));
    assert_eq!(agent.endpoint_name(), "env");
    assert!(agent.head_holds_block());
    assert_rebuilds(&store, &agent);
}

pub(super) async fn failed_third_turn(live: &mut Live) {
    failed_third_turn_with(live, "third").await;
}

pub(super) async fn failed_third_turn_with(live: &mut Live, third: &str) {
    live.turn("u1", "first").await;
    live.compact(2, None).await;
    assert!(completed(&live.turn("u2", "second").await));
    let failed = live.turn("u3", third).await;
    assert!(
        matches!(failed, TurnOutcome::Error { offer: Some(_), .. }),
        "{failed:?}"
    );
    live.send(ClientCommand::Retry {
        turn: 3,
        endpoint: Some("other".into()),
        request: RequestId(4),
    })
    .await;
    live.until(|e| {
        matches!(
            e,
            AgentEvent::TurnStarted {
                retry_of: Some(3),
                ..
            }
        )
    })
    .await;
    live.until(|e| matches!(e, AgentEvent::TurnEnded { .. }))
        .await;
}

#[tokio::test]
async fn a_retry_away_from_a_failing_endpoint_compacts_there_once_then_converts() {
    let store = MemoryStore::default();
    let provider = scripted(vec![
        answer("Hello.", 1_000),
        compaction(SUMMARY),
        answer("Second.", 300),
        failing(server_error()),
        summarised("Fresh.", 2_000, 0, vec![]),
        answer("Retried.", 400),
    ]);
    let mut live = Live::start(other_without_native(&store, &provider));
    failed_third_turn(&mut live).await;
    let (agent, seen) = live.close().await;
    assert!(seen.iter().any(|e| matches!(
        e,
        AgentEvent::Compacted {
            trigger: CompactionTrigger::Switch,
            turn: Some(4),
            ..
        }
    )));
    assert_eq!(provider.sent().len(), 6);
    assert!(provider.beta(4).contains(COMPACTION_BETA));
    assert!(!provider.beta(5).contains(COMPACTION_BETA));
    let body = provider.body(5);
    assert!(
        body["messages"][0]["content"][0]["text"]
            .as_str()
            .unwrap()
            .contains("Fresh.")
    );
    assert!(body["messages"].to_string().contains("third"));
    assert_eq!(agent.endpoint_name(), "other");
    assert_rebuilds(&store, &agent);
}

#[tokio::test]
async fn a_failed_compaction_on_the_failing_endpoint_isnt_retried_and_converts() {
    let store = MemoryStore::default();
    let provider = scripted(vec![
        answer("Hello.", 1_000),
        compaction(SUMMARY),
        answer("Second.", 300),
        failing(server_error()),
        server_failed(),
        answer("Retried.", 400),
    ]);
    let agent = small_other(&store, &provider).with_retry_policy(crate::agent::retry::quick());
    let mut live = Live::start(agent);
    failed_third_turn_with(&mut live, &"word ".repeat(30_000)).await;
    let (agent, seen) = live.close().await;
    assert!(
        !seen
            .iter()
            .any(|e| matches!(e, AgentEvent::Retrying { .. }))
    );
    let failed = seen
        .iter()
        .position(|e| {
            matches!(
                e,
                AgentEvent::CompactionFailed {
                    trigger: CompactionTrigger::Switch,
                    turn: Some(4),
                    ..
                }
            )
        })
        .unwrap();
    let converted = seen
        .iter()
        .position(|e| {
            matches!(
                e,
                AgentEvent::Compacted {
                    trigger: CompactionTrigger::Convert,
                    turn: Some(4),
                    partial: true,
                    ..
                }
            )
        })
        .unwrap();
    assert!(failed < converted);
    assert!(withdrawal(&seen).is_none());
    assert_eq!(provider.sent().len(), 6);
    assert!(!provider.beta(5).contains(COMPACTION_BETA));
    assert_eq!(agent.endpoint_name(), "other");
    assert_rebuilds(&store, &agent);
}

#[tokio::test]
async fn a_retrys_native_leg_that_falls_back_gets_one_wisp_attempt_on_the_failing_endpoint() {
    let store = MemoryStore::default();
    let text_stop = super::wisp::reply(
        Some("Here is a summary."),
        false,
        StopReason::EndTurn,
        60_000,
        0,
    );
    let wisp = super::wisp::reply(Some(WRITTEN), false, StopReason::EndTurn, 60_000, 0);
    let provider = scripted(vec![
        answer("Hello.", 1_000),
        compaction(SUMMARY),
        answer("Second.", 300),
        failing(server_error()),
        text_stop,
        wisp,
        answer("Retried.", 400),
    ]);
    let mut live = Live::start(other_without_native(&store, &provider));
    failed_third_turn(&mut live).await;
    let (agent, seen) = live.close().await;
    assert!(seen.iter().any(|e| matches!(
        e,
        AgentEvent::Compacted {
            trigger: CompactionTrigger::Switch,
            engine: CompactionEngine::Wisp,
            converted: false,
            turn: Some(4),
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
    assert_eq!(provider.sent().len(), 7);
    assert!(provider.body(4).get("compaction").is_some());
    assert!(provider.body(5).get("compaction").is_none());
    assert!(!provider.beta(6).contains(COMPACTION_BETA));
    assert!(
        provider.body(6)["messages"][0]["content"][0]["text"]
            .as_str()
            .unwrap()
            .contains("Say hello.")
    );
    assert_eq!(agent.endpoint_name(), "other");
    assert_rebuilds(&store, &agent);
}

#[tokio::test]
async fn a_switching_retry_after_a_saved_step_sends_the_head_and_the_continue_note() {
    let store = MemoryStore::default();
    let provider = scripted(vec![
        answer("Hello.", 1_000),
        compaction(SUMMARY),
        paused_search(300, 10),
        failing(server_error()),
        compaction("Fresh."),
        answer("Done.", 400),
    ]);
    let mut live = Live::start(other_without_native(&store, &provider));
    live.turn("u1", "first").await;
    live.compact(2, None).await;
    change(&mut live, 5, None, Some(wisp_protocol::Effort::Low)).await;
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
    let ended = live
        .until(|e| {
            matches!(
                e,
                AgentEvent::TurnEnded {
                    outcome: TurnOutcome::Completed { .. } | TurnOutcome::Error { .. },
                    ..
                }
            )
        })
        .await;
    let (agent, _) = live.close().await;
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
    assert_eq!(provider.sent().len(), 6);
    assert!(!provider.beta(5).contains(COMPACTION_BETA));
    let messages = provider.body(5)["messages"].as_array().unwrap().clone();
    assert_eq!(messages.len(), 2, "{messages:#?}");
    assert_eq!(messages[1]["role"], "system");
    assert_eq!(messages[1]["output_config"]["effort"], "low");
    let content = messages[0]["content"].as_array().unwrap();
    assert!(content[0]["text"].as_str().unwrap().contains("Fresh."));
    assert!(
        content.iter().any(|b| b["text"] == CONTINUE_NOTE),
        "{content:#?}"
    );
    assert_rebuilds(&store, &agent);
}

#[tokio::test]
async fn a_switching_retry_after_a_failed_pause_that_lands_at_the_head_sends_one_note() {
    for compacts in [true, false] {
        let store = MemoryStore::default();
        let fifth = if compacts {
            compaction("Fresh.")
        } else {
            failing(server_error())
        };
        let provider = scripted(vec![
            answer("Hello.", 1_000),
            compaction(SUMMARY),
            paused_search(300, 10),
            failing(server_error()),
            fifth,
            answer("Done.", 400),
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
            "{compacts}: {ended:?}"
        );
        let trigger = if compacts {
            CompactionTrigger::Switch
        } else {
            CompactionTrigger::Convert
        };
        assert!(
            seen.iter()
                .any(|e| matches!(e, AgentEvent::Compacted { trigger: t, .. } if *t == trigger)),
            "{compacts}"
        );
        assert_eq!(provider.sent().len(), 6);
        let body = provider.body(5);
        let text = body["messages"].to_string();
        assert_eq!(text.matches(CONTINUE_NOTE).count(), 1, "{compacts}: {text}");
        assert!(
            !text.contains(wisp_protocol::CLOSED_PAUSE_NOTE),
            "{compacts}: {text}"
        );
        let view = agent.view_messages();
        let request = &view[..view.len() - 1];
        let layout = crate::agent::check_layout(
            request,
            &vec![false; request.len()],
            &agent.search.closed.calls,
            agent.provider().as_ref(),
        );
        assert!(layout.is_ok(), "{compacts}: {layout:?}");
        assert_rebuilds(&store, &agent);
    }
}

#[tokio::test]
async fn a_refused_block_on_a_switched_retry_replaces_the_switch_note_with_one_continue_note() {
    for (name, refusal) in [("block", block_refused()), ("beta", beta_refused())] {
        let store = MemoryStore::default();
        let provider = scripted(vec![
            answer("Hello.", 1_000),
            compaction(SUMMARY),
            paused_search(300, 10),
            failing(server_error()),
            refusal,
            answer("Done.", 400),
        ]);
        let agent = two_endpoints(logged(provider.clone(), &store), provider.clone());
        let mut live = Live::start(agent);
        live.turn("u1", "first").await;
        live.compact(2, None).await;
        let failed = live.turn("u2", "second").await;
        assert!(
            matches!(failed, TurnOutcome::Error { offer: Some(_), .. }),
            "{name}: {failed:?}"
        );
        live.send(ClientCommand::Retry {
            turn: 2,
            endpoint: Some("other".into()),
            request: RequestId(3),
        })
        .await;
        let ended = live
            .until(|e| matches!(e, AgentEvent::TurnEnded { .. }))
            .await;
        let (agent, _) = live.close().await;
        assert!(
            matches!(
                ended,
                AgentEvent::TurnEnded {
                    outcome: TurnOutcome::Completed { .. },
                    ..
                }
            ),
            "{name}: {ended:?}"
        );
        assert_eq!(provider.sent().len(), 6, "{name}");
        let refused = provider.body(4)["messages"].to_string();
        assert!(
            refused.contains(wisp_protocol::CLOSED_PAUSE_NOTE),
            "{name}: {refused}"
        );
        let text = provider.body(5)["messages"].to_string();
        assert_eq!(text.matches(CONTINUE_NOTE).count(), 1, "{name}: {text}");
        assert!(
            !text.contains(wisp_protocol::CLOSED_PAUSE_NOTE),
            "{name}: {text}"
        );
        assert_rebuilds(&store, &agent);
    }
}

#[tokio::test]
async fn a_same_account_switch_to_a_model_only_the_list_has_is_quoted_at_the_lists_window() {
    let store = MemoryStore::default();
    let provider = Fake::with(
        Knobs {
            prefix: Prefix::Sent,
            caps: Some(Caps::Fn(narrow_or_row)),
            documented: Some(false),
            list: Some(Arc::new(crate::models::list::tests::sample())),
            ..Knobs::default()
        },
        [answer("Hello.", 300_000)],
    );
    let mut live = Live::start(two_endpoints(logged(provider.clone(), &store), provider));
    assert!(completed(&live.turn("u1", "first").await));
    let quotes = describe(&mut live, 2).await;
    live.close().await;
    let model = |id: &str| {
        quotes
            .iter()
            .find(|q| matches!(&q.target, SwitchTarget::Model(m) if m == id))
            .and_then(|q| q.consequence.clone())
    };
    assert!(
        matches!(
            model("gpt-new"),
            Some(Consequence::CompactsBeforeSwitch {
                cause: SwitchCompactionCause::SmallerWindow,
                ..
            })
        ),
        "{quotes:#?}"
    );
}
