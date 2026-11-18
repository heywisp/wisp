use wisp_protocol::log::{LoggedBlock, LoggedMessage};
use wisp_protocol::{CONTINUE_NOTE, Effort, Message, Role};

use wisp_protocol::{RetryClass, SearchEnd, UnfinishedReason};

use super::marker::change;
use super::*;

fn texts(message: &Message) -> String {
    pick!(&message.content, ContentBlock::Text { text } => text.as_str()).join("|")
}

fn logged_texts(message: &LoggedMessage) -> String {
    pick!(&message.content, LoggedBlock::Inline(ContentBlock::Text { text }) => text.as_str())
        .join("|")
}

fn after_convert(store: &MemoryStore) -> Vec<String> {
    let records = log_events(store);
    let at = records
        .iter()
        .position(|e| {
            matches!(
                e,
                LogEvent::ContextCompacted {
                    trigger: CompactionTrigger::Convert,
                    ..
                }
            )
        })
        .unwrap();
    records[at..]
        .iter()
        .filter_map(|e| match e {
            LogEvent::ContextCompacted { turn, partial, .. } => {
                Some(format!("compacted {turn:?} partial {partial}"))
            }
            LogEvent::MessageCommitted { message, .. } if message.effort_marker().is_some() => {
                Some("marker".to_owned())
            }
            LogEvent::MessageCommitted { message, .. } => {
                Some(format!("{:?} {}", message.role, logged_texts(message)))
            }
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

async fn steered_turn(live: &mut Live, gate: &Arc<Semaphore>) -> AgentEvent {
    live.send(send("u2", "second")).await;
    live.until(|e| matches!(e, AgentEvent::StepStarted { step: 1 }))
        .await;
    live.send(send("u3", "and this")).await;
    live.until(|e| matches!(e, AgentEvent::InputAccepted { .. }))
        .await;
    gate.add_permits(1);
    live.until(|e| matches!(e, AgentEvent::TurnEnded { .. }))
        .await
}

fn ended_completed(ended: &AgentEvent) -> bool {
    matches!(
        ended,
        AgentEvent::TurnEnded {
            outcome: TurnOutcome::Completed { .. },
            ..
        }
    )
}

fn fell_back(seen: &[AgentEvent]) -> bool {
    seen.iter().any(|e| {
        matches!(
            e,
            AgentEvent::FeatureStateChanged {
                feature: wisp_protocol::Feature::NativeCompaction,
                state: FeatureState::FallenBack,
                ..
            }
        )
    })
}

fn last_text(content: &Value) -> Value {
    content.as_array().unwrap().last().unwrap()["text"].clone()
}

#[tokio::test]
async fn a_block_refused_after_the_message_was_kept_sends_the_head_the_note_and_the_steer() {
    let store = MemoryStore::default();
    let gate = Arc::new(Semaphore::new(0));
    let provider = scripted(vec![
        answer("Hello.", 1_000),
        compaction(SUMMARY),
        answer("Two.", 300).gated(&gate),
        block_refused(),
        answer("Three.", 400),
        answer("Four.", 500),
    ]);
    let mut live = Live::start(logged(provider.clone(), &store));
    live.turn("u1", "first").await;
    live.compact(2, None).await;
    let ended = steered_turn(&mut live, &gate).await;
    assert!(completed(&live.turn("u4", "fourth").await));
    let (agent, seen) = live.close().await;
    assert!(ended_completed(&ended), "{ended:?}");
    assert!(!fell_back(&seen));
    let bodies: Vec<Value> = (0..6)
        .map(|n| strip_cache_control(&provider.body(n)))
        .collect();
    for body in &bodies[1..] {
        assert_eq!(body["system"], bodies[0]["system"]);
        assert_eq!(body.get("tools"), bodies[0].get("tools"));
    }
    let resent = bodies[4]["messages"].as_array().unwrap();
    let next = bodies[5]["messages"].as_array().unwrap();
    assert!(next.len() > resent.len());
    assert_eq!(next[..resent.len()], resent[..]);
    assert!(seen.iter().any(|e| matches!(
        e,
        AgentEvent::Compacted {
            trigger: CompactionTrigger::Convert,
            partial: true,
            turn: Some(2),
            ..
        }
    )));
    assert_eq!(provider.sent().len(), 6);
    assert_eq!(
        provider.body(3)["messages"][0]["content"][0],
        raw_block(SUMMARY)
    );
    assert!(provider.beta(3).contains(COMPACTION_BETA));
    assert!(!provider.beta(4).contains(COMPACTION_BETA));
    let messages = provider.body(4)["messages"].as_array().unwrap().clone();
    assert_eq!(messages.len(), 1, "{messages:#?}");
    let content = messages[0]["content"].as_array().unwrap();
    assert!(content[0]["text"].as_str().unwrap().contains(SUMMARY));
    let n = content.len();
    assert_eq!(content[n - 3]["text"], CONTINUE_NOTE);
    assert_eq!(content[n - 2]["text"], "and this");
    assert_eq!(content[n - 1]["text"], STEER_NOTE);
    assert_eq!(
        after_convert(&store),
        [
            "compacted Some(2) partial true".to_owned(),
            format!("User {CONTINUE_NOTE}"),
            "input and this".to_owned(),
            "Assistant Three.".to_owned(),
            "Assistant Four.".to_owned(),
        ]
    );
    let note = &agent.history[agent.view_from()];
    assert_eq!(note.role, Role::User);
    assert_eq!(
        texts(note),
        format!("{CONTINUE_NOTE}|and this|{STEER_NOTE}")
    );
    assert_rebuilds(&store, &agent);
}

#[tokio::test]
async fn a_kept_convert_after_an_effort_change_sends_the_note_the_marker_then_the_steer() {
    let store = MemoryStore::default();
    let gate = Arc::new(Semaphore::new(0));
    let provider = scripted(vec![
        answer("Hello.", 1_000),
        compaction(SUMMARY),
        answer("Two.", 300).gated(&gate),
        block_refused(),
        answer("Three.", 400),
    ]);
    let mut live = Live::start(logged(provider.clone(), &store));
    live.turn("u1", "first").await;
    live.compact(2, None).await;
    change(&mut live, 3, None, Some(Effort::Low)).await;
    let ended = steered_turn(&mut live, &gate).await;
    let (agent, seen) = live.close().await;
    assert!(ended_completed(&ended), "{ended:?}");
    assert!(!fell_back(&seen));
    assert_eq!(provider.sent().len(), 5);
    assert!(!provider.beta(4).contains(COMPACTION_BETA));
    let messages = provider.body(4)["messages"].as_array().unwrap().clone();
    assert_eq!(last_text(&messages[0]["content"]), CONTINUE_NOTE);
    let steer = messages[messages.len() - 1]["content"].as_array().unwrap();
    assert_eq!(steer[steer.len() - 2]["text"], "and this");
    assert_eq!(steer[steer.len() - 1]["text"], STEER_NOTE);
    assert_eq!(
        after_convert(&store),
        [
            "compacted Some(2) partial true".to_owned(),
            format!("User {CONTINUE_NOTE}"),
            "marker".to_owned(),
            "input and this".to_owned(),
            "Assistant Three.".to_owned(),
        ]
    );
    let from = agent.view_from();
    assert_eq!(texts(&agent.history[from]), CONTINUE_NOTE);
    assert_eq!(
        agent.history[from + 1].effort_marker().map(|m| m.level),
        Some(Effort::Low)
    );
    assert_eq!(
        texts(&agent.history[from + 2]),
        format!("and this|{STEER_NOTE}")
    );
    assert_rebuilds(&store, &agent);
}

#[tokio::test]
async fn a_turn_that_fails_after_a_kept_convert_logs_no_note() {
    let store = MemoryStore::default();
    let gate = Arc::new(Semaphore::new(0));
    let provider = scripted(vec![
        answer("Hello.", 1_000),
        compaction(SUMMARY),
        answer("Two.", 300).gated(&gate),
        block_refused(),
        block_refused(),
        answer("Four.", 500),
    ]);
    let mut live = Live::start(logged(provider.clone(), &store));
    live.turn("u1", "first").await;
    live.compact(2, None).await;
    let ended = steered_turn(&mut live, &gate).await;
    assert!(
        matches!(
            ended,
            AgentEvent::TurnEnded {
                outcome: TurnOutcome::Error { .. },
                ..
            }
        ),
        "{ended:?}"
    );
    assert!(completed(&live.turn("u4", "fourth").await));
    let (agent, _) = live.close().await;
    assert_eq!(provider.sent().len(), 6);
    assert!(!log_events(&store).iter().any(|e| matches!(
        e,
        LogEvent::MessageCommitted { message, .. } if logged_texts(message).contains(CONTINUE_NOTE)
    )));
    let from = agent.view_from();
    assert_eq!(agent.history.len(), from + 2);
    assert!(texts(&agent.history[from]).ends_with("fourth"));
    assert_rebuilds(&store, &agent);
}

#[tokio::test]
async fn a_kept_convert_closes_the_paused_reply_it_leaves_behind() {
    let store = MemoryStore::default();
    let provider = scripted(vec![
        answer("Hello.", 1_000),
        compaction(SUMMARY),
        paused_search(300, 10),
        block_refused(),
        answer("Three.", 400),
    ]);
    let mut live = Live::start(logged(provider.clone(), &store));
    live.turn("u1", "first").await;
    live.compact(2, None).await;
    assert!(completed(&live.turn("u2", "second").await));
    let (agent, seen) = live.close().await;
    let logged = log_events(&store);
    let closed = logged
        .iter()
        .position(|e| matches!(e, LogEvent::TailClosed { calls, .. } if calls == &["srvtoolu_1"]))
        .unwrap();
    let converted = logged
        .iter()
        .position(|e| {
            matches!(
                e,
                LogEvent::ContextCompacted {
                    trigger: CompactionTrigger::Convert,
                    ..
                }
            )
        })
        .unwrap();
    assert!(closed < converted, "{logged:#?}");
    assert!(matches!(
        &logged[closed],
        LogEvent::TailClosed {
            reason: UnfinishedReason::Compacted,
            ..
        }
    ));
    assert_eq!(search_end(&seen), (UnfinishedReason::Compacted, false));
    let messages = provider.body(4)["messages"].as_array().unwrap().clone();
    assert_eq!(messages.len(), 1, "{messages:#?}");
    assert_eq!(last_text(&messages[0]["content"]), CONTINUE_NOTE);
    assert_rebuilds(&store, &agent);
}

fn search_end(seen: &[AgentEvent]) -> (UnfinishedReason, bool) {
    let ends: Vec<(UnfinishedReason, bool)> = seen
        .iter()
        .filter_map(|e| match e {
            AgentEvent::SearchEnded {
                call_id,
                outcome:
                    SearchEnd::Unfinished {
                        reason,
                        possibly_billed,
                    },
                ..
            } if call_id == "srvtoolu_1" => Some((*reason, *possibly_billed)),
            _ => None,
        })
        .collect();
    match ends.as_slice() {
        [one] => *one,
        _ => panic!("{seen:#?}"),
    }
}

fn quick_with_budget(store: &MemoryStore, provider: Arc<Fake>, budget: Option<f64>) -> Agent {
    logged(provider, store).with_retry_policy(crate::agent::retry::quick().with_budget(budget))
}

#[tokio::test]
async fn a_kept_convert_after_a_billed_attempt_closes_the_search_as_possibly_billed() {
    let store = MemoryStore::default();
    let provider = scripted(vec![
        answer("Hello.", 1_000),
        compaction(SUMMARY),
        paused_search(300, 10),
        failing(mid_stream(RetryClass::Transient)),
        block_refused(),
        answer("Three.", 400),
    ]);
    let mut live = Live::start(quick_with_budget(&store, provider.clone(), None));
    live.turn("u1", "first").await;
    live.compact(2, None).await;
    assert!(completed(&live.turn("u2", "second").await));
    let (agent, seen) = live.close().await;
    assert_eq!(provider.sent().len(), 6);
    assert_eq!(search_end(&seen), (UnfinishedReason::Compacted, true));
    assert_rebuilds(&store, &agent);
}

#[tokio::test]
async fn a_retry_after_a_kept_convert_doesnt_count_the_closed_search() {
    let store = MemoryStore::default();
    let provider = scripted(vec![
        answer("Hello.", 1_000),
        compaction(SUMMARY),
        paused_search(300, 10),
        block_refused(),
        failing(mid_stream(RetryClass::Transient)),
        answer("Three.", 400),
    ]);
    let budget =
        Some(crate::pricing::web_search_usd(crate::endpoints::ProviderKind::Anthropic, 1) * 1.5);
    let mut live = Live::start(quick_with_budget(&store, provider.clone(), budget));
    live.turn("u1", "first").await;
    live.compact(2, None).await;
    let outcome = live.turn("u2", "second").await;
    let (agent, _) = live.close().await;
    assert!(completed(&outcome), "{outcome:?}");
    assert_eq!(provider.sent().len(), 6);
    assert_rebuilds(&store, &agent);
}
