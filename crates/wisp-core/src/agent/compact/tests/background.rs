use wisp_protocol::settings::{Place, SitePath, find};
use wisp_protocol::{
    DiscardReason, Effort, FailurePhase, Message, Placement, RetryClass, Role, SearchEnd,
    UnfinishedReason,
};

use super::*;
use crate::agent::retry::{RetryPolicy, quick};
use crate::agent::{Pending, TurnTrack, check_layout};
use crate::provider::Thread;
use crate::settings::host::StartLayers;
use crate::test_guard::records;

pub(super) const POINT: &str = "[models.\"claude-opus-5-5\"]\ncompact_at = 20000\n";
const POINT_NO_NATIVE: &str =
    "[models.\"claude-opus-5-5\"]\ncompact_at = 20000\nnative_compaction = false\n";
pub(super) const NEAR: u64 = 19_500;

pub(super) fn saved(entries: Vec<(Place, &str, SettingValue)>) -> StartLayers {
    let mut layers = StartLayers::default();
    for (place, name, value) in entries {
        let spec = find(name).unwrap();
        layers.saved.insert(SitePath::new(place, spec), value, 1);
    }
    layers
}

pub(super) fn small_band() -> Vec<(Place, &'static str, SettingValue)> {
    vec![(
        Place::Global,
        "context.background_tokens",
        SettingValue::Tokens(1_000),
    )]
}

pub(super) fn agent_with(
    provider: &Arc<Fake>,
    store: &MemoryStore,
    config: &str,
    entries: Vec<(Place, &str, SettingValue)>,
) -> Agent {
    logged(provider.clone(), store)
        .with_model_settings(models(config))
        .with_start_layers(saved(entries))
        .with_retry_policy(RetryPolicy {
            step_retries: Some(1),
            ..quick()
        })
}

pub(super) fn background(provider: &Arc<Fake>, store: &MemoryStore) -> Agent {
    agent_with(provider, store, POINT, small_band())
}

pub(super) struct Landing {
    gate: Arc<Semaphore>,
    landed: Arc<Semaphore>,
}

impl Landing {
    pub(super) fn new() -> Self {
        Self {
            gate: Arc::new(Semaphore::new(0)),
            landed: Arc::new(Semaphore::new(0)),
        }
    }

    pub(super) fn gated(&self, step: Step) -> Step {
        let landed = self.landed.clone();
        step.gated(&self.gate).run(move || landed.add_permits(1))
    }

    pub(super) async fn land(&self) {
        self.gate.add_permits(1);
        within("the precompute's end", self.landed.acquire())
            .await
            .unwrap()
            .forget();
    }
}

fn is_background(event: &AgentEvent) -> bool {
    matches!(
        event,
        AgentEvent::CompactionStarted {
            trigger: CompactionTrigger::Background,
            ..
        } | AgentEvent::CompactionPrepared { .. }
            | AgentEvent::Compacted {
                trigger: CompactionTrigger::Background,
                ..
            }
            | AgentEvent::CompactionFailed {
                trigger: CompactionTrigger::Background,
                ..
            }
    )
}

pub(super) fn discarded(seen: &[AgentEvent]) -> Vec<(String, Option<u64>)> {
    pick!(
        seen,
        AgentEvent::CompactionFailed {
            trigger: CompactionTrigger::Background,
            kind: CompactionFailure::Discarded { reason },
            turn,
            ..
        } => (reason.clone(), *turn)
    )
}

pub(super) fn tool_call(input: u64, words: &str) -> Step {
    let mut script = vec![start(input, 0)];
    script.extend(text(0, words));
    script.extend(tool_use(1, "toolu_1", "bash", &["{\"command\":\"true\"}"]));
    script.extend(finish(StopReason::ToolUse, input, 0, 40));
    Step::from(script)
}

fn lead_of(body: &Value) -> bool {
    let first = &body["messages"][0];
    first["role"] == "assistant"
        && first["content"].as_array().map(Vec::len) == Some(1)
        && first["content"][0]["type"] == "compaction"
}

#[tokio::test]
async fn a_summary_prepared_between_turns_is_swapped_in_before_the_next_turn() {
    let store = MemoryStore::default();
    let provider = documented(vec![
        answer("Hello.", NEAR),
        compaction(SUMMARY),
        answer("Again.", 300),
    ]);
    let mut live = Live::start(background(&provider, &store));
    assert!(completed(&live.turn("u1", "first").await));
    live.until(prepared).await;
    assert!(completed(&live.turn("u2", "second").await));
    let (agent, seen) = live.close().await;
    let sequence: Vec<&AgentEvent> = seen
        .iter()
        .filter(|e| is_background(e) || matches!(e, AgentEvent::TurnStarted { .. }))
        .collect();
    assert!(
        matches!(
            sequence.as_slice(),
            [
                AgentEvent::TurnStarted { .. },
                AgentEvent::CompactionStarted {
                    trigger: CompactionTrigger::Background,
                    turn: None,
                    ..
                },
                AgentEvent::CompactionPrepared {
                    turn: None,
                    cost: Some(_),
                    usage: Some(_),
                    ..
                },
                AgentEvent::Compacted {
                    trigger: CompactionTrigger::Background,
                    turn: Some(2),
                    usage: None,
                    cost: None,
                    ..
                },
                AgentEvent::TurnStarted { .. },
            ]
        ),
        "{sequence:#?}"
    );
    let sent = provider.sent();
    assert_eq!(sent.len(), 3);
    assert!(
        sent[1].body().get("compaction").is_some()
            || sent[1].body().to_string().contains("summarize")
    );
    assert_eq!(sent[1].body()["messages"].as_array().unwrap().len(), 1);
    let swapped = &sent[2].body();
    assert!(lead_of(swapped), "{swapped}");
    let messages = swapped["messages"].as_array().unwrap();
    assert_eq!(messages[1]["role"], "assistant");
    assert!(messages[1].to_string().contains("Hello."));
    let user = messages[2].to_string();
    let context = user.find("compacted_context").unwrap();
    let typed = user.find("second").unwrap();
    assert!(context < typed, "{user}");
    assert_eq!(messages.len(), 3);
    let records = log_events(&store);
    let kinds: Vec<&str> = records
        .iter()
        .filter_map(|e| match e {
            LogEvent::CompactionPrepared { turn: None, .. } => Some("prepared"),
            LogEvent::ContextCompacted {
                trigger: CompactionTrigger::Background,
                placement: Placement::Leading,
                cut: Some(1),
                turn: Some(2),
                usage: None,
                ..
            } => Some("swapped"),
            LogEvent::TurnStarted { turn: 2, .. } => Some("started"),
            _ => None,
        })
        .collect();
    assert_eq!(kinds, ["prepared", "swapped", "started"]);
    assert_rebuilds(&store, &agent);
    assert_eq!(agent.background_phase(), "none");
}

#[tokio::test]
async fn background_unknown_events_survive_completion_retry_and_abort() {
    for ending in ["completed", "failed", "retried", "aborted"] {
        let store = MemoryStore::default();
        let observed = Arc::new(Semaphore::new(0));
        let mut first = match ending {
            "completed" => compaction(SUMMARY),
            "failed" => failing(ProviderError::new(ErrorKind::InvalidRequest, "placeholder")),
            "retried" => overloaded(),
            "aborted" => Step::hang(),
            _ => unreachable!(),
        };
        let unknown = |attempt| {
            Ok(ProviderEvent::Unknown {
                provider: "anthropic".into(),
                raw: json!({"type":"future_background_event","attempt":attempt,"account_id":"placeholder"}),
            })
        };
        first.script.insert(0, unknown(0));
        let streamed = observed.clone();
        let mut steps = vec![
            answer("Hello.", NEAR),
            first.run(move || streamed.add_permits(1)),
        ];
        if ending == "retried" {
            let mut retry = compaction(SUMMARY);
            retry.script.insert(0, unknown(1));
            steps.push(retry);
        }
        let provider = documented(steps);
        let mut live = Live::start(background(&provider, &store));
        assert!(completed(&live.turn("u1", "first").await));
        within("the unknown event being streamed", observed.acquire())
            .await
            .unwrap()
            .forget();
        match ending {
            "completed" | "retried" => {
                live.until(prepared).await;
            }
            "failed" => {
                live.until(|event| {
                    matches!(
                        event,
                        AgentEvent::CompactionFailed {
                            trigger: CompactionTrigger::Background,
                            ..
                        }
                    )
                })
                .await;
            }
            _ => {}
        }
        let (_, seen) = live.close().await;
        let rows = log_events(&store);
        let logged = pick!(&rows, LogEvent::UnknownProviderEvent { turn, event, .. } => (*turn, event.clone()));
        let expected: Vec<_> = (0..if ending == "retried" { 2 } else { 1 })
            .map(|attempt| {
                (
                    None,
                    json!({"type":"future_background_event","attempt":attempt}),
                )
            })
            .collect();
        assert_eq!(logged, expected, "{ending}");
        assert_eq!(
            pick!(&seen, AgentEvent::UnknownProviderEvent { event, .. } => event.clone()),
            expected
                .into_iter()
                .map(|(_, event)| event)
                .collect::<Vec<_>>(),
            "{ending}"
        );
        assert!(matches!(rows.last(), Some(LogEvent::SessionEnded { .. })));
    }
}

#[tokio::test]
async fn no_precompute_below_the_band_or_when_turned_off() {
    let off = |place: Place| {
        let mut entries = small_band();
        entries.push((
            place,
            "context.background_compaction",
            SettingValue::Bool(false),
        ));
        entries
    };
    let cases = vec![
        (POINT, small_band(), NEAR - 8_000),
        (POINT, off(Place::Global), NEAR),
        (POINT, off(Place::Endpoint("env".into())), NEAR),
        (POINT, off(Place::Model(DEFAULT_MODEL.into())), NEAR),
        (
            POINT,
            off(Place::EndpointModel {
                endpoint: "env".into(),
                model: DEFAULT_MODEL.into(),
            }),
            NEAR,
        ),
        (POINT_NO_NATIVE, small_band(), NEAR),
    ];
    let documented = cases.into_iter().map(|case| (case, true));
    let trial = std::iter::once(((POINT, small_band(), NEAR), false));
    for ((config, entries, input), established) in documented.chain(trial) {
        let store = MemoryStore::default();
        let steps = vec![answer("Hello.", input), answer("Again.", 300)];
        let provider = if established {
            super::documented(steps)
        } else {
            scripted(steps)
        };
        let mut live = Live::start(agent_with(&provider, &store, config, entries));
        assert!(completed(&live.turn("u1", "first").await));
        assert!(completed(&live.turn("u2", "second").await));
        let (_, seen) = live.close().await;
        assert!(!seen.iter().any(is_background), "{config} {input}");
        assert_eq!(provider.sent().len(), 2);
    }
}

#[tokio::test]
async fn a_model_table_wins_over_the_endpoint_for_the_setting() {
    let store = MemoryStore::default();
    let mut entries = small_band();
    entries.push((
        Place::Endpoint("env".into()),
        "context.background_compaction",
        SettingValue::Bool(false),
    ));
    entries.push((
        Place::Model(DEFAULT_MODEL.into()),
        "context.background_compaction",
        SettingValue::Bool(true),
    ));
    let provider = documented(vec![answer("Hello.", NEAR), compaction(SUMMARY)]);
    let mut live = Live::start(agent_with(&provider, &store, POINT, entries));
    assert!(completed(&live.turn("u1", "first").await));
    live.until(prepared).await;
    let (_, seen) = live.close().await;
    assert_eq!(
        discarded(&seen),
        [(DiscardReason::SessionEnded.as_str().to_owned(), None)]
    );
}

#[tokio::test]
async fn auto_compact_off_means_no_precompute() {
    let store = MemoryStore::default();
    let provider = documented(vec![answer("Hello.", NEAR), answer("Again.", 300)]);
    let agent = background(&provider, &store).with_auto_compact(false);
    let mut live = Live::start(agent);
    assert!(completed(&live.turn("u1", "first").await));
    assert!(completed(&live.turn("u2", "second").await));
    let (_, seen) = live.close().await;
    assert!(!seen.iter().any(is_background));
}

#[tokio::test]
async fn a_summary_arriving_during_a_tool_loop_swaps_at_the_next_step() {
    let store = MemoryStore::default();
    let precompute = Landing::new();
    let step = Arc::new(Semaphore::new(0));
    let provider = documented(vec![
        answer("Hello.", NEAR),
        precompute.gated(compaction(SUMMARY)),
        tool_call(NEAR + 200, "Looking.").gated(&step),
        answer("Done.", 400),
    ]);
    let mut live = Live::start(background(&provider, &store));
    assert!(completed(&live.turn("u1", "first").await));
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
    assert!(
        seen.iter()
            .any(|e| matches!(e, AgentEvent::CompactionPrepared { turn: Some(2), .. }))
    );
    assert!(seen.iter().any(|e| matches!(
        e,
        AgentEvent::Compacted {
            trigger: CompactionTrigger::Background,
            turn: Some(2),
            ..
        }
    )));
    let sent = provider.sent();
    assert_eq!(sent.len(), 4);
    let body = &sent[3].body();
    assert!(lead_of(body), "{body}");
    let messages = body["messages"].as_array().unwrap();
    let last = messages.last().unwrap();
    assert_eq!(last["role"], "user");
    let content = last["content"].as_array().unwrap();
    assert_eq!(content[0]["type"], "tool_result");
    assert!(
        content
            .last()
            .unwrap()
            .to_string()
            .contains("compacted_context")
    );
    assert!(!body.to_string().contains(CONTINUE_NOTE));
    assert!(agent.history.iter().all(|m| {
        !m.content
            .iter()
            .any(|b| matches!(b, ContentBlock::Text { text } if text.contains("compacted_context")))
    }));
    assert_rebuilds(&store, &agent);
}

#[tokio::test]
async fn too_much_added_while_it_ran_discards_it_as_bound() {
    let store = MemoryStore::default();
    let precompute = Landing::new();
    let step = Arc::new(Semaphore::new(0));
    let long = "x".repeat(12_000);
    let provider = documented(vec![
        answer("Hello.", NEAR),
        precompute.gated(compaction(SUMMARY)),
        tool_call(NEAR + 200, &long).gated(&step),
        answer("Done.", 400),
    ]);
    let mut live = Live::start(background(&provider, &store));
    assert!(completed(&live.turn("u1", "first").await));
    live.send(send("u2", "second")).await;
    live.until(|e| matches!(e, AgentEvent::StepStarted { step: 1 }))
        .await;
    precompute.land().await;
    step.add_permits(1);
    live.until(|e| matches!(e, AgentEvent::TurnEnded { .. }))
        .await;
    let (_, seen) = live.close().await;
    assert_eq!(
        discarded(&seen),
        [(DiscardReason::Bound.as_str().to_owned(), Some(2))]
    );
    assert!(!lead_of(&provider.body(3)));
}

#[tokio::test]
async fn a_manual_compact_discards_a_ready_summary_and_compacts_afresh() {
    let store = MemoryStore::default();
    let provider = documented(vec![
        answer("Hello.", NEAR),
        compaction(SUMMARY),
        compaction("Fresh."),
    ]);
    let mut live = Live::start(background(&provider, &store));
    assert!(completed(&live.turn("u1", "first").await));
    live.until(prepared).await;
    let answer = live.compact(1, None).await;
    assert!(matches!(
        answer,
        AgentEvent::Compacted {
            trigger: CompactionTrigger::Manual,
            ..
        }
    ));
    let (agent, seen) = live.close().await;
    assert_eq!(
        discarded(&seen),
        [(DiscardReason::Superseded.as_str().to_owned(), None)]
    );
    assert!(head_text(&agent).contains("compacted_context"));
    assert!(!lead_of(&provider.body(2)));
}

#[tokio::test]
async fn a_queued_model_switch_discards_a_ready_summary() {
    let store = MemoryStore::default();
    let provider = documented(vec![answer("Hello.", NEAR), compaction(SUMMARY)]);
    let mut live = Live::start(background(&provider, &store));
    assert!(completed(&live.turn("u1", "first").await));
    live.until(prepared).await;
    live.send(ClientCommand::shortcut(
        RequestId(5),
        None,
        Some("claude-sonnet-5-5".into()),
        None,
    ))
    .await;
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
    let (_, seen) = live.close().await;
    assert_eq!(
        discarded(&seen),
        [(DiscardReason::Superseded.as_str().to_owned(), None)]
    );
}

#[tokio::test]
async fn turning_it_off_discards_a_ready_summary_at_once() {
    let store = MemoryStore::default();
    let provider = documented(vec![answer("Hello.", NEAR), compaction(SUMMARY)]);
    let mut live = Live::start(background(&provider, &store));
    assert!(completed(&live.turn("u1", "first").await));
    live.until(prepared).await;
    live.send(ClientCommand::ChangeSettings {
        request: RequestId(6),
        changes: SettingChange::named("context.background_compaction", SettingValue::Bool(false))
            .into_iter()
            .collect(),
        save: false,
    })
    .await;
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
    let (_, seen) = live.close().await;
    assert_eq!(
        discarded(&seen),
        [(DiscardReason::Superseded.as_str().to_owned(), None)]
    );
}

#[tokio::test]
async fn a_step_retry_after_the_summary_arrives_discards_it_as_resent() {
    let store = MemoryStore::default();
    let precompute = Landing::new();
    let step = Arc::new(Semaphore::new(0));
    let provider = documented(vec![
        answer("Hello.", NEAR),
        precompute.gated(compaction(SUMMARY)),
        overloaded().gated(&step),
        answer("Again.", 300),
    ]);
    let mut live = Live::start(background(&provider, &store));
    assert!(completed(&live.turn("u1", "first").await));
    live.send(send("u2", "second")).await;
    live.until(|e| matches!(e, AgentEvent::StepStarted { step: 1 }))
        .await;
    precompute.land().await;
    step.add_permits(1);
    let outcome = live
        .until(|e| matches!(e, AgentEvent::TurnEnded { .. }))
        .await;
    assert!(matches!(
        outcome,
        AgentEvent::TurnEnded {
            outcome: TurnOutcome::Completed { .. },
            ..
        }
    ));
    let (_, seen) = live.close().await;
    assert_eq!(
        discarded(&seen),
        [(DiscardReason::Resent.as_str().to_owned(), Some(2))]
    );
    let sent = provider.sent();
    assert_eq!(
        strip_cache_control(&sent[2].body())["messages"],
        strip_cache_control(&sent[3].body())["messages"]
    );
}

#[tokio::test]
async fn a_thread_replay_after_the_summary_arrives_keeps_it() {
    let store = MemoryStore::default();
    let precompute = Landing::new();
    let step = Arc::new(Semaphore::new(0));
    let refused = ProviderError::new(ErrorKind::InvalidRequest, "thread").refusing(Refused::Thread);
    let provider = documented(vec![
        answer("Hello.", NEAR),
        precompute.gated(compaction(SUMMARY)),
        failing(refused).gated(&step),
        answer("Again.", 300),
    ])
    .view(|k| {
        k.threads = true;
        k.wire_print = true;
    });
    let mut live = Live::start(background(&provider, &store));
    assert!(completed(&live.turn("u1", "first").await));
    live.send(send("u2", "second")).await;
    live.until(|e| matches!(e, AgentEvent::StepStarted { step: 1 }))
        .await;
    precompute.land().await;
    step.add_permits(1);
    live.until(|e| matches!(e, AgentEvent::TurnEnded { .. }))
        .await;
    let (_, seen) = live.close().await;
    assert_eq!(
        discarded(&seen),
        [(DiscardReason::SessionEnded.as_str().to_owned(), None)]
    );
    let threads: Vec<_> = provider.sent().iter().map(|s| s.features.thread).collect();
    assert!(
        matches!(threads[2], Some(Thread::Continue { .. })),
        "{threads:?}"
    );
    assert_eq!([threads[1], threads[3]], [None, Some(Thread::Create)]);
}

#[tokio::test]
async fn a_failed_precompute_ends_once_and_the_point_still_compacts() {
    let billed = || erring(overloaded(), ProviderError::possibly_billed);
    for (first, last) in [(billed(), billed()), (overloaded(), cut_off(1_200))] {
        let store = MemoryStore::default();
        let provider = documented(vec![
            answer("Hello.", NEAR),
            first,
            last,
            answer("Again.", 300),
        ]);
        let mut live = Live::start(background(&provider, &store));
        assert!(completed(&live.turn("u1", "first").await));
        live.until(|e| {
            matches!(
                e,
                AgentEvent::CompactionFailed {
                    trigger: CompactionTrigger::Background,
                    kind: CompactionFailure::Error(ErrorKind::Overloaded),
                    turn: None,
                    ..
                }
            )
        })
        .await;
        assert!(completed(&live.turn("u2", "second").await));
        let (agent, seen) = live.close().await;
        assert_eq!(provider.sent().len(), 4);
        assert_eq!(
            strip_cache_control(&provider.body(1)),
            strip_cache_control(&provider.body(2))
        );
        assert!(!lead_of(&provider.body(3)));
        let failures = seen
            .iter()
            .filter(|e| {
                matches!(
                    e,
                    AgentEvent::CompactionFailed {
                        trigger: CompactionTrigger::Background,
                        ..
                    }
                )
            })
            .count();
        assert_eq!(failures, 1);
        assert!(
            !seen
                .iter()
                .any(|e| matches!(e, AgentEvent::Retrying { .. }))
        );
        assert!(log_events(&store).iter().any(|e| matches!(
            e,
            LogEvent::CompactionFailed {
                trigger: CompactionTrigger::Background,
                turn: None,
                lower_bound: true,
                ..
            }
        )));
        assert!(agent.session_lower_bound);
        assert!(agent.accounts.values().any(|a| a.lower_bound));
        assert!(resumed_bound(&store, |e| matches!(
            e,
            LogEvent::CompactionFailed { .. }
        )));
    }
}

fn erring(mut step: Step, edit: impl FnOnce(ProviderError) -> ProviderError) -> Step {
    if let Some(Err(error)) = step.script.pop() {
        step.script.push(Err(edit(error)));
    }
    step
}

fn resumed_bound(store: &MemoryStore, last: impl Fn(&LogEvent) -> bool) -> bool {
    let records = records(store);
    let end = records.iter().position(|r| last(&r.event)).unwrap();
    let logged = crate::resume::LoggedSession::read(&records[..=end]).unwrap();
    logged.lower_bound && logged.accounts.values().any(|a| a.lower_bound)
}

#[tokio::test]
async fn closing_the_session_discards_a_running_precompute_with_a_lower_bound() {
    let store = MemoryStore::default();
    let provider = documented(vec![answer("Hello.", NEAR), Step::hang()]);
    let mut live = Live::start(background(&provider, &store));
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
    tokio::time::sleep(Duration::from_millis(50)).await;
    let (_, seen) = live.close().await;
    let cost = seen.iter().find_map(|e| match e {
        AgentEvent::CompactionFailed {
            trigger: CompactionTrigger::Background,
            kind: CompactionFailure::Discarded { reason },
            cost,
            ..
        } if reason == "session_ended" => Some(*cost),
        _ => None,
    });
    let cost = cost.unwrap().unwrap();
    assert!(cost.turn_lower_bound && cost.turn_usd > 0.0);
}

async fn at_step(agent: &mut Agent, pending: &mut Pending) -> Vec<AgentEvent> {
    let (sink, mut events) = mpsc::channel(64);
    let (_commands_tx, mut commands) = mpsc::channel(8);
    agent
        .background_at_step(pending, &mut commands, &sink)
        .await;
    drop(sink);
    let mut seen = Vec::new();
    while let Some(event) = events.recv().await {
        seen.push(event);
    }
    seen
}

fn lead_block() -> ContentBlock {
    ContentBlock::opaque("anthropic", raw_block(SUMMARY))
}

fn two_turns() -> Vec<Message> {
    vec![
        Message::user_text("first"),
        Message {
            role: Role::Assistant,
            content: vec![ContentBlock::Text {
                text: "Hello.".into(),
            }],
        },
    ]
}

fn ready_agent() -> Agent {
    let provider = documented(vec![]);
    let mut agent = Agent::new(provider, default_session()).with_model_settings(models(POINT));
    agent.history = two_turns();
    agent.track = Some(TurnTrack::new(2));
    agent.ready_background(vec![Message::user_text("first")], lead_block());
    agent
}

#[tokio::test]
async fn a_request_carrying_the_unsaved_message_discards_the_block() {
    let mut agent = ready_agent();
    let mut pending = Pending {
        user: Some(vec![ContentBlock::Text {
            text: "second".into(),
        }]),
        ..Pending::default()
    };
    let seen = at_step(&mut agent, &mut pending).await;
    assert_eq!(discarded(&seen), [("unsaved".to_owned(), Some(2))]);
    assert!(agent.compaction.head.is_none());
}

#[tokio::test]
async fn a_retry_first_request_discards_the_block_as_resent() {
    let mut agent = ready_agent();
    agent.track().resend_first = true;
    let seen = at_step(&mut agent, &mut Pending::default()).await;
    assert_eq!(discarded(&seen), [("resent".to_owned(), Some(2))]);
}

#[tokio::test]
async fn a_changed_view_discards_the_block() {
    let mut agent = ready_agent();
    agent.ready_background(vec![Message::user_text("other")], lead_block());
    agent.track().committed_steps = 1;
    let seen = at_step(&mut agent, &mut Pending::default()).await;
    assert_eq!(discarded(&seen), [("view_changed".to_owned(), Some(2))]);
    let mut agent = ready_agent();
    agent.ready_background(two_turns(), lead_block());
    agent.history.push(Message::user_text("more"));
    agent.track().committed_steps = 1;
    let seen = at_step(&mut agent, &mut Pending::default()).await;
    assert_eq!(discarded(&seen), [("view_changed".to_owned(), Some(2))]);
}

#[tokio::test]
async fn the_kept_view_places_the_block_first_and_the_additions_with_the_next_turn() {
    let mut agent = ready_agent();
    agent.track().committed_steps = 1;
    let seen = at_step(&mut agent, &mut Pending::default()).await;
    assert!(seen.iter().any(|e| matches!(
        e,
        AgentEvent::Compacted {
            trigger: CompactionTrigger::Background,
            ..
        }
    )));
    let mut pending = Pending {
        user: Some(vec![ContentBlock::Text {
            text: "second".into(),
        }]),
        opening: 1,
        ..Pending::default()
    };
    pending.marker = None;
    let view = agent.request_view(&pending);
    assert_eq!(view.len(), 3);
    assert_eq!(view[0].role, Role::Assistant);
    assert_eq!(view[0].content, vec![lead_block()]);
    assert_eq!(view[1], two_turns()[1]);
    assert_eq!(view[2].role, Role::User);
    assert!(matches!(
        view[2].content.last(),
        Some(ContentBlock::Text { text }) if text == "second"
    ));
    assert!(matches!(
        &view[2].content[0],
        ContentBlock::Text { text } if text.contains("compacted_context")
    ));
    assert_eq!(agent.full_index(1), Some(1));
    assert_eq!(agent.view_index(1), 1);
    assert_eq!(agent.history_len(2), 2);
}

#[test]
fn the_layout_check_allows_only_a_lone_block_in_a_leading_assistant_message() {
    let provider = documented(vec![]);
    let assistant = |content: Vec<ContentBlock>| Message {
        role: Role::Assistant,
        content,
    };
    let reply = assistant(vec![ContentBlock::Text {
        text: "Hello.".into(),
    }]);
    let ok = vec![
        assistant(vec![lead_block()]),
        reply.clone(),
        Message::user_text("second"),
    ];
    assert!(check_layout(&ok, &[], &BTreeSet::new(), provider.as_ref()).is_ok());
    let text_first = vec![reply.clone(), Message::user_text("second")];
    assert!(check_layout(&text_first, &[], &BTreeSet::new(), provider.as_ref()).is_err());
    let mixed = vec![
        assistant(vec![
            lead_block(),
            ContentBlock::Text {
                text: "extra".into(),
            },
        ]),
        reply.clone(),
        Message::user_text("second"),
    ];
    assert!(check_layout(&mixed, &[], &BTreeSet::new(), provider.as_ref()).is_err());
    let late = vec![
        Message::user_text("first"),
        reply.clone(),
        reply.clone(),
        Message::user_text("second"),
    ];
    assert!(check_layout(&late, &[], &BTreeSet::new(), provider.as_ref()).is_err());
    let late_after_lead = vec![
        assistant(vec![lead_block()]),
        reply.clone(),
        Message::user_text("second"),
        reply.clone(),
        reply,
        Message::user_text("third"),
    ];
    assert!(check_layout(&late_after_lead, &[], &BTreeSet::new(), provider.as_ref()).is_err());
}

#[tokio::test]
async fn the_additions_and_the_prepared_block_are_logged_past_the_redactions() {
    let secret = "v1-gw-route";
    let store = MemoryStore::default();
    let summary = format!("So far: the {secret} route.");
    let provider = documented(vec![
        answer("Hello.", NEAR),
        compaction(&summary),
        answer("Again.", 300),
    ]);
    let context = LogContext {
        redactions: Redactions::new([secret.to_owned()]),
        ..log_context()
    };
    let agent = Agent::new(provider, default_session())
        .with_log(SessionLog::new(Box::new(store.clone()), "s1"), context)
        .with_model_settings(models(POINT))
        .with_start_layers(saved(small_band()));
    let mut live = Live::start(agent);
    assert!(completed(
        &live.turn("u1", &format!("look at {secret}")).await
    ));
    live.until(prepared).await;
    assert!(completed(&live.turn("u2", "next").await));
    let (agent, _) = live.close().await;
    let head = agent.compaction.head.as_ref().unwrap();
    let logged = log_events(&store);
    let additions = logged.iter().find_map(|e| match e {
        LogEvent::ContextCompacted { additions, .. } => Some(additions.clone()),
        _ => None,
    });
    let additions = additions.unwrap();
    assert_eq!(additions, head.additions);
    assert!(serde_json::to_string(&additions).unwrap().contains(secret));
    let block = logged.iter().find_map(|e| match e {
        LogEvent::CompactionPrepared { block, .. } => Some(block.clone()),
        _ => None,
    });
    assert_eq!(block.unwrap(), raw_block(&summary));
    assert_rebuilds(&store, &agent);
}

#[tokio::test]
async fn a_change_that_moves_the_band_past_the_point_carries_the_line() {
    let store = MemoryStore::default();
    let provider = documented(Vec::new());
    let mut live = Live::start(agent_with(&provider, &store, POINT, Vec::new()));
    let mut change = async |request: u64, name: &str, value: SettingValue| {
        live.send(ClientCommand::ChangeSettings {
            request: RequestId(request),
            changes: SettingChange::named(name, value).into_iter().collect(),
            save: false,
        })
        .await;
        match live
            .until(|e| {
                matches!(e, AgentEvent::SettingsChanged { answers: Some(RequestId(r)), .. } if *r == request)
            })
            .await
        {
            AgentEvent::SettingsChanged { changes, .. } => changes[0].warnings.clone(),
            _ => Vec::new(),
        }
    };
    assert_eq!(
        change(1, "context.background_tokens", SettingValue::Tokens(1_000)).await,
        [
            "background compaction rarely runs below about 21,000, and claude-opus-5-5 compacts at 20,000; compaction will usually wait at the point."
        ]
    );
    assert_eq!(
        change(2, "context.background_tokens", SettingValue::Tokens(30_000)).await,
        [
            "background compaction rarely runs below about 50,000, and claude-opus-5-5 compacts at 20,000; compaction will usually wait at the point."
        ]
    );
    assert!(
        change(
            3,
            "context.background_compaction",
            SettingValue::Bool(false)
        )
        .await
        .is_empty()
    );
    assert!(
        change(4, "context.background_tokens", SettingValue::Tokens(1_000))
            .await
            .is_empty()
    );
    drop(change);
    live.close().await;
}

#[tokio::test]
async fn a_precompute_with_unbounded_turn_retries_still_yields_to_foreground_compaction() {
    let store = MemoryStore::default();
    let policy = RetryPolicy {
        base_delay: Duration::from_millis(1),
        jitter: || 0.0,
        ..RetryPolicy::default()
    };
    let attempts = policy.for_fallback().step_retries.unwrap() as usize + 1;
    let mut script = vec![answer("Hello.", NEAR)];
    script.extend((0..attempts).map(|_| overloaded()));
    script.push(compaction(SUMMARY));
    let provider = documented(script);
    let mut live = Live::start(background(&provider, &store).with_retry_policy(policy));
    assert!(completed(&live.turn("u1", "first").await));
    live.until(|event| {
        matches!(
            event,
            AgentEvent::CompactionFailed {
                trigger: CompactionTrigger::Background,
                ..
            }
        )
    })
    .await;
    let done = live.compact(2, None).await;
    assert!(
        matches!(
            done,
            AgentEvent::Compacted {
                engine: CompactionEngine::Native,
                ..
            }
        ),
        "{done:?}"
    );
    live.close().await;
    assert_eq!(provider.sent().len(), attempts + 2);
}

#[tokio::test]
async fn a_precompute_retries_a_transient_error_like_a_compaction() {
    let store = MemoryStore::default();
    let unended = Step::from(vec![Ok(ProviderEvent::Sending), start(1_200, 0)]);
    let provider = documented(vec![
        answer("Hello.", NEAR),
        unended,
        compaction(SUMMARY),
        answer("Again.", 300),
    ]);
    let mut live = Live::start(background(&provider, &store));
    assert!(completed(&live.turn("u1", "first").await));
    live.until(prepared).await;
    assert!(completed(&live.turn("u2", "second").await));
    let (_, seen) = live.close().await;
    assert!(discarded(&seen).is_empty());
    assert_eq!(provider.sent().len(), 4);
    assert_eq!(
        strip_cache_control(&provider.body(1)),
        strip_cache_control(&provider.body(2))
    );
    assert!(lead_of(&provider.body(3)));
    assert!(log_events(&store).iter().any(|e| matches!(
        e,
        LogEvent::CompactionPrepared {
            lower_bound: true,
            ..
        }
    )));
    assert!(resumed_bound(&store, |e| matches!(
        e,
        LogEvent::CompactionPrepared { .. }
    )));
}

#[tokio::test]
async fn a_summary_arriving_during_a_step_retry_backoff_is_discarded_as_resent() {
    let store = MemoryStore::default();
    let precompute = Arc::new(Semaphore::new(0));
    let step = Arc::new(Semaphore::new(0));
    let provider = documented(vec![
        answer("Hello.", NEAR),
        compaction(SUMMARY).gated(&precompute),
        overloaded().gated(&step),
        answer("Again.", 300),
    ]);
    let agent = logged(provider.clone(), &store)
        .with_model_settings(models(POINT))
        .with_start_layers(saved(small_band()))
        .with_retry_policy(RetryPolicy {
            step_retries: Some(1),
            base_delay: Duration::from_millis(600),
            ..quick()
        });
    let mut live = Live::start(agent);
    assert!(completed(&live.turn("u1", "first").await));
    live.send(send("u2", "second")).await;
    live.until(|e| matches!(e, AgentEvent::StepStarted { step: 1 }))
        .await;
    step.add_permits(1);
    live.until(|e| matches!(e, AgentEvent::Retrying { .. }))
        .await;
    precompute.add_permits(1);
    let outcome = live
        .until(|e| matches!(e, AgentEvent::TurnEnded { .. }))
        .await;
    assert!(matches!(
        outcome,
        AgentEvent::TurnEnded {
            outcome: TurnOutcome::Completed { .. },
            ..
        }
    ));
    let (_, seen) = live.close().await;
    assert_eq!(
        discarded(&seen),
        [(DiscardReason::Resent.as_str().to_owned(), Some(2))]
    );
    let sent = provider.sent();
    assert_eq!(
        strip_cache_control(&sent[2].body())["messages"],
        strip_cache_control(&sent[3].body())["messages"]
    );
}

#[tokio::test]
async fn a_summary_arriving_before_a_request_that_carries_the_unsaved_message_is_discarded() {
    let store = MemoryStore::default();
    let precompute = Landing::new();
    let step = Arc::new(Semaphore::new(0));
    let provider = documented(vec![
        answer("Hello.", NEAR),
        precompute.gated(compaction(SUMMARY)),
        Step::from(
            [
                vec![start(300, 0)],
                finish(StopReason::MaxTokens, 300, 0, 0),
            ]
            .concat(),
        )
        .gated(&step),
        answer("Again.", 300),
    ]);
    let mut live = Live::start(background(&provider, &store));
    assert!(completed(&live.turn("u1", "first").await));
    live.send(send("u2", "second")).await;
    live.until(|e| matches!(e, AgentEvent::StepStarted { step: 1 }))
        .await;
    precompute.land().await;
    live.send(send("u3", "and this")).await;
    live.until(|e| matches!(e, AgentEvent::InputAccepted { id, .. } if id.0 == "u3"))
        .await;
    step.add_permits(1);
    let outcome = live
        .until(|e| matches!(e, AgentEvent::TurnEnded { .. }))
        .await;
    assert!(matches!(
        outcome,
        AgentEvent::TurnEnded {
            outcome: TurnOutcome::Completed { .. },
            ..
        }
    ));
    let (agent, seen) = live.close().await;
    assert_eq!(
        discarded(&seen),
        [(DiscardReason::Unsaved.as_str().to_owned(), Some(2))]
    );
    let last = provider.body(3);
    assert!(!lead_of(&last), "{last}");
    assert!(last.to_string().contains("second"));
    assert!(last.to_string().contains("and this"));
    assert!(agent.compaction.head.is_none());
}

fn server_use(id: &str) -> ContentBlock {
    ContentBlock::opaque(
        "anthropic",
        json!({"type": "server_tool_use", "id": id, "name": "web_search", "input": {"query": "rust"}}),
    )
}

fn server_answer(id: &str) -> ContentBlock {
    ContentBlock::opaque(
        "anthropic",
        json!({"type": "web_search_tool_result", "tool_use_id": id, "content": []}),
    )
}

#[tokio::test]
async fn a_first_kept_message_answering_a_covered_server_call_changes_the_view() {
    let mut agent = ready_agent();
    let covered = vec![
        Message::user_text("first"),
        Message {
            role: Role::Assistant,
            content: vec![server_use("srvtoolu_1")],
        },
        Message::user_text("go on"),
    ];
    agent.history = covered.clone();
    agent.history.push(Message {
        role: Role::Assistant,
        content: vec![
            server_answer("srvtoolu_1"),
            ContentBlock::Text {
                text: "Found it.".into(),
            },
        ],
    });
    agent.sync_history();
    agent.ready_background(covered.clone(), lead_block());
    agent.track().committed_steps = 1;
    let seen = at_step(&mut agent, &mut Pending::default()).await;
    assert_eq!(discarded(&seen), [("view_changed".to_owned(), Some(2))]);
    let mut agent = ready_agent();
    agent.history = covered.clone();
    agent.history.push(Message {
        role: Role::Assistant,
        content: vec![
            server_answer("srvtoolu_9"),
            ContentBlock::Text {
                text: "Found it.".into(),
            },
        ],
    });
    agent.sync_history();
    agent.ready_background(covered, lead_block());
    agent.track().committed_steps = 1;
    let seen = at_step(&mut agent, &mut Pending::default()).await;
    assert!(discarded(&seen).is_empty(), "{seen:#?}");
}

#[tokio::test]
async fn a_first_kept_message_continuing_a_covered_paused_reply_changes_the_view() {
    let mut agent = ready_agent();
    let covered = vec![
        Message::user_text("first"),
        Message {
            role: Role::Assistant,
            content: vec![ContentBlock::Text {
                text: "Searching.".into(),
            }],
        },
    ];
    agent.history = covered.clone();
    agent.history.push(Message {
        role: Role::Assistant,
        content: vec![ContentBlock::Text {
            text: "Done.".into(),
        }],
    });
    agent.sync_history();
    agent.search.paused.insert(1);
    agent.ready_background(covered, lead_block());
    agent.track().committed_steps = 1;
    let seen = at_step(&mut agent, &mut Pending::default()).await;
    assert_eq!(discarded(&seen), [("view_changed".to_owned(), Some(2))]);
}

fn search_then_tool(input: u64) -> Step {
    search_then_tool_with(input, false)
}

fn search_then_tool_with(input: u64, thinks: bool) -> Step {
    let mut script = vec![start(input, 0)];
    script.extend([
        Ok(ProviderEvent::BlockStart {
            index: 0,
            block: server_use("srvtoolu_1"),
        }),
        Ok(ProviderEvent::BlockStop { index: 0 }),
    ]);
    let tool = if thinks {
        script.extend(thinking(1, "Weighing the results.", "sig-1"));
        2
    } else {
        1
    };
    script.extend(tool_use(
        tool,
        "toolu_1",
        "bash",
        &["{\"command\":\"true\"}"],
    ));
    script.extend(finish(StopReason::ToolUse, input, 0, 40));
    Step::from(script)
}

#[tokio::test]
async fn a_kept_reply_with_thinking_and_a_live_server_call_keeps_its_thinking_under_binding() {
    let store = MemoryStore::default();
    let precompute = Landing::new();
    let step = Arc::new(Semaphore::new(0));
    let provider = documented(vec![
        answer("Hello.", NEAR),
        precompute.gated(compaction(SUMMARY)),
        search_then_tool_with(NEAR + 300, true).gated(&step),
        answer("Done.", 300),
    ]);
    let mut live = Live::start(background(&provider, &store));
    assert!(completed(&live.turn("u1", "first").await));
    live.send(send("u2", "second")).await;
    live.until(|e| matches!(e, AgentEvent::StepStarted { step: 1 }))
        .await;
    precompute.land().await;
    step.add_permits(1);
    let outcome = live
        .until(|e| matches!(e, AgentEvent::TurnEnded { .. }))
        .await;
    assert!(ended_completed(&outcome), "{outcome:?}");
    let (agent, _) = live.close().await;
    let logged = log_events(&store);
    assert!(
        !logged
            .iter()
            .any(|e| matches!(e, LogEvent::ThinkingCut { .. })),
        "{logged:#?}"
    );
    assert!(
        logged
            .iter()
            .any(|e| matches!(e, LogEvent::TailClosed { .. }))
    );
    let last = provider.body(3);
    assert!(lead_of(&last), "{last}");
    let kept = last["messages"].to_string();
    assert!(kept.contains("toolu_1"), "{last}");
    assert!(kept.contains("thinking"), "{last}");
    assert_eq!(
        last["thinking"]["block_binding"]["prefix_mismatch_behavior"],
        "drop_block"
    );
    assert_rebuilds(&store, &agent);
}

#[tokio::test]
async fn a_kept_reply_with_a_live_server_call_is_closed_before_the_swap() {
    let store = MemoryStore::default();
    let precompute = Landing::new();
    let step = Arc::new(Semaphore::new(0));
    let provider = documented(vec![
        answer("Hello.", NEAR),
        precompute.gated(compaction(SUMMARY)),
        search_then_tool(NEAR + 300).gated(&step),
        answer("Done.", 300),
    ]);
    let mut live = Live::start(background(&provider, &store));
    assert!(completed(&live.turn("u1", "first").await));
    live.send(send("u2", "second")).await;
    live.until(|e| matches!(e, AgentEvent::StepStarted { step: 1 }))
        .await;
    precompute.land().await;
    step.add_permits(1);
    let outcome = live
        .until(|e| matches!(e, AgentEvent::TurnEnded { .. }))
        .await;
    assert!(matches!(
        outcome,
        AgentEvent::TurnEnded {
            outcome: TurnOutcome::Completed { .. },
            ..
        }
    ));
    let (agent, seen) = live.close().await;
    let logged = log_events(&store);
    let closed = logged
        .iter()
        .position(|e| {
            matches!(e, LogEvent::TailClosed { calls, reason: UnfinishedReason::Compacted, .. } if calls == &["srvtoolu_1"])
        })
        .unwrap_or_else(|| panic!("{logged:#?}"));
    let swapped = logged
        .iter()
        .position(|e| {
            matches!(
                e,
                LogEvent::ContextCompacted {
                    trigger: CompactionTrigger::Background,
                    ..
                }
            )
        })
        .unwrap_or_else(|| panic!("{logged:#?}"));
    assert!(closed < swapped);
    assert!(seen.iter().any(|e| matches!(
        e,
        AgentEvent::SearchEnded {
            outcome: SearchEnd::Unfinished {
                reason: UnfinishedReason::Compacted,
                ..
            },
            ..
        }
    )));
    let last = provider.body(3);
    assert!(lead_of(&last), "{last}");
    assert!(
        !last["messages"].to_string().contains("server_tool_use"),
        "{last}"
    );
    assert!(!last.to_string().contains(CONTINUE_NOTE));
    assert_rebuilds(&store, &agent);
}

#[tokio::test]
async fn a_precompute_refused_for_search_leaves_the_refusal_to_the_next_step() {
    let store = MemoryStore::default();
    let provider = documented(vec![
        answer("Hello.", NEAR),
        search_off(),
        search_off(),
        answer("Again.", 300),
    ]);
    let config = format!(
        "{POINT}\n[endpoints.env.models.\"claude-opus-5-5\"]\nweb_search = [\"web_search_20250305\"]\n"
    );
    let agent = agent_with(&provider, &store, &config, small_band()).with_web_search(true);
    let mut live = Live::start(agent);
    assert!(completed(&live.turn("u1", "first").await));
    assert!(provider.body(0).to_string().contains("web_search_20250305"));
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
    let before = log_events(&store);
    assert!(
        !before
            .iter()
            .any(|e| matches!(e, LogEvent::NativeWebSearch { .. }))
    );
    assert!(completed(&live.turn("u2", "second").await));
    let (_, seen) = live.close().await;
    assert_eq!(
        discarded(&seen),
        [(DiscardReason::Refused.as_str().to_owned(), None)]
    );
    let failed = seen.iter().find_map(|e| match e {
        AgentEvent::CompactionFailed {
            trigger: CompactionTrigger::Background,
            cost,
            usage,
            ..
        } => Some((*cost, *usage)),
        _ => None,
    });
    assert_eq!(failed, Some((None, None)));
    assert_eq!(provider.sent().len(), 4);
    assert!(provider.body(1).to_string().contains("compact"));
    assert!(!provider.body(2).to_string().contains("\"compaction\""));
    assert!(!provider.body(3).to_string().contains("web_search_20250305"));
    let logged = log_events(&store);
    assert_eq!(
        logged
            .iter()
            .filter(|e| matches!(e, LogEvent::NativeWebSearch { .. }))
            .count(),
        1
    );
    assert_eq!(
        seen.iter()
            .filter(|e| matches!(e, AgentEvent::FeatureStateChanged { .. }))
            .count(),
        1
    );
}

fn thinking_answer(words: &str, input: u64) -> Step {
    let mut script = vec![start(input, 0)];
    script.extend(thinking(0, "Considering.", "sig-1"));
    script.extend(text(1, words));
    script.extend(finish(StopReason::EndTurn, input, 0, 40));
    Step::from(script)
}

#[tokio::test]
async fn a_search_refusal_keeps_the_thinking_of_the_single_kept_reply_under_binding() {
    let store = MemoryStore::default();
    let provider = documented(vec![
        thinking_answer("Hello.", NEAR),
        compaction(SUMMARY),
        search_off(),
        answer("Again.", 300),
    ]);
    let config = format!(
        "{POINT}\n[endpoints.env.models.\"claude-opus-5-5\"]\nweb_search = [\"web_search_20250305\"]\n"
    );
    let agent = agent_with(&provider, &store, &config, small_band()).with_web_search(true);
    let mut live = Live::start(agent);
    assert!(completed(&live.turn("u1", "first").await));
    live.until(prepared).await;
    assert!(completed(&live.turn("u2", "second").await));
    let (agent, _) = live.close().await;
    let head = agent.compaction.head.as_ref().map(|h| h.cut);
    assert_eq!(head, Some(1));
    let logged = log_events(&store);
    assert!(
        !logged
            .iter()
            .any(|e| matches!(e, LogEvent::ThinkingCut { .. })),
        "{logged:#?}"
    );
    let swapped = provider.body(2);
    assert!(lead_of(&swapped), "{swapped}");
    assert!(swapped["messages"][1].to_string().contains("thinking"));
    let resent = provider.body(3);
    assert!(lead_of(&resent), "{resent}");
    assert!(!resent.to_string().contains("web_search_20250305"));
    assert_eq!(resent["messages"][1], swapped["messages"][1]);
    assert!(swapped["thinking"].get("block_binding").is_none());
    assert_eq!(
        resent["thinking"]["block_binding"]["prefix_mismatch_behavior"],
        "drop_block"
    );
    assert_rebuilds(&store, &agent);
}

pub(super) fn background_starts(seen: &[AgentEvent]) -> usize {
    seen.iter()
        .filter(|e| {
            matches!(
                e,
                AgentEvent::CompactionStarted {
                    trigger: CompactionTrigger::Background,
                    ..
                }
            )
        })
        .count()
}

fn auto_starts(seen: &[AgentEvent]) -> usize {
    seen.iter()
        .filter(|e| {
            matches!(
                e,
                AgentEvent::CompactionStarted {
                    trigger: CompactionTrigger::Auto,
                    ..
                }
            )
        })
        .count()
}

pub(super) fn swapped_in(seen: &[AgentEvent], turn: u64) -> bool {
    seen.iter().any(|e| {
        matches!(
            e,
            AgentEvent::Compacted {
                trigger: CompactionTrigger::Background,
                turn: Some(t),
                ..
            } if *t == turn
        )
    })
}

fn after_swap_markers(store: &MemoryStore) -> usize {
    log_events(store)
        .iter()
        .skip_while(|e| {
            !matches!(
                e,
                LogEvent::ContextCompacted {
                    trigger: CompactionTrigger::Background,
                    ..
                }
            )
        })
        .filter(|e| {
            matches!(e, LogEvent::MessageCommitted { message, .. } if message.effort_marker().is_some())
        })
        .count()
}

#[tokio::test]
async fn a_mid_turn_swap_that_drops_the_marker_in_force_logs_it_again() {
    let store = MemoryStore::default();
    let precompute = Landing::new();
    let step = Arc::new(Semaphore::new(0));
    let provider = documented(vec![
        tool_call(300, "Looking."),
        answer("Hello.", NEAR),
        precompute.gated(compaction(SUMMARY)),
        tool_call(NEAR + 200, "Again.").gated(&step),
        answer("Done.", 400),
    ]);
    let mut live = Live::start(background(&provider, &store));
    super::marker::change(&mut live, 1, None, Some(Effort::Low)).await;
    assert!(completed(&live.turn("u1", "first").await));
    live.send(send("u2", "second")).await;
    live.until(|e| matches!(e, AgentEvent::StepStarted { step: 1 }))
        .await;
    precompute.land().await;
    step.add_permits(1);
    let outcome = live
        .until(|e| matches!(e, AgentEvent::TurnEnded { .. }))
        .await;
    assert!(ended_completed(&outcome), "{outcome:?}");
    let (agent, seen) = live.close().await;
    assert!(swapped_in(&seen, 2), "{seen:#?}");
    assert_eq!(after_swap_markers(&store), 1);
    let view = agent.view_messages();
    let marker = view
        .iter()
        .position(|m| m.effort_marker().is_some())
        .unwrap_or_else(|| panic!("{view:#?}"));
    assert!(
        view[marker - 1]
            .content
            .iter()
            .any(|b| matches!(b, ContentBlock::ToolResult { .. }))
    );
    assert_eq!(
        view[marker].effort_marker().map(|m| m.level),
        Some(Effort::Low)
    );
    let last = provider.body(4);
    assert!(lead_of(&last), "{last}");
    assert_rebuilds(&store, &agent);
}

#[tokio::test]
async fn an_anchor_ending_in_an_effort_marker_starts_no_precompute_until_the_next_turn() {
    let store = MemoryStore::default();
    let provider = documented(vec![
        answer("Hello.", NEAR),
        answer("Again.", NEAR + 100),
        compaction(SUMMARY),
    ]);
    let mut live = Live::start(background(&provider, &store));
    super::marker::change(&mut live, 1, None, Some(Effort::Low)).await;
    assert!(completed(&live.turn("u1", "first").await));
    tokio::time::sleep(Duration::from_millis(100)).await;
    assert!(completed(&live.turn("u2", "second").await));
    live.until(prepared).await;
    let (_, seen) = live.close().await;
    assert_eq!(background_starts(&seen), 1);
    let second_end = seen
        .iter()
        .enumerate()
        .filter(|(_, e)| matches!(e, AgentEvent::TurnEnded { .. }))
        .nth(1)
        .map(|(i, _)| i)
        .unwrap();
    let started = seen
        .iter()
        .position(|e| {
            matches!(
                e,
                AgentEvent::CompactionStarted {
                    trigger: CompactionTrigger::Background,
                    ..
                }
            )
        })
        .unwrap();
    assert!(started > second_end, "{seen:#?}");
}

#[tokio::test]
async fn a_cancelled_turn_discards_a_running_precompute_with_a_lower_bound() {
    let store = MemoryStore::default();
    let provider = documented(vec![answer("Hello.", NEAR), Step::hang(), Step::hang()]);
    let mut live = Live::start(background(&provider, &store));
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
    tokio::time::sleep(Duration::from_millis(50)).await;
    live.send(send("u2", "second")).await;
    live.until(|e| matches!(e, AgentEvent::StepStarted { step: 1 }))
        .await;
    tokio::time::sleep(Duration::from_millis(50)).await;
    live.send(ClientCommand::Cancel).await;
    let ended = live
        .until(|e| matches!(e, AgentEvent::TurnEnded { .. }))
        .await;
    assert!(matches!(
        ended,
        AgentEvent::TurnEnded {
            outcome: TurnOutcome::Cancelled { .. },
            ..
        }
    ));
    let (_, seen) = live.close().await;
    assert_eq!(discarded(&seen), [("cancelled".to_owned(), None)]);
    let cost = seen
        .iter()
        .find_map(|e| match e {
            AgentEvent::CompactionFailed {
                trigger: CompactionTrigger::Background,
                cost,
                ..
            } => Some(*cost),
            _ => None,
        })
        .flatten()
        .unwrap();
    assert!(cost.turn_lower_bound && cost.turn_usd > 0.0);
}

#[tokio::test]
async fn a_changed_system_tools_or_pair_discards_the_block() {
    let mut agent = ready_agent();
    agent.track().committed_steps = 1;
    agent.session.system = Some("Another prompt.".into());
    let seen = at_step(&mut agent, &mut Pending::default()).await;
    assert_eq!(discarded(&seen), [("view_changed".to_owned(), Some(2))]);
    let mut agent = ready_agent();
    agent.track().committed_steps = 1;
    agent.set_toolbox(crate::tool::test_toolbox());
    let seen = at_step(&mut agent, &mut Pending::default()).await;
    assert_eq!(discarded(&seen), [("view_changed".to_owned(), Some(2))]);
    let mut agent = ready_agent();
    agent.track().committed_steps = 1;
    agent.session.model = "claude-sonnet-4-6".into();
    let seen = at_step(&mut agent, &mut Pending::default()).await;
    assert_eq!(discarded(&seen), [("view_changed".to_owned(), Some(2))]);
    let mut agent = ready_agent();
    agent.track().committed_steps = 1;
    let seen = at_step(&mut agent, &mut Pending::default()).await;
    assert!(discarded(&seen).is_empty(), "{seen:#?}");
    assert!(agent.compaction.head.is_some());
}

#[tokio::test]
async fn reaching_the_point_while_the_precompute_runs_waits_then_swaps() {
    let store = MemoryStore::default();
    let precompute = Arc::new(Semaphore::new(0));
    let provider = documented(vec![
        answer("Hello.", NEAR),
        compaction(SUMMARY).gated(&precompute),
        tool_call(20_500, "Looking."),
        answer("Done.", 300),
    ]);
    let mut live = Live::start(background(&provider, &store));
    assert!(completed(&live.turn("u1", "first").await));
    live.send(send("u2", "second")).await;
    live.until(|e| matches!(e, AgentEvent::ToolCallFinished { .. }))
        .await;
    tokio::time::sleep(Duration::from_millis(200)).await;
    assert_eq!(provider.sent().len(), 3);
    precompute.add_permits(1);
    let outcome = live
        .until(|e| matches!(e, AgentEvent::TurnEnded { .. }))
        .await;
    assert!(ended_completed(&outcome), "{outcome:?}");
    let (agent, seen) = live.close().await;
    assert_eq!(auto_starts(&seen), 0, "{seen:#?}");
    assert!(swapped_in(&seen, 2));
    assert_eq!(provider.sent().len(), 4);
    let last = provider.body(3);
    assert!(lead_of(&last), "{last}");
    assert_rebuilds(&store, &agent);
}

#[tokio::test]
async fn a_precompute_finishing_in_a_turn_adds_its_cost_to_that_turn() {
    let store = MemoryStore::default();
    let precompute = Landing::new();
    let step = Arc::new(Semaphore::new(0));
    let mut summary = compaction(SUMMARY);
    summary.script.insert(
        0,
        Ok(ProviderEvent::Unknown {
            provider: "anthropic".into(),
            raw: json!({"type":"future_background_event"}),
        }),
    );
    let provider = documented(vec![
        answer("Hello.", NEAR),
        precompute.gated(summary),
        tool_call(NEAR + 100, "Looking.").gated(&step),
        answer("Again.", 300),
    ]);
    let mut live = Live::start(background(&provider, &store));
    assert!(completed(&live.turn("u1", "first").await));
    live.send(send("u2", "second")).await;
    live.until(|e| matches!(e, AgentEvent::StepStarted { step: 1 }))
        .await;
    precompute.land().await;
    step.add_permits(1);
    let ended = live
        .until(|e| matches!(e, AgentEvent::TurnEnded { .. }))
        .await;
    let (_, seen) = live.close().await;
    let rows = log_events(&store);
    assert_eq!(
        pick!(&rows, LogEvent::UnknownProviderEvent { turn, .. } => *turn),
        [None]
    );
    let prepared_cost = seen
        .iter()
        .find_map(|e| match e {
            AgentEvent::CompactionPrepared {
                turn: Some(2),
                cost: Some(cost),
                ..
            } => Some(cost.turn_usd),
            _ => None,
        })
        .unwrap_or_else(|| panic!("{seen:#?}"));
    let first = seen
        .iter()
        .find_map(|e| match e {
            AgentEvent::TurnEnded {
                outcome:
                    TurnOutcome::Completed {
                        cost: Some(cost), ..
                    },
                ..
            } => Some(cost.turn_usd),
            _ => None,
        })
        .unwrap();
    let AgentEvent::TurnEnded {
        outcome: TurnOutcome::Completed {
            cost: Some(cost), ..
        },
        ..
    } = ended
    else {
        panic!("{ended:?}");
    };
    assert!(prepared_cost > 0.0);
    assert!(
        cost.turn_usd >= first + prepared_cost - 1e-9,
        "{} {} {}",
        cost.turn_usd,
        first,
        prepared_cost
    );
}

#[tokio::test]
async fn after_a_swap_the_next_turn_end_starts_no_precompute() {
    let store = MemoryStore::default();
    let provider = documented(vec![
        answer("Hello.", NEAR),
        compaction(SUMMARY),
        answer("Again.", 300),
        answer("More.", 400),
    ]);
    let mut live = Live::start(background(&provider, &store));
    assert!(completed(&live.turn("u1", "first").await));
    live.until(prepared).await;
    assert!(completed(&live.turn("u2", "second").await));
    assert!(completed(&live.turn("u3", "third").await));
    let (_, seen) = live.close().await;
    assert!(swapped_in(&seen, 2));
    assert_eq!(background_starts(&seen), 1);
    assert_eq!(provider.sent().len(), 4);
}

#[tokio::test]
async fn a_tool_loop_crossing_the_whole_band_compacts_at_the_point() {
    let store = MemoryStore::default();
    let provider = documented(vec![
        tool_call(18_000, "Looking."),
        tool_call(20_500, "Still looking."),
        compaction(SUMMARY),
        answer("Done.", 300),
    ]);
    let mut live = Live::start(background(&provider, &store));
    assert!(completed(&live.turn("u1", "first").await));
    let (_, seen) = live.close().await;
    assert_eq!(background_starts(&seen), 0);
    assert!(!seen.iter().any(is_background));
    assert_eq!(auto_starts(&seen), 1);
}

#[tokio::test]
async fn a_pending_effort_change_leaves_the_block_to_swap_at_the_turn_start() {
    let store = MemoryStore::default();
    let provider = documented(vec![
        answer("Hello.", NEAR),
        compaction(SUMMARY),
        answer("Again.", 300),
    ]);
    let mut live = Live::start(background(&provider, &store));
    assert!(completed(&live.turn("u1", "first").await));
    live.until(prepared).await;
    super::marker::change(&mut live, 1, None, Some(Effort::Low)).await;
    assert!(completed(&live.turn("u2", "second").await));
    let (agent, seen) = live.close().await;
    assert!(discarded(&seen).is_empty(), "{seen:#?}");
    assert!(swapped_in(&seen, 2));
    let swapped = provider.body(2);
    assert!(lead_of(&swapped), "{swapped}");
    assert!(
        agent
            .view_messages()
            .iter()
            .any(|m| m.effort_marker().map(|m| m.level) == Some(Effort::Low))
    );
    assert_rebuilds(&store, &agent);
}

pub(super) fn extends(provider: &Fake, earlier: usize, later: usize) {
    let a = strip_cache_control(&provider.body(earlier));
    let b = strip_cache_control(&provider.body(later));
    assert_eq!(a["system"], b["system"]);
    assert_eq!(a["tools"], b["tools"]);
    let a = a["messages"].as_array().unwrap();
    let b = b["messages"].as_array().unwrap();
    assert!(b.len() > a.len(), "{a:#?} {b:#?}");
    assert_eq!(&b[..a.len() - 1], &a[..a.len() - 1]);
    let (Some(x), Some(y)) = (
        a.last().and_then(|m| m["content"].as_array()),
        b[a.len() - 1]["content"].as_array(),
    ) else {
        panic!("{a:#?} {b:#?}");
    };
    assert_eq!(&y[..x.len()], &x[..]);
}

#[tokio::test]
async fn requests_after_a_swap_between_turns_extend_each_other() {
    let store = MemoryStore::default();
    let provider = documented(vec![
        answer("Hello.", NEAR),
        compaction(SUMMARY),
        tool_call(300, "Looking."),
        answer("Again.", 400),
        answer("More.", 500),
    ]);
    let mut live = Live::start(background(&provider, &store));
    assert!(completed(&live.turn("u1", "first").await));
    live.until(prepared).await;
    assert!(completed(&live.turn("u2", "second").await));
    assert!(completed(&live.turn("u3", "third").await));
    let (agent, _) = live.close().await;
    assert!(lead_of(&provider.body(2)));
    extends(&provider, 2, 3);
    extends(&provider, 3, 4);
    assert_rebuilds(&store, &agent);
}

#[tokio::test]
async fn requests_after_a_swap_in_a_turn_extend_each_other() {
    let store = MemoryStore::default();
    let precompute = Landing::new();
    let step = Arc::new(Semaphore::new(0));
    let provider = documented(vec![
        answer("Hello.", NEAR),
        precompute.gated(compaction(SUMMARY)),
        tool_call(NEAR + 200, "Looking.").gated(&step),
        tool_call(NEAR + 300, "Still looking."),
        answer("Done.", 400),
        answer("More.", 500),
    ]);
    let mut live = Live::start(background(&provider, &store));
    assert!(completed(&live.turn("u1", "first").await));
    live.send(send("u2", "second")).await;
    live.until(|e| matches!(e, AgentEvent::StepStarted { step: 1 }))
        .await;
    precompute.land().await;
    step.add_permits(1);
    let outcome = live
        .until(|e| matches!(e, AgentEvent::TurnEnded { .. }))
        .await;
    assert!(ended_completed(&outcome), "{outcome:?}");
    assert!(completed(&live.turn("u3", "third").await));
    let (agent, seen) = live.close().await;
    assert!(swapped_in(&seen, 2));
    assert!(lead_of(&provider.body(3)));
    extends(&provider, 3, 4);
    extends(&provider, 4, 5);
    assert_rebuilds(&store, &agent);
}

pub(super) fn continued_search(input: u64) -> Step {
    let mut script = vec![start(input, 0)];
    script.extend([
        Ok(ProviderEvent::BlockStart {
            index: 0,
            block: server_answer("srvtoolu_1"),
        }),
        Ok(ProviderEvent::BlockStop { index: 0 }),
    ]);
    script.extend(tool_use(1, "toolu_1", "bash", &["{\"command\":\"true\"}"]));
    script.extend(finish(StopReason::ToolUse, input, 0, 40));
    Step::from(script)
}

#[tokio::test]
async fn a_block_held_on_a_paused_reply_swaps_instead_of_meeting_the_point() {
    let store = MemoryStore::default();
    let precompute = Landing::new();
    let step = Arc::new(Semaphore::new(0));
    let provider = documented(vec![
        answer("Hello.", NEAR),
        precompute.gated(compaction(SUMMARY)),
        paused_search(NEAR + 100, 40).gated(&step),
        continued_search(20_500),
        answer("Done.", 300),
    ]);
    let mut live = Live::start(background(&provider, &store));
    assert!(completed(&live.turn("u1", "first").await));
    live.send(send("u2", "second")).await;
    live.until(|e| matches!(e, AgentEvent::StepStarted { step: 1 }))
        .await;
    precompute.land().await;
    step.add_permits(1);
    let outcome = live
        .until(|e| matches!(e, AgentEvent::TurnEnded { .. }))
        .await;
    assert!(ended_completed(&outcome), "{outcome:?}");
    let (agent, seen) = live.close().await;
    assert_eq!(auto_starts(&seen), 0, "{seen:#?}");
    assert!(discarded(&seen).is_empty(), "{seen:#?}");
    assert!(swapped_in(&seen, 2));
    assert!(!lead_of(&provider.body(3)));
    assert!(lead_of(&provider.body(4)));
    assert_rebuilds(&store, &agent);
}

fn discard_notes(seen: &[AgentEvent]) -> Vec<String> {
    pick!(
        seen,
        AgentEvent::CompactionFailed {
            trigger: CompactionTrigger::Background,
            kind: CompactionFailure::Discarded { .. },
            message,
            ..
        } => message.clone()
    )
}

fn discard_cost(seen: &[AgentEvent]) -> Option<Cost> {
    seen.iter().find_map(|e| match e {
        AgentEvent::CompactionFailed {
            trigger: CompactionTrigger::Background,
            kind: CompactionFailure::Discarded { .. },
            cost,
            ..
        } => Some(*cost),
        _ => None,
    })?
}

fn cut_off(input: u64) -> Step {
    Step::from(vec![
        Ok(ProviderEvent::Sending),
        start(input, 0),
        Err(classified(
            ErrorKind::Overloaded,
            "overloaded",
            FailurePhase::Response,
            RetryClass::Transient,
        )),
    ])
}

#[tokio::test]
async fn a_ready_block_discarded_as_resent_names_its_cost() {
    let store = MemoryStore::default();
    let precompute = Landing::new();
    let step = Arc::new(Semaphore::new(0));
    let provider = documented(vec![
        answer("Hello.", NEAR),
        precompute.gated(compaction(SUMMARY)),
        overloaded().gated(&step),
        answer("Again.", 300),
    ]);
    let mut live = Live::start(background(&provider, &store));
    assert!(completed(&live.turn("u1", "first").await));
    live.send(send("u2", "second")).await;
    live.until(|e| matches!(e, AgentEvent::StepStarted { step: 1 }))
        .await;
    precompute.land().await;
    step.add_permits(1);
    live.until(|e| matches!(e, AgentEvent::TurnEnded { .. }))
        .await;
    let (_, seen) = live.close().await;
    assert_eq!(
        discarded(&seen),
        [(DiscardReason::Resent.as_str().to_owned(), Some(2))]
    );
    let notes = discard_notes(&seen);
    assert!(notes[0].contains("(it cost $"), "{notes:?}");
    assert_eq!(discard_cost(&seen), None);
}

#[tokio::test]
async fn a_search_refusal_after_a_billed_attempt_reports_that_attempt() {
    let store = MemoryStore::default();
    let provider = documented(vec![answer("Hello.", NEAR), cut_off(1_200), search_off()]);
    let config = format!(
        "{POINT}\n[endpoints.env.models.\"claude-opus-5-5\"]\nweb_search = [\"web_search_20250305\"]\n"
    );
    let agent = agent_with(&provider, &store, &config, small_band()).with_web_search(true);
    let mut live = Live::start(agent);
    assert!(completed(&live.turn("u1", "first").await));
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
    let (_, seen) = live.close().await;
    assert_eq!(
        discarded(&seen),
        [(DiscardReason::Refused.as_str().to_owned(), None)]
    );
    let cost = discard_cost(&seen).unwrap_or_else(|| panic!("{seen:#?}"));
    assert!(cost.turn_usd > 0.0 && cost.turn_lower_bound, "{cost:?}");
    let notes = discard_notes(&seen);
    assert!(notes[0].contains("(it cost $"), "{notes:?}");
    let resumed = crate::resume::LoggedSession::read(&records(&store)).unwrap();
    assert!(resumed.lower_bound);
}

#[tokio::test]
async fn an_abort_while_waiting_to_retry_adds_no_lower_bound() {
    let store = MemoryStore::default();
    let counted = erring(cut_off(1_200), |mut error| {
        error.usage_complete = true;
        error
    });
    let provider = documented(vec![answer("Hello.", NEAR), counted, compaction(SUMMARY)]);
    let agent = logged(provider.clone(), &store)
        .with_model_settings(models(POINT))
        .with_start_layers(saved(small_band()))
        .with_retry_policy(RetryPolicy {
            step_retries: Some(1),
            base_delay: Duration::from_secs(30),
            ..quick()
        });
    let mut live = Live::start(agent);
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
    tokio::time::sleep(Duration::from_millis(200)).await;
    assert_eq!(provider.sent().len(), 2);
    let (_, seen) = live.close().await;
    assert_eq!(
        discarded(&seen),
        [(DiscardReason::SessionEnded.as_str().to_owned(), None)]
    );
    let cost = discard_cost(&seen).unwrap_or_else(|| panic!("{seen:#?}"));
    assert!(cost.turn_usd > 0.0 && !cost.turn_lower_bound, "{cost:?}");
}

#[tokio::test]
async fn a_finished_precompute_is_read_before_it_is_discarded() {
    let store = MemoryStore::default();
    let precompute = Landing::new();
    let step = Arc::new(Semaphore::new(0));
    let provider = documented(vec![
        answer("Hello.", NEAR),
        precompute.gated(compaction(SUMMARY)),
        answer("Again.", 300).gated(&step),
    ]);
    let mut live = Live::start(background(&provider, &store));
    assert!(completed(&live.turn("u1", "first").await));
    live.send(send("u2", "second")).await;
    live.until(|e| matches!(e, AgentEvent::StepStarted { step: 1 }))
        .await;
    precompute.land().await;
    live.send(ClientCommand::ChangeSettings {
        request: RequestId(6),
        changes: SettingChange::named("context.background_compaction", SettingValue::Bool(false))
            .into_iter()
            .collect(),
        save: false,
    })
    .await;
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
    step.add_permits(1);
    live.until(|e| matches!(e, AgentEvent::TurnEnded { .. }))
        .await;
    let (_, seen) = live.close().await;
    assert!(seen.iter().any(prepared), "{seen:#?}");
    assert_eq!(
        discarded(&seen),
        [(DiscardReason::Superseded.as_str().to_owned(), Some(2))]
    );
    assert_eq!(discard_cost(&seen), None);
    assert!(discard_notes(&seen)[0].contains("(it cost $"));
}

#[tokio::test]
async fn a_kept_swap_leaves_the_accepted_position() {
    let mut agent = ready_agent();
    agent.track().committed_steps = 1;
    let before = agent.accepted;
    let seen = at_step(&mut agent, &mut Pending::default()).await;
    assert!(discarded(&seen).is_empty(), "{seen:#?}");
    assert!(agent.compaction.head.is_some());
    assert_eq!(agent.accepted, before);
}

#[tokio::test]
async fn a_retry_applies_the_next_turn_checks_to_the_block() {
    let store = MemoryStore::default();
    let precompute = Arc::new(Semaphore::new(0));
    let provider = documented(vec![
        answer("Hello.", NEAR),
        compaction(SUMMARY).gated(&precompute),
        failing(server_error()),
        answer("Again.", 300),
    ]);
    let mut live = Live::start(background(&provider, &store));
    assert!(completed(&live.turn("u1", "first").await));
    live.until(background_started).await;
    let failed = live.turn("u2", "second").await;
    assert!(
        matches!(failed, TurnOutcome::Error { offer: Some(_), .. }),
        "{failed:?}"
    );
    precompute.add_permits(1);
    live.until(prepared).await;
    live.send(ClientCommand::ChangeSettings {
        request: RequestId(7),
        changes: SettingChange::named("context.auto_compact", SettingValue::Bool(false))
            .into_iter()
            .collect(),
        save: false,
    })
    .await;
    live.until(|e| {
        matches!(
            e,
            AgentEvent::SettingsChanged {
                answers: Some(RequestId(7)),
                ..
            }
        )
    })
    .await;
    live.send(ClientCommand::Retry {
        turn: 2,
        endpoint: None,
        request: RequestId(8),
    })
    .await;
    let outcome = live
        .until(|e| matches!(e, AgentEvent::TurnEnded { .. }))
        .await;
    assert!(ended_completed(&outcome), "{outcome:?}");
    let (_, seen) = live.close().await;
    let reasons = discarded(&seen);
    assert_eq!(reasons.len(), 1, "{reasons:?}");
    assert_eq!(reasons[0].0, DiscardReason::Superseded.as_str());
    assert!(reasons[0].1.is_some());
    assert!(!lead_of(&provider.body(3)));
}

#[tokio::test]
async fn closing_the_session_while_waiting_for_the_precompute_ends_it_as_session_ended() {
    let store = MemoryStore::default();
    let precompute = Arc::new(Semaphore::new(0));
    let provider = documented(vec![
        answer("Hello.", NEAR),
        compaction(SUMMARY).gated(&precompute),
        tool_call(20_500, "Looking."),
        answer("Done.", 300),
    ]);
    let mut live = Live::start(background(&provider, &store));
    assert!(completed(&live.turn("u1", "first").await));
    live.send(send("u2", "second")).await;
    live.until(|e| matches!(e, AgentEvent::ToolCallFinished { .. }))
        .await;
    tokio::time::sleep(Duration::from_millis(200)).await;
    let (_, seen) = live.close().await;
    let reasons = discarded(&seen);
    assert_eq!(reasons.len(), 1, "{seen:#?}");
    assert_eq!(reasons[0].0, DiscardReason::SessionEnded.as_str());
}

#[tokio::test]
async fn a_swap_after_a_failed_turn_puts_the_additions_after_its_tool_results() {
    let store = MemoryStore::default();
    let precompute = Arc::new(Semaphore::new(0));
    let provider = documented(vec![
        answer("Hello.", NEAR),
        compaction(SUMMARY).gated(&precompute),
        tool_call(300, "Looking."),
        failing(server_error()),
        answer("Again.", 400),
        answer("More.", 500),
    ]);
    let mut live = Live::start(background(&provider, &store));
    assert!(completed(&live.turn("u1", "first").await));
    live.until(background_started).await;
    let failed = live.turn("u2", "second").await;
    assert!(matches!(failed, TurnOutcome::Error { .. }), "{failed:?}");
    precompute.add_permits(1);
    live.until(prepared).await;
    assert!(completed(&live.turn("u3", "third").await));
    assert!(completed(&live.turn("u4", "fourth").await));
    let (agent, seen) = live.close().await;
    assert!(swapped_in(&seen, 3));
    let swapped = strip_cache_control(&provider.body(4));
    assert!(lead_of(&swapped), "{swapped}");
    let messages = swapped["messages"].as_array().unwrap();
    let last = messages.last().unwrap()["content"].as_array().unwrap();
    assert_eq!(last[0]["type"], "tool_result", "{swapped}");
    let context = last
        .iter()
        .position(|b| b.to_string().contains("compacted_context"))
        .unwrap();
    let typed = last
        .iter()
        .position(|b| b.to_string().contains("third"))
        .unwrap();
    assert!(0 < context && context < typed, "{swapped}");
    extends(&provider, 4, 5);
    assert_rebuilds(&store, &agent);
}

const SONNET: &str = "claude-sonnet-4-6";

fn switching(provider: &Arc<Fake>, store: &MemoryStore) -> Agent {
    let config = format!("{POINT}\n[models.\"{SONNET}\"]\nnative_compaction = true\n");
    two_endpoints(logged(provider.clone(), store), provider.clone())
        .with_model_settings(models(&config))
        .with_start_layers(saved(small_band()))
        .with_retry_policy(RetryPolicy {
            step_retries: Some(1),
            ..quick()
        })
}

async fn swapped_between_turns(live: &mut Live) {
    assert!(completed(&live.turn("u1", "first").await));
    live.until(prepared).await;
    assert!(completed(&live.turn("u2", "second").await));
}

async fn to_sonnet(live: &mut Live, request: u64) -> AgentEvent {
    live.send(ClientCommand::shortcut(
        RequestId(request),
        None,
        Some(SONNET.to_owned()),
        None,
    ))
    .await;
    live.until(|e| {
        matches!(
            e,
            AgentEvent::SettingsChanged { answers: Some(RequestId(r)), .. } if *r == request
        )
    })
    .await
}

fn kept_turns_quote(event: &AgentEvent) -> bool {
    let AgentEvent::SettingsChanged { changes, .. } = event else {
        return false;
    };
    changes.iter().any(|c| {
        matches!(
            &c.consequence,
            Some(wisp_protocol::settings::Consequence::CompactsBeforeSwitch {
                estimate_micros,
                head_tokens,
                cause: wisp_protocol::SwitchCompactionCause::KeptTurns,
            }) if *estimate_micros > 0 && *head_tokens > 0
        )
    })
}

fn kept_turns_started(seen: &[AgentEvent]) -> bool {
    seen.iter().any(|e| {
        matches!(
            e,
            AgentEvent::CompactionStarted {
                trigger: CompactionTrigger::Switch,
                switch: Some(wisp_protocol::SwitchCompaction {
                    cause: wisp_protocol::SwitchCompactionCause::KeptTurns,
                    ..
                }),
                ..
            }
        )
    })
}

fn switch_cause(seen: &[AgentEvent]) -> Option<wisp_protocol::SwitchCompactionCause> {
    seen.iter().find_map(|e| match e {
        AgentEvent::CompactionStarted {
            trigger: CompactionTrigger::Switch,
            switch: Some(switch),
            ..
        } => Some(switch.cause.clone()),
        _ => None,
    })
}

#[tokio::test]
async fn a_smaller_window_wins_the_cause_over_kept_turns() {
    let store = MemoryStore::default();
    let provider = documented(vec![
        answer("Hello.", NEAR),
        compaction(SUMMARY),
        answer("Again.", 300),
        compaction(SUMMARY),
        answer("On Sonnet.", 400),
    ]);
    let config = format!(
        "{POINT}\n[models.\"{SONNET}\"]\nnative_compaction = true\ncontext_window = 200000\ncompact_at = 100\n"
    );
    let agent = switching(&provider, &store).with_model_settings(models(&config));
    let mut live = Live::start(agent);
    swapped_between_turns(&mut live).await;
    let changed = to_sonnet(&mut live, 7).await;
    assert!(!kept_turns_quote(&changed), "{changed:#?}");
    assert!(completed(&live.turn("u3", "third").await));
    let (agent, seen) = live.close().await;
    assert_eq!(
        switch_cause(&seen),
        Some(wisp_protocol::SwitchCompactionCause::SmallerWindow)
    );
    assert!(agent.compaction.head.as_ref().is_some_and(|h| !h.leading()));
    assert_rebuilds(&store, &agent);
}

#[tokio::test]
async fn a_model_switch_after_a_swap_compacts_the_kept_turns_on_the_old_model_first() {
    let store = MemoryStore::default();
    let provider = documented(vec![
        answer("Hello.", NEAR),
        compaction(SUMMARY),
        answer("Again.", 300),
        compaction(SUMMARY),
        answer("On Sonnet.", 400),
    ]);
    let mut live = Live::start(switching(&provider, &store));
    swapped_between_turns(&mut live).await;
    let changed = to_sonnet(&mut live, 7).await;
    assert!(kept_turns_quote(&changed), "{changed:#?}");
    assert!(completed(&live.turn("u3", "third").await));
    let (agent, seen) = live.close().await;
    assert!(kept_turns_started(&seen), "{seen:#?}");
    let compacting = provider.body(3);
    assert_eq!(compacting["model"], "claude-opus-5-5");
    assert!(lead_of(&compacting), "{compacting}");
    let first = provider.body(4);
    assert_eq!(first["model"], SONNET);
    assert!(!lead_of(&first), "{first}");
    assert_eq!(first["messages"][0]["role"], "user");
    assert!(!first.to_string().contains("Again."), "{first}");
    assert!(agent.compaction.head.as_ref().is_some_and(|h| !h.leading()));
    assert_rebuilds(&store, &agent);
}

#[tokio::test]
async fn cancelling_the_kept_turns_compaction_withdraws_the_switch() {
    let store = MemoryStore::default();
    let provider = documented(vec![
        answer("Hello.", NEAR),
        compaction(SUMMARY),
        answer("Again.", 300),
        Step::hang(),
    ]);
    let mut live = Live::start(switching(&provider, &store));
    swapped_between_turns(&mut live).await;
    to_sonnet(&mut live, 7).await;
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
    live.until(|e| super::quote::withdrawal(std::slice::from_ref(e)).is_some())
        .await;
    let (agent, seen) = live.close().await;
    assert!(kept_turns_started(&seen));
    assert_eq!(agent.session.model, "claude-opus-5-5");
    assert!(agent.compaction.head.as_ref().is_some_and(|h| h.leading()));
}

#[tokio::test]
async fn a_failed_kept_turns_compaction_converts_before_the_switch() {
    let store = MemoryStore::default();
    let provider = documented(vec![
        answer("Hello.", NEAR),
        compaction(SUMMARY),
        answer("Again.", 300),
        failing(server_error()),
        answer("On Sonnet.", 400),
    ]);
    let mut live = Live::start(switching(&provider, &store));
    swapped_between_turns(&mut live).await;
    to_sonnet(&mut live, 7).await;
    assert!(completed(&live.turn("u3", "third").await));
    let (agent, seen) = live.close().await;
    assert!(kept_turns_started(&seen));
    assert!(seen.iter().any(|e| matches!(
        e,
        AgentEvent::Compacted {
            converted: true,
            ..
        }
    )));
    let first = provider.body(4);
    assert_eq!(first["model"], SONNET);
    assert!(!lead_of(&first), "{first}");
    assert!(!first.to_string().contains("\"compaction\""), "{first}");
    assert!(agent.compaction.head.as_ref().is_some_and(|h| !h.leading()));
    assert_rebuilds(&store, &agent);
}

#[tokio::test]
async fn an_endpoint_switch_that_keeps_the_model_keeps_the_turns() {
    let store = MemoryStore::default();
    let provider = documented(vec![
        answer("Hello.", NEAR),
        compaction(SUMMARY),
        answer("Again.", 300),
        answer("Elsewhere.", 400),
    ]);
    let mut live = Live::start(switching(&provider, &store));
    swapped_between_turns(&mut live).await;
    super::marker::change(&mut live, 7, Some("other"), None).await;
    assert!(completed(&live.turn("u3", "third").await));
    let (agent, seen) = live.close().await;
    assert!(!kept_turns_started(&seen));
    assert!(
        !seen
            .iter()
            .any(|e| matches!(e, AgentEvent::CompactionStarted { .. } if !is_background(e)))
    );
    assert!(lead_of(&provider.body(3)));
    assert!(agent.compaction.head.as_ref().is_some_and(|h| h.leading()));
}

fn resumed_background(provider: &Arc<Fake>, records: &[wisp_protocol::log::LogRecord]) -> Agent {
    let (_, log) = continued(records);
    Agent::new(provider.clone(), default_session())
        .with_clock(Arc::new(crate::clock::StillClock))
        .with_log(log, log_context())
        .with_model_settings(models(POINT))
        .with_start_layers(saved(small_band()))
        .resume(records, 0)
        .unwrap()
}

async fn resumes_with_the_same_bytes(
    live: Live,
    provider: &Arc<Fake>,
    store: &MemoryStore,
    turn: &str,
) {
    let records = records(store);
    let mut live = live;
    assert!(completed(&live.turn(turn, "next").await));
    live.close().await;
    let again = documented(vec![answer("Next.", 900)]);
    let mut resumed = Live::start(resumed_background(&again, &records));
    assert!(completed(&resumed.turn(turn, "next").await));
    let (agent, _) = resumed.close().await;
    assert!(agent.compaction.head.as_ref().is_some_and(|h| h.leading()));
    let last = provider.sent().len() - 1;
    assert_eq!(again.body(0), provider.body(last));
}

#[tokio::test]
async fn a_session_with_kept_turns_resumes_with_the_same_bytes() {
    let store = MemoryStore::default();
    let provider = documented(vec![
        answer("Hello.", NEAR),
        compaction(SUMMARY),
        answer("Again.", 300),
        answer("Next.", 900),
    ]);
    let mut live =
        Live::start(background(&provider, &store).with_clock(Arc::new(crate::clock::StillClock)));
    swapped_between_turns(&mut live).await;
    resumes_with_the_same_bytes(live, &provider, &store, "u3").await;
}

#[tokio::test]
async fn a_head_joined_to_tool_results_resumes_with_the_same_bytes() {
    let store = MemoryStore::default();
    let precompute = Arc::new(Semaphore::new(0));
    let provider = documented(vec![
        answer("Hello.", NEAR),
        compaction(SUMMARY).gated(&precompute),
        tool_call(300, "Looking."),
        failing(server_error()),
        answer("Again.", 400),
        answer("Next.", 900),
    ]);
    let mut live =
        Live::start(background(&provider, &store).with_clock(Arc::new(crate::clock::StillClock)));
    assert!(completed(&live.turn("u1", "first").await));
    live.until(background_started).await;
    let failed = live.turn("u2", "second").await;
    assert!(matches!(failed, TurnOutcome::Error { .. }), "{failed:?}");
    precompute.add_permits(1);
    live.until(prepared).await;
    assert!(completed(&live.turn("u3", "third").await));
    let joined = rebuild_history(log_events(&store).iter(), no_images)
        .unwrap()
        .head
        .and_then(|h| h.join);
    assert_eq!(joined, Some(1));
    resumes_with_the_same_bytes(live, &provider, &store, "u4").await;
}

#[tokio::test]
async fn a_swap_after_a_reply_without_calls_puts_the_additions_in_the_steer_after_the_marker() {
    let store = MemoryStore::default();
    let precompute = Landing::new();
    let step = Arc::new(Semaphore::new(0));
    let provider = documented(vec![
        tool_call(300, "Looking."),
        answer("Hello.", NEAR),
        precompute.gated(compaction(SUMMARY)),
        answer("Partial.", NEAR + 100).gated(&step),
        answer("Done.", 400),
    ]);
    let mut live = Live::start(background(&provider, &store));
    super::marker::change(&mut live, 1, None, Some(Effort::Low)).await;
    assert!(completed(&live.turn("u1", "first").await));
    live.send(send("u2", "second")).await;
    live.until(|e| matches!(e, AgentEvent::StepStarted { step: 1 }))
        .await;
    precompute.land().await;
    live.send(send("u3", "and this")).await;
    live.until(|e| matches!(e, AgentEvent::InputAccepted { id, .. } if id.0 == "u3"))
        .await;
    step.add_permits(1);
    let outcome = live
        .until(|e| matches!(e, AgentEvent::TurnEnded { .. }))
        .await;
    assert!(ended_completed(&outcome), "{outcome:?}");
    let (agent, seen) = live.close().await;
    assert!(swapped_in(&seen, 2), "{seen:#?}");
    let head = agent.compaction.head.clone().unwrap();
    let view = agent.view_messages();
    assert_eq!(view.len(), agent.history.len() - head.cut + 1);
    let marker = view
        .iter()
        .rposition(|m| m.effort_marker().is_some())
        .unwrap_or_else(|| panic!("{view:#?}"));
    assert!(
        view[marker - 1]
            .content
            .iter()
            .any(|b| matches!(b, ContentBlock::Text { text } if text == "Partial."))
    );
    let steer = &view[marker + 1];
    assert_eq!(steer.role, Role::User);
    let texts: Vec<&str> = pick!(&steer.content, ContentBlock::Text { text } => text.as_str());
    let context = texts
        .iter()
        .position(|t| t.contains("compacted_context"))
        .unwrap_or_else(|| panic!("{texts:?}"));
    let typed = texts
        .iter()
        .position(|t| t.contains("and this"))
        .unwrap_or_else(|| panic!("{texts:?}"));
    assert!(context < typed);
    assert!(lead_of(&provider.body(4)));
    assert_rebuilds(&store, &agent);
}

#[tokio::test]
async fn a_resumed_rewind_inside_a_joined_epoch_sends_what_live_sends() {
    let store = MemoryStore::default();
    let precompute = Arc::new(Semaphore::new(0));
    let provider = documented(vec![
        answer("Hello.", NEAR),
        compaction(SUMMARY).gated(&precompute),
        tool_call(300, "Looking."),
        failing(server_error()),
        answer("Again.", 400),
        answer("Next.", 500),
        answer("Again b.", 600),
    ]);
    let mut live =
        Live::start(background(&provider, &store).with_clock(Arc::new(crate::clock::StillClock)));
    assert!(completed(&live.turn("u1", "first").await));
    tokio::time::sleep(Duration::from_millis(100)).await;
    let failed = live.turn("u2", "second").await;
    assert!(matches!(failed, TurnOutcome::Error { .. }), "{failed:?}");
    precompute.add_permits(1);
    live.until(prepared).await;
    assert!(completed(&live.turn("u3", "third").await));
    assert!(completed(&live.turn("u4", "fourth").await));
    let records = records(&store);
    live.send(ClientCommand::Rewind {
        to: wisp_protocol::RewindPoint::Turn { turn: 4 },
        request: RequestId(7),
    })
    .await;
    live.until(|e| matches!(e, AgentEvent::Rewound { .. } | AgentEvent::Rejected { .. }))
        .await;
    assert!(completed(&live.turn("u5", "fourth b").await));
    live.close().await;
    let last = provider.sent().len() - 1;

    let again = documented(vec![answer("Again b.", 600)]);
    let mut back = Live::start(resumed_background(&again, &records));
    back.send(ClientCommand::Rewind {
        to: wisp_protocol::RewindPoint::Turn { turn: 4 },
        request: RequestId(7),
    })
    .await;
    let done = back
        .until(|e| matches!(e, AgentEvent::Rewound { .. } | AgentEvent::Rejected { .. }))
        .await;
    assert!(matches!(done, AgentEvent::Rewound { .. }), "{done:?}");
    assert!(completed(&back.turn("u5", "fourth b").await));
    back.close().await;
    assert_eq!(again.body(0), provider.body(last));
}

async fn swap_for(agent: &mut Agent, limit: u64, pending: &Pending) -> (bool, Vec<AgentEvent>) {
    let (sink, mut events) = mpsc::channel(64);
    let (_commands_tx, mut commands) = mpsc::channel(8);
    let swapped = agent
        .swap_for_overflow(limit, pending, &mut commands, &sink)
        .await;
    drop(sink);
    let mut seen = Vec::new();
    while let Some(event) = events.recv().await {
        seen.push(event);
    }
    (swapped, seen)
}

#[tokio::test]
async fn an_overflow_swaps_a_ready_block_only_when_it_fits_under_the_limit() {
    let mut agent = ready_agent();
    agent.track().committed_steps = 1;
    let (swapped, seen) = swap_for(&mut agent, 1_000, &Pending::default()).await;
    assert!(!swapped);
    assert_eq!(discarded(&seen), [("superseded".to_owned(), Some(2))]);
    assert!(agent.compaction.head.is_none());
    let mut agent = ready_agent();
    agent.track().committed_steps = 1;
    let (swapped, seen) = swap_for(&mut agent, 1_000_000, &Pending::default()).await;
    assert!(swapped);
    assert!(discarded(&seen).is_empty());
    assert!(agent.compaction.head.as_ref().is_some_and(Head::leading));
}

#[tokio::test]
async fn an_overflow_discards_a_ready_block_over_the_bound_or_with_the_unsaved_message() {
    let mut agent = ready_agent();
    agent.track().committed_steps = 1;
    agent.history[1] = Message {
        role: Role::Assistant,
        content: vec![ContentBlock::Text {
            text: "x".repeat(400_000),
        }],
    };
    let (swapped, seen) = swap_for(&mut agent, 1_000, &Pending::default()).await;
    assert!(!swapped);
    assert_eq!(discarded(&seen), [("bound".to_owned(), Some(2))]);
    let mut agent = ready_agent();
    let pending = Pending {
        user: Some(vec![ContentBlock::Text {
            text: "second".into(),
        }]),
        ..Pending::default()
    };
    let (swapped, seen) = swap_for(&mut agent, 1_000_000, &pending).await;
    assert!(!swapped);
    assert_eq!(discarded(&seen), [("unsaved".to_owned(), Some(2))]);
}

#[tokio::test]
async fn an_overflow_counts_the_pending_content_against_a_ready_block() {
    let agent = ready_agent();
    assert!(agent.swap_fits(1_000_000, 0));
    assert!(!agent.swap_fits(1_000_000, 1_000_000));
    let mut agent = ready_agent();
    agent.track().committed_steps = 1;
    let pending = Pending {
        user: Some(vec![ContentBlock::Text {
            text: "x".repeat(4_000_000),
        }]),
        ..Pending::default()
    };
    let (swapped, seen) = swap_for(&mut agent, 1_000_000, &pending).await;
    assert!(!swapped);
    assert_eq!(discarded(&seen), [("superseded".to_owned(), Some(2))]);
}

#[tokio::test]
async fn an_overflow_with_a_paused_tail_supersedes_a_ready_block() {
    let mut agent = ready_agent();
    agent.track().committed_steps = 1;
    let last = agent.history.len() - 1;
    assert_eq!(agent.history[last].role, Role::Assistant);
    agent.search.paused.insert(last);
    assert!(agent.paused_tail());
    let (swapped, seen) = swap_for(&mut agent, 1_000_000, &Pending::default()).await;
    assert!(!swapped);
    assert_eq!(discarded(&seen), [("superseded".to_owned(), Some(2))]);
    assert!(agent.compaction.head.is_none());
}

#[tokio::test]
async fn a_mid_turn_overflow_aborts_a_running_precompute_as_superseded() {
    let store = MemoryStore::default();
    let precompute = Arc::new(Semaphore::new(0));
    let step = Arc::new(Semaphore::new(0));
    let provider = documented(vec![
        answer("Hello.", NEAR),
        compaction(SUMMARY).gated(&precompute),
        tool_call(NEAR + 100, "Looking."),
        overflowing(&step),
    ]);
    let mut live = Live::start(background(&provider, &store));
    assert!(completed(&live.turn("u1", "first").await));
    live.send(send("u2", "second")).await;
    live.until(|e| matches!(e, AgentEvent::StepStarted { step: 2 }))
        .await;
    step.add_permits(1);
    let ended = live
        .until(|e| matches!(e, AgentEvent::TurnEnded { .. }))
        .await;
    precompute.add_permits(1);
    assert!(overflowed(&ended), "{ended:?}");
    let (agent, seen) = live.close().await;
    assert_eq!(
        discarded(&seen),
        [(DiscardReason::Superseded.as_str().to_owned(), Some(2))]
    );
    assert_eq!(overflow_compactions(&seen), 1);
    assert_rebuilds(&store, &agent);
}

pub(super) const WIDE_POINT: &str = "[models.\"claude-opus-5-5\"]\ncompact_at = 200000\n";
pub(super) const WIDE_NEAR: u64 = 199_500;

pub(super) fn overflowing(gate: &Arc<Semaphore>) -> Step {
    Step::from(vec![Err(ProviderError::new(
        ErrorKind::ContextOverflow,
        "prompt is too long",
    ))])
    .gated(gate)
}

pub(super) async fn overflow_at_step_two(
    live: &mut Live,
    precompute: &Arc<Semaphore>,
    step: &Arc<Semaphore>,
) -> AgentEvent {
    assert!(completed(&live.turn("u1", "first").await));
    live.send(send("u2", "second")).await;
    live.until(|e| matches!(e, AgentEvent::StepStarted { step: 2 }))
        .await;
    precompute.add_permits(1);
    tokio::time::sleep(Duration::from_millis(200)).await;
    step.add_permits(1);
    live.until(|e| matches!(e, AgentEvent::TurnEnded { .. }))
        .await
}

fn overflow_compactions(seen: &[AgentEvent]) -> usize {
    seen.iter()
        .filter(|e| {
            matches!(
                e,
                AgentEvent::CompactionStarted {
                    trigger: CompactionTrigger::Overflow,
                    ..
                }
            )
        })
        .count()
}

#[tokio::test]
async fn a_mid_turn_overflow_swaps_a_ready_block_that_fits_and_sends_the_step_again() {
    let store = MemoryStore::default();
    let precompute = Arc::new(Semaphore::new(0));
    let step = Arc::new(Semaphore::new(0));
    let provider = documented(vec![
        answer("Hello.", WIDE_NEAR),
        compaction(SUMMARY).gated(&precompute),
        tool_call(WIDE_NEAR + 100, "Looking."),
        overflowing(&step),
        answer("Done.", 900),
    ]);
    let agent = agent_with(&provider, &store, WIDE_POINT, small_band());
    let mut live = Live::start(agent);
    let ended = overflow_at_step_two(&mut live, &precompute, &step).await;
    assert!(ended_completed(&ended), "{ended:?}");
    let (agent, seen) = live.close().await;
    assert!(swapped_in(&seen, 2), "{seen:#?}");
    assert!(discarded(&seen).is_empty(), "{seen:#?}");
    assert_eq!(overflow_compactions(&seen), 0);
    assert_eq!(provider.sent().len(), 5);
    let resent = provider.body(4);
    assert!(lead_of(&resent), "{resent}");
    assert!(!resent.to_string().contains(CONTINUE_NOTE));
    assert_rebuilds(&store, &agent);
}

#[tokio::test]
async fn a_second_overflow_after_a_swap_ends_the_turn() {
    let store = MemoryStore::default();
    let precompute = Arc::new(Semaphore::new(0));
    let step = Arc::new(Semaphore::new(0));
    let provider = documented(vec![
        answer("Hello.", WIDE_NEAR),
        compaction(SUMMARY).gated(&precompute),
        tool_call(WIDE_NEAR + 100, "Looking."),
        overflowing(&step),
        failing(ProviderError::new(
            ErrorKind::ContextOverflow,
            "prompt is too long",
        )),
    ]);
    let agent = agent_with(&provider, &store, WIDE_POINT, small_band());
    let mut live = Live::start(agent);
    let ended = overflow_at_step_two(&mut live, &precompute, &step).await;
    let (_, seen) = live.close().await;
    assert!(
        matches!(
            ended,
            AgentEvent::TurnEnded {
                outcome: TurnOutcome::Error {
                    kind: ErrorKind::ContextOverflow,
                    ..
                },
                ..
            }
        ),
        "{ended:?}"
    );
    assert!(swapped_in(&seen, 2));
    assert_eq!(overflow_compactions(&seen), 0);
    assert_eq!(provider.sent().len(), 5);
}

#[tokio::test]
async fn a_mid_turn_overflow_with_a_block_that_wont_fit_discards_it_and_compacts() {
    let store = MemoryStore::default();
    let precompute = Arc::new(Semaphore::new(0));
    let step = Arc::new(Semaphore::new(0));
    let provider = documented(vec![
        answer("Hello.", NEAR),
        compaction(SUMMARY).gated(&precompute),
        tool_call(NEAR + 100, "Looking."),
        overflowing(&step),
    ]);
    let mut live = Live::start(background(&provider, &store));
    let ended = overflow_at_step_two(&mut live, &precompute, &step).await;
    assert!(overflowed(&ended), "{ended:?}");
    let (agent, seen) = live.close().await;
    assert_eq!(
        discarded(&seen),
        [(DiscardReason::Superseded.as_str().to_owned(), Some(2))]
    );
    assert!(!swapped_in(&seen, 2));
    assert_eq!(overflow_compactions(&seen), 1);
    assert_eq!(provider.sent().len(), 4);
    assert!(agent.compaction.head.is_none());
    assert_rebuilds(&store, &agent);
}

#[tokio::test]
async fn an_overflow_on_the_unsaved_message_discards_a_ready_block_as_unsaved() {
    let store = MemoryStore::default();
    let precompute = Arc::new(Semaphore::new(0));
    let step = Arc::new(Semaphore::new(0));
    let provider = documented(vec![
        answer("Hello.", WIDE_NEAR),
        compaction(SUMMARY).gated(&precompute),
        overflowing(&step),
    ]);
    let agent = agent_with(&provider, &store, WIDE_POINT, small_band());
    let mut live = Live::start(agent);
    assert!(completed(&live.turn("u1", "first").await));
    live.send(send("u2", "second")).await;
    live.until(|e| matches!(e, AgentEvent::StepStarted { step: 1 }))
        .await;
    precompute.add_permits(1);
    tokio::time::sleep(Duration::from_millis(200)).await;
    step.add_permits(1);
    let ended = live
        .until(|e| matches!(e, AgentEvent::TurnEnded { .. }))
        .await;
    assert!(overflowed(&ended), "{ended:?}");
    let (agent, seen) = live.close().await;
    assert_eq!(
        discarded(&seen),
        [(DiscardReason::Unsaved.as_str().to_owned(), Some(2))]
    );
    assert_eq!(overflow_compactions(&seen), 1);
    assert_rebuilds(&store, &agent);
}

fn overflowed(event: &AgentEvent) -> bool {
    matches!(
        event,
        AgentEvent::TurnEnded {
            outcome: TurnOutcome::Error {
                kind: ErrorKind::ContextOverflow,
                ..
            },
            ..
        }
    )
}

pub(super) fn window_stop(input: u64, words: &str) -> Step {
    let mut script = vec![start(input, 0)];
    script.extend(text(0, words));
    script.extend(finish(StopReason::ContextWindowExceeded, input, 0, 50));
    Step::from(script)
}

const BAND_POINT: &str = "[models.\"claude-opus-5-5\"]\ncompact_at = 40000\n";

pub(super) fn wide_band() -> Vec<(Place, &'static str, SettingValue)> {
    vec![(
        Place::Global,
        "context.background_tokens",
        SettingValue::Tokens(30_000),
    )]
}

async fn cut_off_then_ready(live: &mut Live, precompute: &Arc<Semaphore>) -> TurnOutcome {
    assert!(completed(&live.turn("u1", "first").await));
    live.turn("u2", "second").await;
    precompute.add_permits(1);
    live.until(prepared).await;
    live.turn("u3", "third").await
}

#[tokio::test]
async fn a_cut_off_reply_then_a_ready_block_that_fits_swaps_instead_of_compacting() {
    let store = MemoryStore::default();
    let precompute = Arc::new(Semaphore::new(0));
    let provider = documented(vec![
        answer("One.", 37_000),
        compaction(SUMMARY).gated(&precompute),
        window_stop(38_000, &"word ".repeat(30_000)),
        answer("Three.", 900),
    ]);
    let mut live = Live::start(agent_with(&provider, &store, BAND_POINT, wide_band()));
    let outcome = cut_off_then_ready(&mut live, &precompute).await;
    let (agent, seen) = live.close().await;
    assert!(completed(&outcome), "{outcome:?}");
    assert!(swapped_in(&seen, 3), "{seen:#?}");
    assert_eq!(overflow_compactions(&seen), 0);
    assert!(lead_of(&provider.body(3)));
    assert!(!agent.compaction.overflow.due);
    assert_rebuilds(&store, &agent);
}

#[tokio::test]
async fn a_cut_off_reply_then_a_ready_block_that_wont_fit_discards_it_and_compacts() {
    let store = MemoryStore::default();
    let precompute = Arc::new(Semaphore::new(0));
    let provider = documented(vec![
        answer("One.", 37_000),
        compaction(SUMMARY).gated(&precompute),
        window_stop(38_000, &"word ".repeat(30_000)),
        compaction(SUMMARY),
        answer("Three.", 900),
    ]);
    let config = "[models.\"claude-opus-5-5\"]\ncompact_at = 40000\ncontext_window = 104000\n";
    let mut live = Live::start(agent_with(&provider, &store, config, wide_band()));
    let outcome = cut_off_then_ready(&mut live, &precompute).await;
    let (agent, seen) = live.close().await;
    assert!(completed(&outcome), "{outcome:?}");
    assert_eq!(
        discarded(&seen),
        [(DiscardReason::Superseded.as_str().to_owned(), Some(3))]
    );
    assert_eq!(overflow_compactions(&seen), 1);
    assert!(!lead_of(&provider.body(4)));
    assert_eq!(provider.sent().len(), 5);
    assert_rebuilds(&store, &agent);
}

#[tokio::test]
async fn an_overflow_compaction_aborts_a_running_precompute_as_superseded() {
    let store = MemoryStore::default();
    let precompute = Arc::new(Semaphore::new(0));
    let provider = documented(vec![
        answer("One.", 37_000),
        compaction(SUMMARY).gated(&precompute),
        window_stop(38_000, &"word ".repeat(30_000)),
        compaction(SUMMARY),
        answer("Three.", 900),
    ]);
    let config = "[models.\"claude-opus-5-5\"]\ncompact_at = 40000\ncontext_window = 104000\n";
    let mut live = Live::start(agent_with(&provider, &store, config, wide_band()));
    assert!(completed(&live.turn("u1", "first").await));
    live.turn("u2", "second").await;
    let third = live.turn("u3", "third").await;
    precompute.add_permits(1);
    let (agent, seen) = live.close().await;
    assert!(completed(&third), "{third:?}");
    assert_eq!(
        discarded(&seen),
        [(DiscardReason::Superseded.as_str().to_owned(), Some(3))]
    );
    assert_eq!(overflow_compactions(&seen), 1);
    let aborted = seen.iter().position(|e| {
        matches!(
            e,
            AgentEvent::CompactionFailed {
                trigger: CompactionTrigger::Background,
                ..
            }
        )
    });
    let started = seen.iter().position(|e| {
        matches!(
            e,
            AgentEvent::CompactionStarted {
                trigger: CompactionTrigger::Overflow,
                ..
            }
        )
    });
    assert!(
        aborted.is_some() && aborted < started,
        "{aborted:?} {started:?}"
    );
    assert_rebuilds(&store, &agent);
}

#[tokio::test]
async fn a_retrys_smaller_window_fallback_supersedes_a_prepared_summary_first() {
    let store = MemoryStore::default();
    let precompute = Arc::new(Semaphore::new(0));
    let provider = documented(vec![
        answer("Hello.", 150_000),
        answer("Second.", 300_000),
        compaction(SUMMARY).gated(&precompute),
        failing(server_error()),
        compaction(SUMMARY),
        answer("Retried.", 3_000),
    ]);
    let config = "[models.\"claude-opus-5-5\"]\ncontext_window = 1000000\ncompact_at = 320000\n\n[endpoints.other.models.\"claude-opus-5-5\"]\ncontext_window = 200000\n";
    let agent = two_endpoints(logged(provider.clone(), &store), provider.clone())
        .with_model_settings(models(config))
        .with_start_layers(saved(wide_band()));
    let mut live = Live::start(agent);
    assert!(completed(&live.turn("u1", "first").await));
    assert!(completed(&live.turn("u2", "second").await));
    let failed = live.turn("u3", "third").await;
    assert!(
        matches!(failed, TurnOutcome::Error { offer: Some(_), .. }),
        "{failed:?}"
    );
    precompute.add_permits(1);
    live.until(prepared).await;
    live.send(ClientCommand::Retry {
        turn: 3,
        endpoint: Some("other".into()),
        request: RequestId(4),
    })
    .await;
    let ended = live
        .until(|e| matches!(e, AgentEvent::TurnEnded { .. }))
        .await;
    let (agent, seen) = live.close().await;
    assert!(ended_completed(&ended), "{ended:?}");
    assert_eq!(
        discarded(&seen),
        [(DiscardReason::Superseded.as_str().to_owned(), Some(4))]
    );
    let aborted = seen.iter().position(|e| {
        matches!(
            e,
            AgentEvent::CompactionFailed {
                trigger: CompactionTrigger::Background,
                ..
            }
        )
    });
    let started = seen.iter().position(|e| {
        matches!(
            e,
            AgentEvent::CompactionStarted {
                trigger: CompactionTrigger::Overflow,
                ..
            }
        )
    });
    assert!(
        aborted.is_some() && aborted < started,
        "{aborted:?} {started:?}"
    );
    assert_eq!(agent.endpoint_name(), "other");
    assert_rebuilds(&store, &agent);
}

#[tokio::test(start_paused = true)]
async fn a_ready_summary_is_never_refreshed_while_it_waits() {
    let store = MemoryStore::default();
    let provider = documented(vec![answer("Hello.", NEAR), compaction(SUMMARY)]);
    let mut live = Live::start(background(&provider, &store).with_keep_warm(8));
    assert!(completed(&live.turn("u1", "first").await));
    live.until(prepared).await;
    tokio::time::sleep(Duration::from_secs(4 * 3_600)).await;
    live.close().await;
    assert_eq!(provider.sent().len(), 2, "no refresh is sent");
    let skipped: Vec<(wisp_protocol::RefreshOutcome, Option<String>)> = pick!(
        log_events(&store),
        LogEvent::CacheRefresh {
            outcome, detail, ..
        } => (outcome, detail)
    );
    assert_eq!(
        skipped,
        [(
            wisp_protocol::RefreshOutcome::Skipped,
            Some("changed".into())
        )]
    );

    let store = MemoryStore::default();
    let provider = documented(vec![answer("Hello.", NEAR), answer("Warm.", NEAR)]);
    let plain = agent_with(&provider, &store, POINT_NO_NATIVE, small_band()).with_keep_warm(8);
    let mut live = Live::start(plain);
    assert!(completed(&live.turn("u1", "first").await));
    tokio::time::sleep(Duration::from_secs(56 * 60)).await;
    live.close().await;
    assert_eq!(provider.sent().len(), 2);
    assert_eq!(
        provider.body(1)["max_tokens"],
        1,
        "without a summary it refreshes"
    );
}

#[test]
fn a_prepared_summary_logs_its_estimate_and_model() {
    crate::stack_alarm::on_alarm_stack(crate::stack_alarm::Timer::Real, || async {
        for case in crate::agent::tests::COST_CASES {
            let store = MemoryStore::default();
            let summary = super::usage::billed_step(compaction(SUMMARY), case.0);
            let provider = documented(vec![answer("Hello.", NEAR), summary]);
            let agent = crate::agent::tests::priced_as(background(&provider, &store), case.1);
            let mut live = Live::start(agent);
            assert!(completed(&live.turn("u1", "first").await));
            live.until(prepared).await;
            live.close().await;
            let prepared: Vec<LogEvent> = log_events(&store)
                .into_iter()
                .filter(|e| matches!(e, LogEvent::CompactionPrepared { .. }))
                .collect();
            crate::agent::tests::assert_logged_costs(&prepared, case, &["compaction_prepared"]);
        }
    });
}
