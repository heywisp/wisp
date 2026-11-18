use wisp_protocol::BlockKind;

use super::*;
use crate::events::{End, Ending, turn_ended};
use crate::pick::pick;

mod compaction;
mod pre_turn;
mod processes;
mod retry;

fn answer(chunks: usize) -> Vec<AgentEvent> {
    let mut out = vec![
        AgentEvent::turn_started(1),
        AgentEvent::BlockStarted {
            index: 0,
            kind: BlockKind::Text,
        },
    ];
    for i in 0..chunks {
        out.push(AgentEvent::TextDelta {
            index: 0,
            text: format!("word {i} "),
        });
    }
    out.push(AgentEvent::BlockStopped { index: 0 });
    out.push(turn_ended(End {
        ending: Ending::Cancelled,
        ..End::default()
    }));
    out
}

#[test]
fn lines_parse_into_events_and_bad_lines_are_named() {
    let text =
        "{\"type\":\"turn_started\",\"turn\":1}\n\n{\"type\":\"block_stopped\",\"index\":0}\n";
    assert_eq!(
        parse(text).unwrap(),
        [
            AgentEvent::turn_started(1),
            AgentEvent::BlockStopped { index: 0 }
        ]
    );
    let err = parse("{\"type\":\"turn_started\",\"turn\":1}\nnope\n").unwrap_err();
    assert_eq!(err.line, 2);
}

#[test]
fn a_script_splits_into_turns() {
    let mut events = vec![AgentEvent::LogDisabled {
        reason: "x".to_owned(),
    }];
    events.extend(answer(2));
    events.extend(answer(1));
    let script = Script::new(events);
    assert_eq!(script.preamble.len(), 1);
    assert_eq!(script.turns.len(), 2);
}

#[tokio::test(start_paused = true)]
async fn a_message_plays_the_next_turn_and_cancel_ends_it() {
    let (commands, mut events) = spawn(Script::new(answer(1000)), PACE);
    commands
        .send(ClientCommand::send("t-1", "go"))
        .await
        .unwrap();
    assert_eq!(events.recv().await, Some(AgentEvent::turn_started(1)));
    events.recv().await.unwrap();
    commands.send(ClientCommand::Cancel).await.unwrap();
    let mut last = None;
    while let Ok(Some(event)) = tokio::time::timeout(Duration::from_secs(1), events.recv()).await {
        let end = matches!(event, AgentEvent::TurnEnded { .. });
        last = Some(event);
        if end {
            break;
        }
    }
    assert_eq!(last, Some(cancelled()));
    commands
        .send(ClientCommand::send("t-2", "again"))
        .await
        .unwrap();
    assert_eq!(
        events.recv().await,
        Some(AgentEvent::Rejected {
            request: None,
            reason: NO_MORE_TURNS.to_owned(),
            id: Some(InputId("t-2".to_owned())),
            kind: None,
        })
    );
    drop(commands);
    assert_eq!(events.recv().await, None);
}

fn effect(event: &AgentEvent) -> (CacheEffect, Vec<String>) {
    match event {
        AgentEvent::SettingsChanged {
            cache,
            account_bound,
            ..
        } => (cache.clone(), account_bound.clone()),
        other => panic!("{other:?}"),
    }
}

#[test]
fn settings_answers_compare_with_the_running_turn_like_the_kernel() {
    let mut s = Settings {
        endpoint: "a".to_owned(),
        model: "m1".to_owned(),
        account_bound: vec!["m1".to_owned()],
        ..Settings::default()
    };
    let switch = CacheEffect::Rebuilt {
        reason: RebuildReason::EndpointSwitch,
    };
    let model = CacheEffect::Rebuilt {
        reason: RebuildReason::ModelSwitch,
    };
    assert_eq!(
        effect(&s.answer(Some("b".to_owned()), None, None)),
        (switch.clone(), vec!["m1".to_owned()])
    );
    assert_eq!(
        effect(&s.answer(None, Some("m2".to_owned()), None)),
        (switch.clone(), vec!["m1".to_owned()])
    );
    assert_eq!(
        effect(&s.answer(Some("a".to_owned()), None, None)),
        (model, vec![])
    );
    assert_eq!(
        effect(&s.answer(None, Some("m1".to_owned()), None)),
        (CacheEffect::Kept, vec![])
    );
    assert_eq!(s.pending, None);
    s.answer(Some("b".to_owned()), None, Some(Effort::Low));
    s.start_turn();
    assert_eq!((s.endpoint.as_str(), s.effort), ("b", Effort::Low));
    assert_eq!(s.pending, None);
    assert_eq!(
        effect(&s.answer(None, None, None)),
        (CacheEffect::Kept, vec![])
    );
}

fn stepped(text: &str) -> Vec<AgentEvent> {
    vec![
        AgentEvent::turn_started(1),
        AgentEvent::StepStarted { step: 1 },
        AgentEvent::BlockStarted {
            index: 0,
            kind: BlockKind::ToolUse {
                writing: None,
                name: "run".to_owned(),
            },
        },
        AgentEvent::BlockStopped { index: 0 },
        AgentEvent::StepStarted { step: 2 },
        AgentEvent::BlockStarted {
            index: 0,
            kind: BlockKind::Text,
        },
        AgentEvent::TextDelta {
            index: 0,
            text: text.to_owned(),
        },
        AgentEvent::BlockStopped { index: 0 },
        turn_ended(End {
            model: "replay".to_owned(),
            steps: 2,
            tool_calls: 1,
            ..End::default()
        }),
    ]
}

fn send(id: &str, mode: DeliveryMode, turn: Option<u64>) -> ClientCommand {
    ClientCommand::Send {
        id: InputId(id.to_owned()),
        text: format!("text of {id}"),
        images: Vec::new(),
        mode,
        turn,
        replaces: None,
    }
}

async fn until(
    events: &mut mpsc::Receiver<AgentEvent>,
    done: impl Fn(&AgentEvent) -> bool,
) -> Vec<AgentEvent> {
    let mut seen = vec![];
    while let Ok(Some(event)) = tokio::time::timeout(Duration::from_secs(2), events.recv()).await {
        let stop = done(&event);
        seen.push(event);
        if stop {
            break;
        }
    }
    seen
}

fn ids(names: &[&str]) -> Vec<InputId> {
    names.iter().map(|n| InputId((*n).to_owned())).collect()
}

#[tokio::test(start_paused = true)]
async fn a_steer_is_delivered_after_the_next_step_starts_and_a_queued_message_starts_the_next_turn()
{
    let mut events_in = stepped("one");
    events_in.extend(stepped("two"));
    let (commands, mut events) = spawn(Script::new(events_in), PACE);
    commands
        .send(send("t-1", DeliveryMode::Steer, None))
        .await
        .unwrap();
    let opened = until(&mut events, |e| {
        matches!(e, AgentEvent::InputDelivered { .. })
    })
    .await;
    assert_eq!(
        opened[2],
        AgentEvent::InputDelivered {
            ids: ids(&["t-1"]),
            step: 1
        }
    );
    commands
        .send(send("t-2", DeliveryMode::Steer, Some(1)))
        .await
        .unwrap();
    commands
        .send(send("t-3", DeliveryMode::Queue, Some(1)))
        .await
        .unwrap();
    let first = until(&mut events, |e| matches!(e, AgentEvent::TurnEnded { .. })).await;
    let accepted: Vec<&AgentEvent> = first
        .iter()
        .filter(|e| matches!(e, AgentEvent::InputAccepted { .. }))
        .collect();
    assert_eq!(accepted.len(), 2);
    let step_two = first
        .iter()
        .position(|e| *e == AgentEvent::StepStarted { step: 2 })
        .unwrap();
    assert_eq!(
        first[step_two + 1],
        AgentEvent::InputDelivered {
            ids: ids(&["t-2"]),
            step: 2
        }
    );
    let second = until(&mut events, |e| {
        matches!(e, AgentEvent::InputDelivered { .. })
    })
    .await;
    assert_eq!(second[0], AgentEvent::turn_started(2));
    assert_eq!(
        second.last(),
        Some(&AgentEvent::InputDelivered {
            ids: ids(&["t-3"]),
            step: 1
        })
    );
}

#[tokio::test(start_paused = true)]
async fn a_stop_returns_waiting_steers_holds_queued_messages_and_release_sends_them() {
    let mut events_in = stepped("one");
    events_in.extend(stepped("two"));
    let (commands, mut events) = spawn(Script::new(events_in), PACE);
    commands
        .send(send("t-1", DeliveryMode::Steer, None))
        .await
        .unwrap();
    until(&mut events, |e| {
        matches!(e, AgentEvent::InputDelivered { .. })
    })
    .await;
    commands
        .send(send("t-2", DeliveryMode::Steer, Some(1)))
        .await
        .unwrap();
    commands
        .send(send("t-3", DeliveryMode::Queue, Some(1)))
        .await
        .unwrap();
    commands.send(ClientCommand::Cancel).await.unwrap();
    let stopped = until(&mut events, |e| matches!(e, AgentEvent::TurnEnded { .. })).await;
    let tail: Vec<&AgentEvent> = stopped.iter().rev().take(3).rev().collect();
    assert_eq!(
        tail,
        [
            &AgentEvent::InputWithdrawn {
                ids: ids(&["t-1", "t-2"]),
                by: WithdrawnBy::TurnStopped
            },
            &AgentEvent::InputHeld {
                ids: ids(&["t-3"]),
                by: WithdrawnBy::TurnStopped
            },
            &cancelled(),
        ]
    );
    commands.send(ClientCommand::Release).await.unwrap();
    let released = until(&mut events, |e| {
        matches!(e, AgentEvent::InputDelivered { .. })
    })
    .await;
    assert_eq!(
        released.last(),
        Some(&AgentEvent::InputDelivered {
            ids: ids(&["t-3"]),
            step: 1
        })
    );
}

fn step_ended(step: u32) -> AgentEvent {
    AgentEvent::StepEnded {
        step,
        usage: None,
        cost_usd: None,
        lower_bound: false,
    }
}

fn long_second_step() -> Vec<AgentEvent> {
    let mut out = vec![
        AgentEvent::turn_started(1),
        AgentEvent::StepStarted { step: 1 },
        AgentEvent::BlockStarted {
            index: 0,
            kind: BlockKind::Text,
        },
        AgentEvent::TextDelta {
            index: 0,
            text: "first step".to_owned(),
        },
        AgentEvent::BlockStopped { index: 0 },
        step_ended(1),
        AgentEvent::StepStarted { step: 2 },
        AgentEvent::BlockStarted {
            index: 0,
            kind: BlockKind::Text,
        },
    ];
    for i in 0..1000 {
        out.push(AgentEvent::TextDelta {
            index: 0,
            text: format!("word {i} "),
        });
    }
    out.extend(stepped("never").into_iter().skip(7));
    out
}

fn accepted_ids(events: &[AgentEvent]) -> Vec<InputId> {
    pick!(events, AgentEvent::InputAccepted { id, .. } => id.clone())
}

#[tokio::test(start_paused = true)]
async fn a_send_naming_a_stopped_turn_is_returned_or_held_and_starts_no_turn() {
    let mut events_in = answer(1000);
    events_in.extend(stepped("two"));
    let (commands, mut events) = spawn(Script::new(events_in), PACE);
    commands
        .send(send("t-1", DeliveryMode::Steer, None))
        .await
        .unwrap();
    until(&mut events, |e| matches!(e, AgentEvent::TextDelta { .. })).await;
    commands.send(ClientCommand::Cancel).await.unwrap();
    until(&mut events, |e| matches!(e, AgentEvent::TurnEnded { .. })).await;
    commands
        .send(send("t-2", DeliveryMode::Steer, Some(1)))
        .await
        .unwrap();
    commands
        .send(send("t-3", DeliveryMode::Queue, Some(1)))
        .await
        .unwrap();
    let stale = until(&mut events, |e| matches!(e, AgentEvent::InputHeld { .. })).await;
    assert_eq!(accepted_ids(&stale), ids(&["t-2", "t-3"]));
    assert_eq!(
        stale[1],
        AgentEvent::InputWithdrawn {
            ids: ids(&["t-2"]),
            by: WithdrawnBy::TurnStopped
        }
    );
    assert_eq!(
        stale.last(),
        Some(&AgentEvent::InputHeld {
            ids: ids(&["t-3"]),
            by: WithdrawnBy::TurnStopped
        })
    );
    assert!(
        !stale
            .iter()
            .any(|e| matches!(e, AgentEvent::TurnStarted { .. }))
    );
    commands
        .send(send("t-4", DeliveryMode::Steer, Some(1)))
        .await
        .unwrap();
    commands
        .send(send("t-5", DeliveryMode::Steer, None))
        .await
        .unwrap();
    let second = until(&mut events, |e| {
        matches!(e, AgentEvent::InputDelivered { .. })
    })
    .await;
    assert_eq!(
        second[0],
        AgentEvent::InputAccepted {
            opening: false,
            front: false,
            id: InputId("t-4".to_owned()),
            mode: DeliveryMode::Steer,
            text: "text of t-4".to_owned(),
            images: Vec::new(),
            replaces: None,
            held: false,
            from: None,
        }
    );
    assert_eq!(
        second[1],
        AgentEvent::InputWithdrawn {
            ids: ids(&["t-4"]),
            by: WithdrawnBy::TurnStopped
        }
    );
    assert_eq!(second[2], AgentEvent::turn_started(2));
    assert_eq!(
        second.last(),
        Some(&AgentEvent::InputDelivered {
            ids: ids(&["t-3", "t-5"]),
            step: 1
        })
    );
    commands
        .send(send("t-6", DeliveryMode::Steer, Some(1)))
        .await
        .unwrap();
    let late = until(&mut events, |e| {
        matches!(e, AgentEvent::InputWithdrawn { .. })
    })
    .await;
    assert_eq!(
        late.last(),
        Some(&AgentEvent::InputWithdrawn {
            ids: ids(&["t-6"]),
            by: WithdrawnBy::TurnStopped
        })
    );
}

#[tokio::test(start_paused = true)]
async fn a_cancel_marks_not_kept_only_what_was_delivered_after_the_last_committed_step() {
    let (commands, mut events) = spawn(Script::new(long_second_step()), PACE);
    commands
        .send(send("t-1", DeliveryMode::Steer, None))
        .await
        .unwrap();
    until(&mut events, |e| {
        matches!(e, AgentEvent::InputDelivered { .. })
    })
    .await;
    commands
        .send(send("t-2", DeliveryMode::Steer, Some(1)))
        .await
        .unwrap();
    let step_two = until(&mut events, |e| {
        matches!(e, AgentEvent::InputDelivered { step: 2, .. })
    })
    .await;
    assert!(step_two.contains(&step_ended(1)));
    commands.send(ClientCommand::Cancel).await.unwrap();
    let stopped = until(&mut events, |e| matches!(e, AgentEvent::TurnEnded { .. })).await;
    let tail: Vec<&AgentEvent> = stopped.iter().rev().take(2).rev().collect();
    assert_eq!(
        tail,
        [
            &AgentEvent::InputWithdrawn {
                ids: ids(&["t-2"]),
                by: WithdrawnBy::TurnStopped
            },
            &cancelled_after(1),
        ]
    );
}

#[tokio::test(start_paused = true)]
async fn when_the_turns_run_out_queued_and_released_messages_fail_their_turn() {
    let (commands, mut events) = spawn(Script::new(stepped("only")), PACE);
    commands
        .send(send("t-1", DeliveryMode::Steer, None))
        .await
        .unwrap();
    until(&mut events, |e| {
        matches!(e, AgentEvent::InputDelivered { .. })
    })
    .await;
    commands
        .send(send("t-2", DeliveryMode::Queue, Some(1)))
        .await
        .unwrap();
    until(&mut events, |e| matches!(e, AgentEvent::TurnEnded { .. })).await;
    let failed = until(&mut events, |e| matches!(e, AgentEvent::TurnEnded { .. })).await;
    assert_eq!(
        failed,
        [
            AgentEvent::turn_started(2),
            AgentEvent::StepStarted { step: 1 },
            AgentEvent::InputDelivered {
                ids: ids(&["t-2"]),
                step: 1
            },
            AgentEvent::InputWithdrawn {
                ids: ids(&["t-2"]),
                by: WithdrawnBy::TurnFailed
            },
            out_of_turns(),
        ]
    );
    commands
        .send(send("t-3", DeliveryMode::Queue, Some(2)))
        .await
        .unwrap();
    let held = until(&mut events, |e| matches!(e, AgentEvent::InputHeld { .. })).await;
    assert_eq!(
        held.last(),
        Some(&AgentEvent::InputHeld {
            ids: ids(&["t-3"]),
            by: WithdrawnBy::TurnFailed
        })
    );
    commands
        .send(send("t-4", DeliveryMode::Steer, None))
        .await
        .unwrap();
    assert_eq!(
        events.recv().await,
        Some(AgentEvent::Rejected {
            request: None,
            reason: NO_MORE_TURNS.to_owned(),
            id: Some(InputId("t-4".to_owned())),
            kind: None,
        })
    );
    commands.send(ClientCommand::Release).await.unwrap();
    let released = until(&mut events, |e| matches!(e, AgentEvent::TurnEnded { .. })).await;
    assert_eq!(
        released,
        [
            AgentEvent::turn_started(3),
            AgentEvent::StepStarted { step: 1 },
            AgentEvent::InputDelivered {
                ids: ids(&["t-3"]),
                step: 1
            },
            AgentEvent::InputWithdrawn {
                ids: ids(&["t-3"]),
                by: WithdrawnBy::TurnFailed
            },
            out_of_turns(),
        ]
    );
    assert_eq!(accepted_ids(&held), ids(&["t-3"]));
}

#[tokio::test(start_paused = true)]
async fn running_out_of_turns_never_answers_a_pending_settings_change() {
    use crate::app::fixture::facts;
    use crate::app::settings::{Answer, Tracker};
    use wisp_protocol::CacheTtl;
    use wisp_render::chrome::Endpoint;
    use wisp_render::theme::glyphs::GlyphSet;

    let (commands, mut events) = spawn(Script::new(stepped("only")), PACE);
    let mut seen = vec![];
    commands
        .send(send("t-1", DeliveryMode::Steer, None))
        .await
        .unwrap();
    seen.extend(
        until(&mut events, |e| {
            matches!(e, AgentEvent::InputDelivered { .. })
        })
        .await,
    );
    commands
        .send(send("t-2", DeliveryMode::Queue, Some(1)))
        .await
        .unwrap();
    commands.send(ClientCommand::Cancel).await.unwrap();
    seen.extend(until(&mut events, |e| matches!(e, AgentEvent::TurnEnded { .. })).await);
    commands
        .send(send("t-3", DeliveryMode::Steer, None))
        .await
        .unwrap();
    seen.extend(events.recv().await);
    commands.send(ClientCommand::Release).await.unwrap();
    seen.extend(until(&mut events, |e| matches!(e, AgentEvent::TurnEnded { .. })).await);
    assert!(
        !seen.iter().any(|e| matches!(
            e,
            AgentEvent::Rejected {
                id: None,
                kind: None,
                ..
            }
        )),
        "{seen:#?}"
    );
    let f = facts(Endpoint::Official, CacheTtl::OneHour);
    let mut tracker = Tracker::new(&f);
    tracker.asked_settings(wisp_protocol::RequestId(0), None, None, Some(Effort::High));
    let wanted = tracker.wanted();
    for event in &seen {
        let answer = tracker.apply(event, &f, Duration::from_secs(1), GlyphSet::Unicode);
        assert!(!matches!(answer, Answer::Refused(_)), "{event:?}");
    }
    assert_eq!(tracker.wanted(), wanted);
    assert_eq!(tracker.wanted().effort, Some(Effort::High));
}

#[tokio::test(start_paused = true)]
async fn a_context_report_is_answered_idle_and_during_a_turn() {
    use wisp_protocol::RequestId;
    use wisp_protocol::context::{EstimateReason, Fill, ReportBasis, ReportDetail};
    let replay = ReportBasis::Estimated {
        reason: EstimateReason::Other("replay".into()),
    };
    let (commands, mut events) = spawn(Script::new(answer(1000)), PACE);
    commands
        .send(ClientCommand::ContextReport {
            detail: ReportDetail::All,
            request: RequestId(1),
        })
        .await
        .unwrap();
    let seen = until(&mut events, |e| matches!(e, AgentEvent::ContextReport(_))).await;
    let Some(AgentEvent::ContextReport(report)) = seen.last() else {
        panic!("{seen:?}");
    };
    assert_eq!(report.answers, RequestId(1));
    assert_eq!(report.fill, Fill::None);
    assert_eq!(report.window, None);
    assert_eq!(report.basis, replay);
    assert_eq!(report.compact_now_cost_usd, None);
    commands
        .send(ClientCommand::send("t-1", "go"))
        .await
        .unwrap();
    until(&mut events, |e| matches!(e, AgentEvent::TextDelta { .. })).await;
    commands
        .send(ClientCommand::ContextReport {
            detail: ReportDetail::Summary,
            request: RequestId(2),
        })
        .await
        .unwrap();
    let seen = until(&mut events, |e| matches!(e, AgentEvent::ContextReport(_))).await;
    let Some(AgentEvent::ContextReport(report)) = seen.last() else {
        panic!("{seen:?}");
    };
    assert_eq!(report.answers, RequestId(2));
    assert_eq!(report.basis, replay);
    let later: Vec<AgentEvent> = until(&mut events, |e| matches!(e, AgentEvent::TurnEnded { .. }))
        .await
        .into_iter()
        .filter(|e| matches!(e, AgentEvent::ContextReport(_)))
        .collect();
    assert!(later.is_empty(), "{later:?}");
    assert!(
        !seen
            .iter()
            .any(|e| matches!(e, AgentEvent::TurnEnded { .. }))
    );
}

#[tokio::test(start_paused = true)]
async fn slots_shown_gets_nothing_idle_or_during_a_turn() {
    use wisp_protocol::RequestId;
    use wisp_protocol::context::ReportDetail;
    let shown = || ClientCommand::SlotsShown {
        sections: vec!["context/window".into()],
        pages: vec!["context/window".into()],
    };
    let report = |request: u64| ClientCommand::ContextReport {
        detail: ReportDetail::Summary,
        request: RequestId(request),
    };
    let reported = |e: &AgentEvent| matches!(e, AgentEvent::ContextReport(_));
    let (commands, mut events) = spawn(Script::new(answer(1000)), PACE);
    commands.send(shown()).await.unwrap();
    commands.send(report(1)).await.unwrap();
    let seen = until(&mut events, reported).await;
    assert!(
        matches!(seen.as_slice(), [AgentEvent::ContextReport(_)]),
        "{seen:?}"
    );
    commands
        .send(ClientCommand::send("t-1", "go"))
        .await
        .unwrap();
    until(&mut events, |e| matches!(e, AgentEvent::TextDelta { .. })).await;
    commands.send(shown()).await.unwrap();
    commands.send(report(2)).await.unwrap();
    let seen: Vec<AgentEvent> = until(&mut events, reported)
        .await
        .into_iter()
        .filter(|e| !matches!(e, AgentEvent::TextDelta { .. }))
        .collect();
    assert!(
        matches!(seen.as_slice(), [AgentEvent::ContextReport(_)]),
        "{seen:?}"
    );
}

#[tokio::test(start_paused = true)]
async fn replay_refuses_every_endpoint_command_idle_and_during_a_turn() {
    use wisp_protocol::endpoint::EndpointEdit;
    use wisp_protocol::{RequestId, Secret};
    let edit = |request: u64| ClientCommand::EditEndpoint {
        request: RequestId(request),
        name: "manifest".into(),
        edit: EndpointEdit {
            key: Some(Secret::new("sk-ant-CANARYREPLAY0001")),
            ..EndpointEdit::default()
        },
        store: None,
        check: true,
    };
    let remove = |request: u64| ClientCommand::RemoveEndpoint {
        request: RequestId(request),
        name: "manifest".into(),
        keep_key: false,
    };
    let refused =
        |request: u64| AgentEvent::endpoint_refused(Some(RequestId(request)), NO_ENDPOINT_SAVES);
    let (commands, mut events) = spawn(Script::new(answer(1000)), PACE);
    commands.send(edit(1)).await.unwrap();
    commands.send(remove(2)).await.unwrap();
    let seen = until(&mut events, |e| *e == refused(2)).await;
    assert!(seen.contains(&refused(1)), "{seen:?}");
    commands
        .send(ClientCommand::send("t-1", "go"))
        .await
        .unwrap();
    until(&mut events, |e| matches!(e, AgentEvent::TextDelta { .. })).await;
    commands.send(edit(3)).await.unwrap();
    commands.send(remove(4)).await.unwrap();
    let seen = until(&mut events, |e| *e == refused(4)).await;
    assert!(seen.contains(&refused(3)), "{seen:?}");
    assert!(!format!("{seen:?}").contains("CANARY"));
}

fn resolved(name: &str, value: &str, layer: Layer, origin: &str) -> ResolvedSetting {
    ResolvedSetting {
        name: wisp_protocol::settings::SettingName::of(
            wisp_protocol::settings::find(name).unwrap(),
        ),
        value: Some(SettingValue::Text(value.to_owned())),
        layer,
        origin: origin.to_owned(),
        shadowed: Vec::new(),
        choices: Vec::new(),
        applies: true,
    }
}

fn described_with(file: FileState, saved: Vec<SavedValue>) -> Settings {
    Settings {
        endpoint: "manifest".to_owned(),
        model: "m1".to_owned(),
        effort: Effort::Medium,
        described: Described {
            settings: vec![
                resolved("cache_ttl", "5m", Layer::Config, "cache_ttl"),
                resolved("effort", "medium", Layer::Default, "default"),
                resolved("model", "m1", Layer::Flag, "--model"),
            ],
            saved,
            file,
            models: Vec::new(),
            max_tokens: 0,
            endpoint: Some("manifest".to_owned()),
            model: Some("m1".to_owned()),
            plugins: Vec::new(),
        },
        ..Settings::default()
    }
}

fn snapshot_of(event: AgentEvent) -> SettingsSnapshot {
    match event {
        AgentEvent::SettingsSnapshot(snapshot) => snapshot,
        other => panic!("{other:?}"),
    }
}

#[test]
fn a_settings_request_is_answered_with_exactly_the_rows_saved_values_and_file_given() {
    let ttl = wisp_protocol::settings::site_path("cache_ttl").unwrap();
    let saved = vec![SavedValue::of(
        ttl,
        SettingValue::Text("5m".to_owned()),
        Some(1),
    )];
    let file = FileState::Ok {
        path_display: "the given config".to_owned(),
    };
    let s = described_with(file.clone(), saved.clone());
    let snapshot = snapshot_of(s.snapshot(RequestId(7)));
    assert_eq!(snapshot.answers, RequestId(7));
    assert_eq!(snapshot.settings, s.described.settings);
    assert_eq!(snapshot.saved, saved);
    assert_eq!(snapshot.file, file);
    assert!(snapshot.switches.is_empty());
    assert!(snapshot.endpoints.is_empty());
    assert_eq!(snapshot.compaction, None);
    assert_eq!(snapshot.context.endpoint, "manifest");

    assert!(snapshot.plugins.is_empty());

    let mut missing = described_with(FileState::Missing, Vec::new());
    missing.described.plugins = vec![wisp_protocol::PluginEntry {
        prompt_sections: Vec::new(),
        id: "lists".to_owned(),
        version: "0.1.0".to_owned(),
        summary: "Lists".to_owned(),
        source: wisp_protocol::PluginSource::Bundled,
        installed: true,
        enabled: true,
        tools: Vec::new(),
        commands: Vec::new(),
        hooks: Vec::new(),
        slots: Vec::new(),
        settings: Vec::new(),
        problems: Vec::new(),
    }];
    let snapshot = snapshot_of(missing.snapshot(RequestId(8)));
    assert_eq!(snapshot.file, FileState::Missing);
    assert!(snapshot.saved.is_empty());
    assert_eq!(snapshot.plugins, missing.described.plugins);
}

#[test]
fn replay_refuses_a_save_and_answers_a_session_effort_reading_session_after() {
    let mut s = described_with(FileState::Missing, Vec::new());
    let model = wisp_protocol::settings::find("model").unwrap();
    let saving = vec![SettingChange::set(
        model,
        SettingValue::Text("m2".to_owned()),
    )];
    assert_eq!(
        s.command(RequestId(1), &saving, true),
        AgentEvent::refused(RequestId(1), NO_SAVES)
    );
    assert_eq!(s.model, "m1");
    assert_eq!(s.pending, None);

    let effort = wisp_protocol::settings::find("effort").unwrap();
    let lower = vec![SettingChange::set(
        effort,
        SettingValue::Text("low".to_owned()),
    )];
    let AgentEvent::SettingsChanged {
        effort: answered,
        answers,
        ..
    } = s.command(RequestId(2), &lower, false)
    else {
        panic!("no answer");
    };
    assert_eq!((answered, answers), (Effort::Low, Some(RequestId(2))));
    let snapshot = snapshot_of(s.snapshot(RequestId(3)));
    let row = |name: &str| {
        snapshot
            .settings
            .iter()
            .find(|r| r.name.spec().name == name)
            .unwrap()
            .clone()
    };
    let effort_row = row("effort");
    assert_eq!(effort_row.layer, Layer::Session);
    assert_eq!(effort_row.value, Some(SettingValue::Text("low".to_owned())));
    assert_eq!(effort_row.shadowed.len(), 1);
    assert_eq!(effort_row.shadowed[0].layer, Layer::Default);
    assert_eq!(row("model").layer, Layer::Flag);
    assert_eq!(row("cache_ttl").layer, Layer::Config);
}

#[test]
fn a_switched_replay_names_the_pair_its_rows_were_resolved_for() {
    let window = wisp_protocol::settings::find("context_window").unwrap();
    let at = |model: &str| {
        wisp_protocol::settings::SitePath::new(
            wisp_protocol::settings::Place::Model(model.to_owned()),
            window,
        )
    };
    let saved = vec![
        SavedValue::of(at("m1"), SettingValue::Tokens(400_000), Some(4)),
        SavedValue::of(at("m2"), SettingValue::Tokens(300_000), Some(5)),
    ];
    let mut s = described_with(FileState::Missing, saved);
    s.described.settings.push(ResolvedSetting {
        name: wisp_protocol::settings::SettingName::of(window),
        value: Some(SettingValue::Tokens(400_000)),
        layer: Layer::Model,
        origin: at("m1").to_string(),
        shadowed: Vec::new(),
        choices: Vec::new(),
        applies: true,
    });
    let model = wisp_protocol::settings::find("model").unwrap();
    let switch = vec![SettingChange::set(
        model,
        SettingValue::Text("m2".to_owned()),
    )];
    s.command(RequestId(1), &switch, false);
    let window_row = |snapshot: &SettingsSnapshot| {
        crate::app::config_screen::model_rows(snapshot, &snapshot.context.model)
            .into_iter()
            .find(|r| r.spec.name == "context_window")
            .unwrap()
    };
    let pending = snapshot_of(s.snapshot(RequestId(2)));
    assert_eq!(pending.context.model, "m1");
    assert_eq!(window_row(&pending).changed, None);
    s.start_turn();
    let started = snapshot_of(s.snapshot(RequestId(3)));
    assert_eq!(started.context.model, "m1");
    let row = window_row(&started);
    assert_eq!((row.value.as_str(), row.changed.clone()), ("400,000", None));
    let model_row = started
        .settings
        .iter()
        .find(|r| r.name.spec().name == "model")
        .unwrap();
    assert_eq!(model_row.value, Some(SettingValue::Text("m2".to_owned())));
}

#[tokio::test(start_paused = true)]
async fn a_plugin_command_is_refused_by_request_idle_and_during_a_turn() {
    use wisp_protocol::RequestId;
    let run = |request: u64| ClientCommand::RunPluginCommand {
        name: "testkit:go".into(),
        args: wisp_protocol::Secret::new(String::new()),
        request: RequestId(request),
    };
    let refused = |e: &AgentEvent| matches!(e, AgentEvent::Rejected { .. });
    let want = |request: u64| AgentEvent::refused(RequestId(request), NO_REPLAYED_COMMANDS);
    let (commands, mut events) = spawn(Script::new(answer(1000)), PACE);
    commands.send(run(1)).await.unwrap();
    let seen = until(&mut events, refused).await;
    assert_eq!(seen, [want(1)]);
    commands
        .send(ClientCommand::send("t-1", "go"))
        .await
        .unwrap();
    until(&mut events, |e| matches!(e, AgentEvent::TextDelta { .. })).await;
    commands.send(run(2)).await.unwrap();
    let seen: Vec<AgentEvent> = until(&mut events, refused)
        .await
        .into_iter()
        .filter(|e| !matches!(e, AgentEvent::TextDelta { .. }))
        .collect();
    assert_eq!(seen, [want(2)]);
}

#[test]
fn private_auth_is_never_retained_by_parsed_or_constructed_replay_scripts() {
    use wisp_protocol::subscription_auth::{
        AuthEvent, AuthOutcome, ChallengeKind, PrivateChallenge, SubscriptionProvider,
    };
    let private = AgentEvent::SubscriptionAuth {
        event: AuthEvent {
            request: RequestId(4),
            outcome: AuthOutcome::Challenge {
                challenge: PrivateChallenge {
                    login: RequestId(4),
                    provider: SubscriptionProvider::Claude,
                    alias: "plan".into(),
                    kind: ChallengeKind::ManualCode,
                    authorization_url: wisp_protocol::Secret::new(
                        "https://example.invalid/?state=STATE_CANARY&code=CODE_CANARY",
                    ),
                    expires_in_secs: 600,
                    opened: false,
                },
            },
        },
    };
    let child = AgentEvent::Agent {
        agent: wisp_protocol::AgentPath::new("/child"),
        event: Box::new(private.clone()),
    };
    let events = vec![private, child];
    let jsonl = events
        .iter()
        .map(|event| serde_json::to_string(event).unwrap())
        .collect::<Vec<_>>()
        .join("\n");
    assert!(parse(&jsonl).unwrap().is_empty());
    let script = Script::new(events);
    assert!(script.preamble.is_empty());
    assert!(script.turns.is_empty());
}

#[tokio::test(start_paused = true)]
async fn a_steer_leaves_an_open_question_open_until_it_is_answered() {
    let question = wisp_protocol::Question {
        header: "Plan".into(),
        question: "Which plan?".into(),
        options: vec![wisp_protocol::QuestionOption {
            label: "Keep".into(),
            description: String::new(),
        }],
        multi_select: false,
        source: None,
    };
    let answered = AgentEvent::QuestionClosed {
        call_id: "q".into(),
        how: QuestionEnd::Answered {
            answers: vec![wisp_protocol::Answer::choose(0)],
        },
    };
    let mut events = answer(0);
    events.insert(
        1,
        AgentEvent::QuestionAsked {
            call_id: "q".into(),
            questions: vec![question],
        },
    );
    events.insert(2, answered.clone());
    let (commands, mut out) = spawn(Script::new(events), PACE);
    let closed = |e: &AgentEvent| matches!(e, AgentEvent::QuestionClosed { .. });
    commands
        .send(send("t-1", DeliveryMode::Steer, None))
        .await
        .unwrap();
    until(&mut out, |e| matches!(e, AgentEvent::QuestionAsked { .. })).await;
    commands
        .send(send("t-2", DeliveryMode::Steer, Some(1)))
        .await
        .unwrap();
    let waited = until(&mut out, closed).await;
    assert!(!waited.iter().any(closed), "{waited:?}");
    let reply = ClientCommand::Answer {
        call_id: "q".into(),
        answers: vec![wisp_protocol::Answer::choose(0)],
    };
    commands.send(reply).await.unwrap();
    assert_eq!(until(&mut out, closed).await.last(), Some(&answered));
}

#[test]
fn server_sign_in_links_are_never_retained_in_either_replay_path() {
    let event: AgentEvent = serde_json::from_value(serde_json::json!({"type":"plugins_changed","notes":[{"plugin":"mcp","message":"child added server local"}],"links":[{"plugin":"mcp","message":"sign in","link":"https://example.invalid/authorize?CANARY-placeholder"}]})).unwrap();
    let nested = AgentEvent::Agent {
        agent: wisp_protocol::AgentPath::new("/child"),
        event: Box::new(event.clone()),
    };
    let events = vec![event, nested];
    let jsonl = events
        .iter()
        .map(|event| serde_json::to_string(event).unwrap())
        .collect::<Vec<_>>()
        .join("\n");
    let parsed = parse(&jsonl).unwrap();
    let public = serde_json::to_string(&parsed).unwrap();
    assert!(!public.contains("CANARY-placeholder"));
    assert!(public.contains("child added server local"));
    let script = Script::new(events);
    let preamble = serde_json::to_string(&script.preamble).unwrap();
    assert!(!preamble.contains("CANARY-placeholder"));
    assert!(preamble.contains("child added server local"));
    let only: AgentEvent = serde_json::from_value(serde_json::json!({"type":"plugins_changed","links":[{"plugin":"mcp","message":"sign in","link":"https://example.invalid/authorize?CANARY-placeholder"}]})).unwrap();
    let events = vec![
        only.clone(),
        AgentEvent::Agent {
            agent: wisp_protocol::AgentPath::new("/child"),
            event: Box::new(only),
        },
    ];
    let jsonl = events
        .iter()
        .map(|event| serde_json::to_string(event).unwrap())
        .collect::<Vec<_>>()
        .join("\n");
    assert!(parse(&jsonl).unwrap().is_empty());
    assert!(Script::new(events).preamble.is_empty());
}
