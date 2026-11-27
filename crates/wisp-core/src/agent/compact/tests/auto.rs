use wisp_protocol::CONTINUE_NOTE;
use wisp_protocol::log::LEAD_FILL;

use super::*;
use crate::agent::compact::auto::TOO_LARGE;
use crate::agent::retry::{RetryPolicy, quick};
use crate::resume::LoggedSession;
use crate::test_guard::records;

pub(super) const LOW: &str = "[models.\"claude-opus-5-5\"]\ncompact_at = 2000\n";
const LOW_WISP: &str =
    "[models.\"claude-opus-5-5\"]\ncompact_at = 2000\nnative_compaction = false\n";
const TINY: &str = "[models.\"claude-opus-5-5\"]\ncompact_at = 10\n";
const SMALL_WINDOW: &str = "[models.\"claude-opus-5-5\"]\ncontext_window = 104000\n";

pub(super) fn no_retries(agent: Agent) -> Agent {
    agent.with_retry_policy(RetryPolicy {
        step_retries: Some(0),
        ..quick()
    })
}

pub(super) fn auto_started(event: &AgentEvent) -> bool {
    matches!(
        event,
        AgentEvent::CompactionStarted {
            trigger: CompactionTrigger::Auto,
            ..
        }
    )
}

pub(super) fn body_text(body: &Value) -> String {
    body["messages"].to_string()
}

pub(super) fn order(store: &MemoryStore, turn: u64) -> Vec<&'static str> {
    log_events(store)
        .iter()
        .filter_map(|e| match e {
            LogEvent::ContextCompacted { turn: t, .. } if *t == Some(turn) => Some("compacted"),
            LogEvent::CompactionFailed { turn: t, .. } if *t == Some(turn) => Some("failed"),
            LogEvent::TurnStarted { turn: t, .. } if *t == turn => Some("started"),
            LogEvent::TurnEnded { turn: t, .. } if *t == turn => Some("ended"),
            _ => None,
        })
        .collect()
}

fn context_notes(store: &MemoryStore) -> Vec<Option<String>> {
    pick!(
        &log_events(store),
        LogEvent::TurnStarted { lead, .. } => lead.text(LEAD_FILL).map(str::to_owned)
    )
}

pub(super) fn after_compaction(store: &MemoryStore) -> Vec<String> {
    let records = log_events(store);
    let at = records
        .iter()
        .position(|e| matches!(e, LogEvent::ContextCompacted { .. }))
        .unwrap();
    records[at + 1..]
        .iter()
        .filter_map(|e| match e {
            LogEvent::MessageCommitted { message, .. } => Some(format!(
                "{:?} {}",
                message.role,
                serde_json::to_string(&message.content).unwrap()
            )),
            LogEvent::InputCommitted { inputs, .. } => Some(format!(
                "input {}",
                inputs
                    .iter()
                    .map(|i| i.text.as_str())
                    .collect::<Vec<_>>()
                    .join("|")
            )),
            _ => None,
        })
        .collect()
}

async fn steer_first_step(live: &mut Live, gate: &Arc<Semaphore>, id: &str, text: &str) {
    live.send(send(id, text)).await;
    live.until(|e| matches!(e, AgentEvent::StepStarted { .. }))
        .await;
    live.send(send(&format!("{id}-steer"), "and this")).await;
    live.until(|e| matches!(e, AgentEvent::InputAccepted { .. }))
        .await;
    gate.add_permits(1);
}

async fn steered_turn(live: &mut Live, gate: &Arc<Semaphore>, id: &str, text: &str) -> TurnOutcome {
    steer_first_step(live, gate, id, text).await;
    match live
        .until(|e| matches!(e, AgentEvent::TurnEnded { .. }))
        .await
    {
        AgentEvent::TurnEnded { outcome, .. } => outcome,
        _ => unreachable!(),
    }
}

pub(super) fn assert_prefix_kept(provider: &Fake) {
    let sent = provider.sent();
    let first = strip_cache_control(&sent[0].body());
    for sent in &sent[1..] {
        let body = strip_cache_control(&sent.body());
        assert_eq!(body["system"], first["system"]);
        assert_eq!(body["tools"], first["tools"]);
    }
}

#[tokio::test]
async fn a_turn_over_the_point_compacts_before_its_first_request() {
    let store = MemoryStore::default();
    let provider = scripted(vec![
        answer("Hello.", 5_000),
        compaction(SUMMARY),
        answer("Again.", 300),
    ]);
    let mut live = Live::start(on(&provider, &store, LOW));
    assert!(completed(&live.turn("u1", "first").await));
    assert!(completed(&live.turn("u2", "second").await));
    let (agent, seen) = live.close().await;
    let second: Vec<&AgentEvent> = seen
        .iter()
        .skip_while(|e| !auto_started(e))
        .take_while(|e| !matches!(e, AgentEvent::TurnStarted { .. }))
        .filter(|e| {
            matches!(
                e,
                AgentEvent::CompactionStarted { .. }
                    | AgentEvent::Compacted { .. }
                    | AgentEvent::ContextChanged { .. }
            )
        })
        .collect();
    assert!(
        matches!(
            second.as_slice(),
            [
                AgentEvent::CompactionStarted { turn: Some(2), .. },
                AgentEvent::Compacted {
                    trigger: CompactionTrigger::Auto,
                    turn: Some(2),
                    answers: None,
                    ..
                },
                AgentEvent::ContextChanged { epoch: 1, .. },
                ..
            ]
        ),
        "{second:#?}"
    );
    assert_eq!(order(&store, 2), ["compacted", "started", "ended"]);
    let notes = context_notes(&store);
    assert!(
        notes[0]
            .as_deref()
            .unwrap()
            .ends_with("Wisp compacts at 2k.\n"),
        "{notes:?}"
    );
    let line = notes[1].as_deref().unwrap();
    assert!(
        line.starts_with("Context: ") && !line.contains("5k"),
        "{line}"
    );
    let sent = provider.sent();
    assert_eq!(sent.len(), 3);
    let last = body_text(&sent[2].body());
    assert!(last.contains("second") && last.contains(SUMMARY), "{last}");
    assert!(!last.contains("Hello."), "{last}");
    let compacted_cost = seen.iter().find_map(|e| match e {
        AgentEvent::Compacted { cost: Some(c), .. } => Some(c.turn_usd),
        _ => None,
    });
    let turn_cost = seen.iter().rev().find_map(|e| match e {
        AgentEvent::TurnEnded {
            outcome: TurnOutcome::Completed { cost: Some(c), .. },
            ..
        } => Some(c.turn_usd),
        _ => None,
    });
    assert!(turn_cost.unwrap() > compacted_cost.unwrap());
    assert_eq!(agent.compaction.compactions, 1);
    assert_prefix_kept(&provider);
}

#[tokio::test]
async fn auto_compact_on_states_the_point_and_off_restores_the_meter_and_line() {
    for auto in [true, false] {
        let store = MemoryStore::default();
        let provider = scripted(vec![answer("Hello.", 5_000), answer("Again.", 300)]);
        let agent = logged(provider.clone(), &store).with_auto_compact(auto);
        let mut live = Live::start(agent);
        assert!(completed(&live.turn("u1", "first").await));
        assert!(completed(&live.turn("u2", "second").await));
        let (agent, seen) = live.close().await;
        let limits: Vec<(Option<u64>, Option<u64>)> = pick!(
            &seen,
            AgentEvent::ContextChanged {
                compact_at,
                meter_limit,
                ..
            } => (*compact_at, *meter_limit)
        );
        assert!(!limits.is_empty());
        for (point, limit) in limits {
            assert_eq!(point, Some(850_000));
            assert_eq!(limit, Some(850_000));
        }
        let notes = context_notes(&store);
        let wanted = if auto {
            "Wisp compacts at 850k.\n"
        } else {
            "Automatic compaction is off.\n"
        };
        assert!(notes[1].as_deref().unwrap().ends_with(wanted), "{notes:?}");
        assert_eq!(
            agent
                .report(ReportDetail::Summary, RequestId(9))
                .auto_compact,
            auto
        );
        assert!(!seen.iter().any(auto_started));
    }
    for auto in [true, false] {
        let store = MemoryStore::default();
        let provider = scripted(vec![answer("Hello.", 5_000), answer("Again.", 300)]);
        let agent = on(&provider, &store, LOW).with_auto_compact(auto);
        let mut live = Live::start(agent);
        if auto {
            live.turn("u1", "first").await;
            let (_, seen) = live.close().await;
            let limit = seen.iter().find_map(|e| match e {
                AgentEvent::ContextChanged { meter_limit, .. } => Some(*meter_limit),
                _ => None,
            });
            assert_eq!(limit, Some(Some(2_000)));
            continue;
        }
        live.turn("u1", "first").await;
        assert!(completed(&live.turn("u2", "second").await));
        let (_, seen) = live.close().await;
        assert!(!seen.iter().any(auto_started), "{seen:#?}");
        let limit = seen.iter().find_map(|e| match e {
            AgentEvent::ContextChanged { meter_limit, .. } => Some(*meter_limit),
            _ => None,
        });
        assert_eq!(limit, Some(Some(850_000)));
    }
}

#[tokio::test]
async fn switching_automatic_compaction_resends_the_meter_before_the_next_request() {
    for (config, on_before, limit_after) in [
        ("", true, 850_000),
        ("", false, 850_000),
        (LOW, true, 850_000),
    ] {
        let store = MemoryStore::default();
        let gate = Arc::new(Semaphore::new(0));
        let provider = scripted(vec![
            answer("Hello.", 5_000).gated(&gate),
            answer("Again.", 300),
        ]);
        let agent = on(&provider, &store, config).with_auto_compact(on_before);
        let mut live = Live::start(agent);
        live.send(send("u1", "first")).await;
        live.until(|e| matches!(e, AgentEvent::StepStarted { .. }))
            .await;
        live.send(ClientCommand::ChangeSettings {
            request: RequestId(1),
            changes: vec![
                SettingChange::named("context.auto_compact", SettingValue::Bool(!on_before))
                    .unwrap(),
            ],
            save: false,
        })
        .await;
        live.until(|e| {
            matches!(
                e,
                AgentEvent::SettingsChanged {
                    answers: Some(RequestId(1)),
                    ..
                }
            )
        })
        .await;
        gate.add_permits(1);
        live.until(|e| matches!(e, AgentEvent::TurnEnded { .. }))
            .await;
        let mark = live.seen.len();
        assert!(completed(&live.turn("u2", "second").await));
        let (_, seen) = live.close().await;
        let first = seen[mark..]
            .iter()
            .find(|e| {
                matches!(
                    e,
                    AgentEvent::ContextChanged { .. } | AgentEvent::StepStarted { .. }
                )
            })
            .unwrap();
        assert!(
            matches!(
                first,
                AgentEvent::ContextChanged { meter_limit, auto_compact, .. }
                    if *meter_limit == Some(limit_after) && *auto_compact == !on_before
            ),
            "{config} {on_before}: {first:?}"
        );
    }
}

#[tokio::test]
async fn the_panels_limit_is_the_meters_with_compaction_on_or_off_and_any_point() {
    for config in ["", LOW] {
        for auto_compact in [true, false] {
            let agent = on(&scripted(Vec::new()), &MemoryStore::default(), config)
                .with_auto_compact(auto_compact);
            let AgentEvent::ContextChanged { meter_limit, .. } = agent.context_figures() else {
                unreachable!()
            };
            let fill = agent.quiet_snapshot(&crate::plugin::KernelHost::new()).fill;
            let content = crate::plugin::convert::slot_content(wisp_plugin_api::slots::Content {
                note: Vec::new(),
                blocks: vec![wisp_plugin_api::slots::Block::Fill(fill)],
            });
            let [wisp_protocol::slot::SlotBlock::Fill { fill }] = content.blocks.as_slice() else {
                panic!("{content:?}")
            };
            assert_eq!(fill.limit, meter_limit, "{config:?} {auto_compact}");
            assert!(meter_limit.is_some());
        }
    }
}

#[tokio::test]
async fn a_steer_at_a_boundary_over_the_point_compacts_and_follows_the_note() {
    for (config, summary) in [
        (LOW, compaction(SUMMARY)),
        (LOW_WISP, answer(WRITTEN, 60_000)),
    ] {
        let store = MemoryStore::default();
        let gate = Arc::new(Semaphore::new(0));
        let provider = scripted(vec![
            answer("Hello.", 5_000).gated(&gate),
            summary,
            answer("Two.", 300),
        ]);
        let mut live = Live::start(on(&provider, &store, config));
        let outcome = steered_turn(&mut live, &gate, "u1", "first").await;
        assert!(completed(&outcome), "{outcome:?}");
        let (agent, seen) = live.close().await;
        assert!(seen.iter().any(|e| matches!(
            e,
            AgentEvent::Compacted {
                trigger: CompactionTrigger::Auto,
                turn: Some(1),
                ..
            }
        )));
        let sent = provider.sent();
        assert_eq!(sent.len(), 3);
        assert!(!body_text(&sent[1].body()).contains("and this"));
        let after = body_text(&sent[2].body());
        let note = after.find(CONTINUE_NOTE).unwrap();
        let steer = after.find("and this").unwrap();
        assert!(note < steer, "{after}");
        assert!(!after.contains("Hello."), "{after}");
        let logged = after_compaction(&store);
        assert_eq!(logged.len(), 3, "{logged:?}");
        assert!(
            logged[0].starts_with("User ") && logged[0].contains(CONTINUE_NOTE),
            "{logged:?}"
        );
        assert!(!logged[0].contains("and this"), "{logged:?}");
        assert_eq!(logged[1], "input and this");
        assert!(logged[2].starts_with("Assistant "), "{logged:?}");
        assert_rebuilds(&store, &agent);
        assert_prefix_kept(&provider);
        let ended = seen
            .iter()
            .filter(|e| matches!(e, AgentEvent::TurnEnded { .. }));
        assert_eq!(ended.count(), 1);
    }
}

async fn accepted_while_compacting(live: &mut Live, gate: &Arc<Semaphore>, id: &str) {
    live.until(
        |e| matches!(e, AgentEvent::InputAccepted { id: input, .. } if input.as_str() == id),
    )
    .await;
    gate.add_permits(1);
    let ended = live
        .until(|e| matches!(e, AgentEvent::TurnEnded { .. }))
        .await;
    assert!(
        matches!(&ended, AgentEvent::TurnEnded { outcome, .. } if completed(outcome)),
        "{ended:?}"
    );
}

fn gated_compaction(first: &Arc<Semaphore>, summary: &Arc<Semaphore>) -> Arc<Fake> {
    scripted(vec![
        answer("Hello.", 5_000).gated(first),
        compaction(SUMMARY).gated(summary),
        answer("Two.", 300),
        answer("Three.", 300),
    ])
}

fn sent_after(provider: &Fake, late: &str) -> Vec<bool> {
    let sent = provider.sent();
    sent.iter()
        .map(|s| body_text(&s.body()).contains(late))
        .collect()
}

#[tokio::test]
async fn a_steer_typed_while_an_in_turn_compaction_runs_waits_for_the_next_boundary() {
    let store = MemoryStore::default();
    let (first, summary) = (Arc::new(Semaphore::new(0)), Arc::new(Semaphore::new(0)));
    let provider = gated_compaction(&first, &summary);
    let mut live = Live::start(on(&provider, &store, LOW));
    steer_first_step(&mut live, &first, "u1", "first").await;
    live.until(auto_started).await;
    live.send(send("late", "typed during")).await;
    accepted_while_compacting(&mut live, &summary, "late").await;
    let (agent, _) = live.close().await;
    assert_eq!(
        sent_after(&provider, "typed during"),
        [false, false, false, true]
    );
    assert!(body_text(&provider.body(2)).contains("and this"));
    assert_rebuilds(&store, &agent);
    assert_prefix_kept(&provider);
}

#[tokio::test]
async fn a_childs_mail_that_arrives_while_an_in_turn_compaction_runs_opens_the_next_turn() {
    let store = MemoryStore::default();
    let (first, summary) = (Arc::new(Semaphore::new(0)), Arc::new(Semaphore::new(0)));
    let provider = gated_compaction(&first, &summary);
    let agent = on(&provider, &store, LOW);
    let mail = agent.mail_sender();
    let mut live = Live::start(agent);
    steer_first_step(&mut live, &first, "u1", "first").await;
    live.until(auto_started).await;
    mail.send(crate::agent::Mail {
        id: wisp_protocol::MailId("m1".into()),
        from: wisp_protocol::AgentPath::new("/child"),
        to: wisp_protocol::AgentPath::new("/"),
        kind: wisp_protocol::MailKind::Message,
        wake: false,
        text: "a note from the child".into(),
        checklist_spans: Vec::new(),
    })
    .unwrap();
    accepted_while_compacting(&mut live, &summary, "mail-m1").await;
    assert!(completed(&live.turn("u2", "second").await));
    let (agent, _) = live.close().await;
    assert_eq!(
        sent_after(&provider, "a note from the child"),
        [false, false, false, true]
    );
    assert_rebuilds(&store, &agent);
    assert_prefix_kept(&provider);
}

#[tokio::test]
async fn a_compaction_that_leaves_the_conversation_over_the_point_pauses_then_holds() {
    let store = MemoryStore::default();
    let first = Arc::new(Semaphore::new(0));
    let second = Arc::new(Semaphore::new(0));
    let provider = scripted(vec![
        answer("Hello.", 5_000).gated(&first),
        compaction(SUMMARY),
        answer("Two.", 300).gated(&second),
        answer("Three.", 300),
        answer("Four.", 300),
    ]);
    let mut live = Live::start(on(&provider, &store, TINY));
    live.send(send("u1", "first")).await;
    live.until(|e| matches!(e, AgentEvent::StepStarted { step: 1 }))
        .await;
    live.send(send("s1", "one more")).await;
    live.until(|e| matches!(e, AgentEvent::InputAccepted { .. }))
        .await;
    first.add_permits(1);
    live.until(|e| matches!(e, AgentEvent::StepStarted { step: 2 }))
        .await;
    live.send(send("s2", "and another")).await;
    live.until(|e| matches!(e, AgentEvent::InputAccepted { .. }))
        .await;
    second.add_permits(1);
    live.until(|e| matches!(e, AgentEvent::TurnEnded { .. }))
        .await;
    let starts_in_first = live.seen.iter().filter(|e| auto_started(e)).count();
    assert_eq!(starts_in_first, 1);
    assert!(live.seen.iter().any(|e| matches!(
        e,
        AgentEvent::Compacted {
            trigger: CompactionTrigger::Auto,
            paused: true,
            ..
        }
    )));
    assert!(completed(&live.turn("u2", "second").await));
    let (_, seen) = live.close().await;
    assert_eq!(seen.iter().filter(|e| auto_started(e)).count(), 1);
    let held = seen
        .iter()
        .filter(|e| matches!(e, AgentEvent::AutoCompactionHeld { point: 10, .. }))
        .count();
    assert_eq!(held, 1);
    assert_eq!(provider.sent().len(), 5);
    assert_eq!(order(&store, 2), ["started", "ended"]);
}

#[tokio::test]
async fn a_failed_compaction_pauses_and_the_step_goes_ahead() {
    let store = MemoryStore::default();
    let provider = scripted(vec![
        answer("Hello.", 5_000),
        server_failed(),
        answer("Again.", 300),
    ]);
    let mut live = Live::start(no_retries(on(&provider, &store, LOW)));
    live.turn("u1", "first").await;
    assert!(completed(&live.turn("u2", "second").await));
    let (agent, seen) = live.close().await;
    assert!(seen.iter().any(|e| matches!(
        e,
        AgentEvent::CompactionFailed {
            trigger: CompactionTrigger::Auto,
            kind: CompactionFailure::Error(ErrorKind::Server),
            turn: Some(2),
            paused: true,
            ..
        }
    )));
    assert_eq!(order(&store, 2), ["failed", "started", "ended"]);
    assert!(agent.compaction.head.is_none());
    assert!(body_text(&provider.body(2)).contains("Hello."));
}

#[tokio::test]
async fn a_failed_compaction_over_the_window_ends_the_turn_with_its_own_kind() {
    let store = MemoryStore::default();
    let provider = scripted(vec![answer("Hello.", 100_000), server_failed()]);
    let mut live = Live::start(no_retries(on(&provider, &store, SMALL_WINDOW)));
    live.turn("u1", "first").await;
    let outcome = live.turn("u2", "second").await;
    assert!(
        live.seen
            .iter()
            .any(|e| matches!(e, AgentEvent::CompactionFailed { paused: false, .. }))
    );
    assert!(
        matches!(
            &outcome,
            TurnOutcome::Error {
                kind: ErrorKind::Server,
                committed_steps: 0,
                ..
            }
        ),
        "{outcome:?}"
    );
    if let TurnOutcome::Error { message, .. } = &outcome {
        assert_ne!(message, TOO_LARGE);
    }
    live.close().await;
    assert_eq!(provider.sent().len(), 2);
    assert_eq!(order(&store, 2), ["failed", "started", "ended"]);
}

#[tokio::test]
async fn a_message_too_large_after_compacting_ends_the_turn_as_an_overflow() {
    let store = MemoryStore::default();
    let provider = scripted(vec![answer("Hello.", 1_000), compaction(SUMMARY)]);
    let mut live = Live::start(on(&provider, &store, SMALL_WINDOW));
    live.turn("u1", "first").await;
    let outcome = live.turn("u2", &"x".repeat(300_000)).await;
    assert!(
        live.seen
            .iter()
            .any(|e| matches!(e, AgentEvent::Compacted { paused: false, .. }))
    );
    assert!(
        matches!(
            &outcome,
            TurnOutcome::Error {
                kind: ErrorKind::ContextOverflow,
                message,
                ..
            } if message == TOO_LARGE
        ),
        "{outcome:?}"
    );
    live.close().await;
    assert_eq!(provider.sent().len(), 2);
    assert_eq!(order(&store, 2), ["compacted", "started", "ended"]);
}

#[tokio::test]
async fn cancelling_a_compaction_before_a_turn_ends_that_turn_cancelled() {
    let store = MemoryStore::default();
    let provider = scripted(vec![answer("Hello.", 5_000), Step::hang()]);
    let mut live = Live::start(on(&provider, &store, LOW));
    live.turn("u1", "first").await;
    live.send(send("u2", "second")).await;
    live.until(auto_started).await;
    live.send(ClientCommand::Cancel).await;
    let ended = live
        .until(|e| matches!(e, AgentEvent::TurnEnded { .. }))
        .await;
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
    let (_, seen) = live.close().await;
    let ends = seen
        .iter()
        .filter(|e| matches!(e, AgentEvent::TurnEnded { .. }))
        .count();
    assert_eq!(ends, 2);
    assert!(seen.iter().any(|e| matches!(
        e,
        AgentEvent::CompactionFailed {
            kind: CompactionFailure::Cancelled,
            turn: Some(2),
            ..
        }
    )));
    assert_eq!(provider.sent().len(), 2);
    assert_eq!(order(&store, 2), ["failed", "started", "ended"]);
}

#[tokio::test]
async fn cancelling_a_compaction_mid_turn_ends_the_turn_with_every_step_kept() {
    let store = MemoryStore::default();
    let gate = Arc::new(Semaphore::new(0));
    let provider = scripted(vec![answer("Hello.", 5_000).gated(&gate), Step::hang()]);
    let mut live = Live::start(on(&provider, &store, LOW));
    live.send(send("u1", "first")).await;
    live.until(|e| matches!(e, AgentEvent::StepStarted { .. }))
        .await;
    live.send(send("s1", "and this")).await;
    live.until(|e| matches!(e, AgentEvent::InputAccepted { .. }))
        .await;
    gate.add_permits(1);
    live.until(auto_started).await;
    live.send(ClientCommand::Cancel).await;
    let ended = live
        .until(|e| matches!(e, AgentEvent::TurnEnded { .. }))
        .await;
    assert!(
        matches!(
            ended,
            AgentEvent::TurnEnded {
                outcome: TurnOutcome::Cancelled {
                    committed_steps: 1,
                    ..
                },
                ..
            }
        ),
        "{ended:?}"
    );
    let (agent, _) = live.close().await;
    assert!(agent.compaction.head.is_none());
    assert_eq!(agent.history.len(), 2);
    assert_eq!(provider.sent().len(), 2);
}

#[tokio::test]
async fn a_lowered_limit_compacts_before_the_next_turn_once() {
    let store = MemoryStore::default();
    let provider = scripted(vec![
        answer("Hello.", 50_000),
        compaction(SUMMARY),
        answer("Again.", 300),
        answer("Third.", 300),
    ]);
    let mut live = Live::start(logged(provider.clone(), &store));
    live.turn("u1", "first").await;
    live.send(ClientCommand::ChangeSettings {
        request: RequestId(1),
        changes: vec![
            SettingChange::named("context_window", SettingValue::Tokens(104_000)).unwrap(),
        ],
        save: false,
    })
    .await;
    live.until(|e| {
        matches!(
            e,
            AgentEvent::SettingsChanged {
                answers: Some(RequestId(1)),
                ..
            }
        )
    })
    .await;
    assert!(completed(&live.turn("u2", "second").await));
    assert!(completed(&live.turn("u3", "third").await));
    let (_, seen) = live.close().await;
    assert_eq!(seen.iter().filter(|e| auto_started(e)).count(), 1);
    assert_eq!(order(&store, 2), ["compacted", "started", "ended"]);
}

#[tokio::test]
async fn the_reserve_before_a_switch_uses_the_new_models_output_limit() {
    let config = "[models.\"claude-opus-5-5\"]\ncontext_window = 400000\n\n[models.\"claude-narrow-test\"]\ncontext_window = 400000\n";
    let session = || SessionConfig {
        max_tokens: 200_000,
        ..default_session()
    };
    for switch in [true, false] {
        let store = MemoryStore::default();
        let mut steps = vec![answer("Hello.", 300_000)];
        if !switch {
            steps.push(compaction(SUMMARY));
        }
        steps.push(answer("Again.", 300));
        let provider = scripted(steps);
        let agent = Agent::new(provider.clone(), session())
            .with_log(
                SessionLog::new(Box::new(store.clone()), "s1"),
                log_context(),
            )
            .with_model_settings(models(config));
        let mut live = Live::start(agent);
        live.turn("u1", "first").await;
        if switch {
            live.send(ClientCommand::shortcut(
                RequestId(1),
                None,
                Some(NARROW_MODEL.into()),
                None,
            ))
            .await;
            live.until(|e| {
                matches!(
                    e,
                    AgentEvent::SettingsChanged {
                        answers: Some(RequestId(1)),
                        ..
                    }
                )
            })
            .await;
        }
        assert!(completed(&live.turn("u2", "second").await));
        let (_, seen) = live.close().await;
        assert_eq!(seen.iter().any(auto_started), !switch, "switch {switch}");
    }
}

#[tokio::test]
async fn a_compact_during_an_automatic_compaction_is_refused_as_turn_running() {
    let provider = scripted(vec![answer("Hello.", 5_000), Step::hang()]);
    let store = MemoryStore::default();
    let mut live = Live::start(on(&provider, &store, LOW));
    live.turn("u1", "first").await;
    live.send(send("u2", "second")).await;
    live.until(auto_started).await;
    let refused = live.compact(7, None).await;
    assert!(
        matches!(
            refused,
            AgentEvent::Rejected {
                kind: Some(RejectKind::TurnRunning),
                ..
            }
        ),
        "{refused:?}"
    );
    live.send(ClientCommand::Cancel).await;
    live.until(|e| matches!(e, AgentEvent::TurnEnded { .. }))
        .await;
    live.close().await;
}

fn crashed_after(store: &MemoryStore, pick: impl Fn(&LogEvent) -> bool) -> LoggedSession {
    let mut records = records(store);
    let at = records.iter().position(|r| pick(&r.event)).unwrap();
    records.truncate(at + 1);
    LoggedSession::read(&records).unwrap()
}

fn compacted_cost(seen: &[AgentEvent]) -> wisp_protocol::Cost {
    seen.iter()
        .find_map(|e| match e {
            AgentEvent::Compacted { cost: Some(c), .. } => Some(*c),
            _ => None,
        })
        .unwrap()
}

fn retried_usd(seen: &[AgentEvent]) -> f64 {
    seen.iter()
        .filter_map(|e| match e {
            AgentEvent::Retrying { cost_usd, .. } => *cost_usd,
            _ => None,
        })
        .sum()
}

fn compaction_retries(store: &MemoryStore, turn: u64) -> usize {
    log_events(store)
        .iter()
        .filter(|e| {
            matches!(
                e,
                LogEvent::Retrying {
                    turn: Some(t),
                    step: None,
                    ..
                } if *t == turn
            )
        })
        .count()
}

fn live_retries(seen: &[AgentEvent], index: usize) -> u32 {
    seen.iter()
        .filter_map(|e| match e {
            AgentEvent::TurnEnded { stats, .. } => Some(stats.retries),
            _ => None,
        })
        .nth(index)
        .unwrap()
}

fn retried_compaction() -> Step {
    super::usage::cut_after_start(
        vec![super::usage::iteration(100, 9_000, 50)],
        mid_stream(wisp_protocol::RetryClass::Transient),
    )
}

#[tokio::test]
async fn a_crash_after_a_retried_compaction_before_a_turn_counts_the_retry_once() {
    let store = MemoryStore::default();
    let provider = scripted(vec![
        answer("Hello.", 5_000),
        retried_compaction(),
        compaction(SUMMARY),
        answer("Again.", 300),
    ]);
    let mut live = Live::start(on(&provider, &store, LOW).with_retry_policy(quick()));
    let first = live.turn("u1", "first").await;
    assert!(completed(&live.turn("u2", "second").await));
    let (_, seen) = live.close().await;
    let TurnOutcome::Completed {
        cost: Some(first), ..
    } = first
    else {
        panic!("{first:?}");
    };
    assert_eq!(compaction_retries(&store, 2), 1);
    let retried = retried_usd(&seen);
    assert!(retried > 0.0);
    let compacted = compacted_cost(&seen);

    let cut = crashed_after(&store, |e| matches!(e, LogEvent::Retrying { .. }));
    assert!(
        (cut.session_usd - (first.session_usd + retried)).abs() < 1e-12,
        "a compaction cut short still cost its retry"
    );

    let open = crashed_after(&store, |e| {
        matches!(e, LogEvent::TurnStarted { turn: 2, .. })
    });
    assert!((open.session_usd - compacted.session_usd).abs() < 1e-12);
    let last = open.repaired_turn().unwrap();
    assert!((last.usd.unwrap() - compacted.turn_usd).abs() < 1e-12);
    assert_eq!(
        live_retries(&seen, 1),
        1,
        "the live turn counts the compaction's retry"
    );
    assert_eq!(last.retries, live_retries(&seen, 1));
    assert!(last.lower_bound);
}

#[tokio::test]
async fn a_crash_after_a_retried_compaction_mid_turn_counts_the_retry_once() {
    let store = MemoryStore::default();
    let gate = Arc::new(Semaphore::new(0));
    let provider = scripted(vec![
        answer("Hello.", 5_000).gated(&gate),
        retried_compaction(),
        compaction(SUMMARY),
        answer("Two.", 300),
    ]);
    let mut live = Live::start(on(&provider, &store, LOW).with_retry_policy(quick()));
    let outcome = steered_turn(&mut live, &gate, "u1", "first").await;
    assert!(completed(&outcome), "{outcome:?}");
    let (_, seen) = live.close().await;
    assert_eq!(compaction_retries(&store, 1), 1);
    let compacted = compacted_cost(&seen);
    let step_usd = log_events(&store)
        .iter()
        .find_map(|e| match e {
            LogEvent::StepEnded {
                step: 1, cost_usd, ..
            } => *cost_usd,
            _ => None,
        })
        .unwrap();

    let open = crashed_after(&store, |e| matches!(e, LogEvent::ContextCompacted { .. }));
    assert!(
        (open.session_usd - compacted.session_usd).abs() < 1e-12,
        "{} against {}",
        open.session_usd,
        compacted.session_usd
    );
    let last = open.repaired_turn().unwrap();
    assert!((last.usd.unwrap() - (step_usd + compacted.turn_usd)).abs() < 1e-12);
    assert_eq!(
        live_retries(&seen, 0),
        1,
        "the live turn counts the compaction's retry"
    );
    assert_eq!(
        (last.retries, last.committed_steps),
        (live_retries(&seen, 0), 1)
    );
    assert!(last.lower_bound);
}

#[tokio::test]
async fn busy_retries_inside_an_in_turn_compaction_leave_the_turn_its_counted_retries() {
    let detail = |kind| {
        failing(classified(
            kind,
            "refused",
            FailurePhase::Response,
            RetryClass::Transient,
        ))
    };
    let store = MemoryStore::default();
    let gate = Arc::new(Semaphore::new(0));
    let mut script = vec![answer("Hello.", 5_000).gated(&gate)];
    script.extend((0..8).map(|_| detail(ErrorKind::RateLimit)));
    script.push(compaction(SUMMARY));
    script.extend((0..3).map(|_| detail(ErrorKind::Server)));
    script.push(answer("Two.", 300));
    let provider = scripted(script);
    let policy = RetryPolicy {
        busy_window: RetryPolicy::default().for_fallback().busy_window,
        ..quick()
    };
    let mut live = Live::start(on(&provider, &store, LOW).with_retry_policy(policy));
    let outcome = steered_turn(&mut live, &gate, "u1", "first").await;
    assert!(completed(&outcome), "{outcome:?}");
    let (_, seen) = live.close().await;
    assert_eq!(compaction_retries(&store, 1), 8);
    assert_eq!(live_retries(&seen, 0), 11);
}

#[tokio::test]
async fn a_rewrite_past_the_point_compacts_before_the_turn() {
    use wisp_plugin_api::hooks::{Event, Reply};
    let store = MemoryStore::default();
    let provider = scripted(vec![
        answer("Hello.", 300),
        compaction(SUMMARY),
        answer("Again.", 300),
    ]);
    let script: wisp_plugin_testkit::hooks::Script = Arc::new(|_, _, event| {
        let long = matches!(&event, Event::UserInput(i) if i.text == "long");
        Box::pin(async move {
            if long {
                Reply::Rewrite("word ".repeat(1_900))
            } else {
                Reply::None
            }
        })
    });
    let calls = wisp_plugin_testkit::hooks::Calls::default();
    let sources = vec![crate::plugin::testing::hook_source(
        "aa",
        &crate::plugin::testing::hook_body("u", "user-input", ""),
        script,
        &calls,
    )];
    let agent = crate::plugin::testing::with_test_hooks(on(&provider, &store, LOW), sources);
    let mut live = Live::start(agent);
    assert!(completed(&live.turn("u1", "first").await));
    assert!(completed(&live.turn("u2", "long").await));
    live.close().await;
    assert_eq!(order(&store, 2), ["compacted", "started", "ended"]);
    assert_eq!(calls.count("u"), 2);
}
