use wisp_protocol::CONTINUE_NOTE;
use wisp_protocol::log::LEAD_FILL;

use super::attachments::named_call;
use super::auto::{no_retries, order};
use super::smaller::{WINDOWS, WINDOWS_NO_NATIVE, endpoint_quote, snapshot, windows};
use super::*;
use crate::agent::compact::overflow::smaller_window;

const SMALL_WINDOW: &str = "[models.\"claude-opus-5-5\"]\ncontext_window = 104000\n";

async fn retry_on(live: &mut Live, turn: u64, endpoint: Option<&str>, request: u64) -> TurnOutcome {
    live.send(ClientCommand::Retry {
        turn,
        endpoint: endpoint.map(str::to_owned),
        request: RequestId(request),
    })
    .await;
    match live
        .until(|e| matches!(e, AgentEvent::TurnEnded { .. }))
        .await
    {
        AgentEvent::TurnEnded { outcome, .. } => outcome,
        _ => unreachable!(),
    }
}

fn context_note(store: &MemoryStore, turn: u64) -> Option<String> {
    log_events(store).into_iter().find_map(|e| match e {
        LogEvent::TurnStarted { turn: t, lead, .. } if t == turn => {
            lead.text(LEAD_FILL).map(str::to_owned)
        }
        _ => None,
    })
}

fn kinds(store: &MemoryStore) -> Vec<String> {
    log_events(store)
        .iter()
        .map(|r| {
            serde_json::to_value(r).unwrap()["type"]
                .as_str()
                .unwrap()
                .to_owned()
        })
        .collect()
}

fn first_after(kinds: &[String], from: usize, kind: &str) -> usize {
    from + kinds[from..].iter().position(|k| k == kind).unwrap()
}

fn overflow_started(seen: &[AgentEvent], turn: u64) -> bool {
    seen.iter().any(|e| {
        matches!(
            e,
            AgentEvent::CompactionStarted {
                trigger: CompactionTrigger::Overflow,
                turn: Some(t),
                ..
            } if *t == turn
        )
    })
}

async fn failed_at_300k(live: &mut Live) {
    assert!(completed(&live.turn("u1", "first").await));
    assert!(completed(&live.turn("u2", "second").await));
    let failed = live.turn("u3", "third").await;
    assert!(offered(&failed), "{failed:?}");
}

#[tokio::test]
async fn a_switching_retry_to_a_smaller_window_compacts_there_through_an_earlier_request() {
    let mut estimates = Vec::new();
    for (source, model) in [
        (DEFAULT_MODEL, DEFAULT_MODEL),
        ("gpt-oss-gateway", "claude-sonnet-5-5"),
    ] {
        let store = MemoryStore::default();
        let provider = scripted(vec![
            answer("Hello.", if source == model { 150_000 } else { 100_000 }),
            answer("Second.", 300_000),
            failing(server_error()),
            compaction(SUMMARY),
            answer("Retried.", 3_000),
        ]);
        let mut agent = windows(&store, &provider, WINDOWS);
        if model != DEFAULT_MODEL {
            agent = agent.with_model_settings(models(&format!("{WINDOWS}\n[models.\"{source}\"]\ncontext_window = 1000000\nnative_compaction = true\n[endpoints.other.models.\"{model}\"]\ncontext_window = 200000\nnative_compaction = true\n")));
            agent.session.model = source.into();
            agent.connections[1].provider = provider.view(|k| {
                k.list = Some(Arc::new(
                    crate::models::list::ModelList::from_gemini_ids(
                        [(model.to_owned(), None, None)],
                        Some(model),
                    )
                    .unwrap(),
                ));
            });
        }
        let mut live = Live::start(agent);
        failed_at_300k(&mut live).await;
        let quotes = snapshot(&mut live, 4).await.switches;
        let other = endpoint_quote(&quotes, "other").unwrap();
        let cost = other.retry_compaction.unwrap();
        assert!(cost.estimate_micros > 0 && cost.head_write_micros > 0);
        estimates.push(cost.estimate_micros);
        assert!(other.consequence.is_some());
        assert!(endpoint_quote(&quotes, "env").is_none_or(|q| q.retry_compaction.is_none()));
        let outcome = retry_on(&mut live, 3, Some("other"), 5).await;
        assert!(completed(&outcome), "{outcome:?}");
        let (agent, seen) = live.close().await;

        assert!(overflow_started(&seen, 4));
        assert_eq!(order(&store, 4), ["compacted", "started", "ended"]);
        let logged = log_events(&store);
        assert!(logged.iter().any(|e| matches!(
            e,
            LogEvent::ContextCompacted {
                trigger: CompactionTrigger::Overflow,
                partial,
                turn: Some(4),
                ..
            } if *partial == (source != model)
        )));
        let kinds = kinds(&store);
        let switched = kinds.iter().rposition(|k| k == "settings_changed").unwrap();
        let marked = first_after(&kinds, switched, "rebuild_marked");
        let compacted = first_after(&kinds, switched, "context_compacted");
        let started = first_after(&kinds, switched, "turn_started");
        assert!(switched < marked && marked < compacted && compacted < started);

        assert_eq!(provider.sent().len(), 5);
        assert_eq!(
            provider.body(3)["messages"].as_array().unwrap().len(),
            provider.body(0)["messages"].as_array().unwrap().len()
        );
        assert_eq!(provider.body(3)["model"], model);
        let retried = provider.body(4);
        assert_eq!(retried["model"], model);
        assert_eq!(agent.session().model, model);
        assert_eq!(retried["messages"][0]["content"][0]["type"], "compaction");
        assert!(retried["messages"].to_string().contains("third"));
        assert_ne!(context_note(&store, 4), context_note(&store, 3));
        assert!(context_note(&store, 4).is_some());
        assert_eq!(agent.endpoint_name(), "other");
        assert_rebuilds(&store, &agent);
    }
    assert!(estimates[1] < estimates[0]);
}

#[tokio::test]
async fn a_later_overflow_in_the_retry_turn_is_its_second_and_ends_it() {
    let store = MemoryStore::default();
    let provider = scripted(vec![
        answer("Hello.", 150_000),
        answer("Second.", 300_000),
        failing(server_error()),
        compaction(SUMMARY),
        failing(ProviderError::new(
            ErrorKind::ContextOverflow,
            "prompt is too long",
        )),
    ]);
    let mut live = Live::start(windows(&store, &provider, WINDOWS));
    failed_at_300k(&mut live).await;
    let outcome = retry_on(&mut live, 3, Some("other"), 4).await;
    let (_, seen) = live.close().await;
    assert!(
        matches!(
            outcome,
            TurnOutcome::Error {
                kind: ErrorKind::ContextOverflow,
                offer: None,
                ..
            }
        ),
        "{outcome:?}"
    );
    assert_eq!(provider.sent().len(), 5);
    let overflows = seen
        .iter()
        .filter(|e| {
            matches!(
                e,
                AgentEvent::CompactionStarted {
                    trigger: CompactionTrigger::Overflow,
                    ..
                }
            )
        })
        .count();
    assert_eq!(overflows, 1);
}

#[tokio::test]
async fn a_failed_smaller_window_fallback_ends_the_retry_and_the_next_retry_reruns_it() {
    let store = MemoryStore::default();
    let provider = scripted(vec![
        answer("Hello.", 150_000),
        answer("Second.", 300_000),
        failing(server_error()),
        failing(server_error()),
        compaction(SUMMARY),
        answer("Retried.", 3_000),
    ]);
    let mut live = Live::start(windows(&store, &provider, WINDOWS));
    failed_at_300k(&mut live).await;
    let outcome = retry_on(&mut live, 3, Some("other"), 4).await;
    assert!(
        matches!(
            outcome,
            TurnOutcome::Error {
                kind: ErrorKind::Server,
                offer: Some(_),
                ..
            }
        ),
        "{outcome:?}"
    );
    assert_eq!(order(&store, 4), ["failed", "started", "ended"]);
    let again = retry_on(&mut live, 4, None, 5).await;
    assert!(completed(&again), "{again:?}");
    let (agent, seen) = live.close().await;
    assert!(overflow_started(&seen, 5));
    assert_eq!(order(&store, 5), ["compacted", "started", "ended"]);
    assert_eq!(provider.sent().len(), 6);
    assert!(provider.body(5)["messages"].to_string().contains("third"));
    assert_eq!(agent.endpoint_name(), "other");
    assert_rebuilds(&store, &agent);
}

#[tokio::test]
async fn a_smaller_window_retry_with_no_request_that_fits_ends_as_an_overflow() {
    let store = MemoryStore::default();
    let provider = scripted(vec![
        answer("Hello.", 250_000),
        answer("Second.", 300_000),
        failing(server_error()),
    ]);
    let mut live = Live::start(windows(&store, &provider, WINDOWS));
    failed_at_300k(&mut live).await;
    let outcome = retry_on(&mut live, 3, Some("other"), 4).await;
    live.close().await;
    let expected = smaller_window(DEFAULT_MODEL, 200_000, "other");
    assert!(
        matches!(
            &outcome,
            TurnOutcome::Error {
                kind: ErrorKind::ContextOverflow,
                offer: None,
                message,
                ..
            } if *message == expected
        ),
        "{outcome:?}"
    );
    assert_eq!(provider.sent().len(), 3);
    assert_eq!(order(&store, 4), ["failed", "started", "ended"]);
}

#[tokio::test]
async fn a_retry_whose_converted_view_wont_fit_the_target_ends_before_any_request() {
    let store = MemoryStore::default();
    let provider = scripted(vec![
        answer("Hello.", 1_000),
        compaction(SUMMARY),
        failing(server_error()),
    ]);
    let mut live = Live::start(windows(&store, &provider, WINDOWS_NO_NATIVE));
    live.turn("u1", "first").await;
    live.compact(2, None).await;
    let failed = live.turn("u2", &"word ".repeat(200_000)).await;
    assert!(offered(&failed), "{failed:?}");
    let outcome = retry_on(&mut live, 2, Some("other"), 3).await;
    let (_, seen) = live.close().await;
    let expected = smaller_window(DEFAULT_MODEL, 200_000, "other");
    assert!(
        matches!(
            &outcome,
            TurnOutcome::Error {
                kind: ErrorKind::ContextOverflow,
                message,
                ..
            } if *message == expected
        ),
        "{outcome:?}"
    );
    assert!(!seen.iter().any(|e| matches!(
        e,
        AgentEvent::Compacted {
            trigger: CompactionTrigger::Convert,
            ..
        }
    )));
    assert_eq!(provider.sent().len(), 3);
    assert_eq!(order(&store, 3), ["started", "ended"]);
}

#[tokio::test]
async fn a_switching_retry_marks_the_rebuild_before_its_convert() {
    let store = MemoryStore::default();
    let provider = scripted(vec![
        answer("Hello.", 1_000),
        compaction(SUMMARY),
        answer("Second.", 1_000),
        failing(server_error()),
        server_failed(),
        answer("Retried.", 1_000),
    ]);
    let mut live = Live::start(windows(&store, &provider, WINDOWS_NO_NATIVE));
    live.turn("u1", "first").await;
    live.compact(2, None).await;
    assert!(completed(&live.turn("u2", "second").await));
    let failed = live.turn("u3", "third").await;
    assert!(offered(&failed), "{failed:?}");
    let outcome = retry_on(&mut live, 3, Some("other"), 3).await;
    assert!(completed(&outcome), "{outcome:?}");
    let (agent, _) = live.close().await;
    let kinds = kinds(&store);
    let switched = kinds.iter().rposition(|k| k == "settings_changed").unwrap();
    let marked = first_after(&kinds, switched, "rebuild_marked");
    let compacted = first_after(&kinds, switched, "context_compacted");
    let started = first_after(&kinds, switched, "turn_started");
    assert!(switched < marked && marked < compacted && compacted < started);
    assert_rebuilds(&store, &agent);
}

#[tokio::test]
async fn a_retry_after_a_turn_that_ended_on_its_compaction_reruns_it_first() {
    let store = MemoryStore::default();
    let provider = scripted(vec![
        answer("Hello.", 100_000),
        server_failed(),
        compaction(SUMMARY),
        answer("Retried.", 2_000),
    ]);
    let mut live = Live::start(no_retries(on(&provider, &store, SMALL_WINDOW)));
    assert!(completed(&live.turn("u1", "first").await));
    let failed = live.turn("u2", "second").await;
    assert!(
        matches!(
            failed,
            TurnOutcome::Error {
                kind: ErrorKind::Server,
                offer: Some(_),
                ..
            }
        ),
        "{failed:?}"
    );
    assert_eq!(order(&store, 2), ["failed", "started", "ended"]);
    let outcome = retry_on(&mut live, 2, None, 1).await;
    assert!(completed(&outcome), "{outcome:?}");
    let (agent, seen) = live.close().await;

    assert!(seen.iter().any(|e| matches!(
        e,
        AgentEvent::CompactionStarted {
            trigger: CompactionTrigger::Auto,
            turn: Some(3),
            ..
        }
    )));
    assert_eq!(order(&store, 3), ["compacted", "started", "ended"]);
    assert_eq!(provider.sent().len(), 4);
    let retried = provider.body(3);
    assert_eq!(retried["messages"][0]["content"][0]["type"], "compaction");
    assert!(retried["messages"].to_string().contains("second"));
    assert!(context_note(&store, 3).is_some());
    assert_ne!(context_note(&store, 3), context_note(&store, 2));
    assert_rebuilds(&store, &agent);
}

#[tokio::test]
async fn a_retry_after_a_mid_turn_compaction_failure_reruns_it_and_continues() {
    let store = MemoryStore::default();
    let provider = scripted(vec![
        answer("Hello.", 30_000),
        named_call("toolu_1", "bash", json!({"command": "ls"}), 100_000),
        server_failed(),
        compaction(SUMMARY),
        answer("Retried.", 2_000),
    ]);
    let mut live = Live::start(no_retries(on(&provider, &store, SMALL_WINDOW)));
    assert!(completed(&live.turn("u1", "first").await));
    let failed = live.turn("u2", "second").await;
    assert!(
        matches!(
            failed,
            TurnOutcome::Error {
                kind: ErrorKind::Server,
                committed_steps: 1,
                offer: Some(_),
                ..
            }
        ),
        "{failed:?}"
    );
    let outcome = retry_on(&mut live, 2, None, 1).await;
    assert!(completed(&outcome), "{outcome:?}");
    let (agent, _) = live.close().await;

    assert_eq!(order(&store, 3), ["compacted", "started", "ended"]);
    assert_eq!(provider.sent().len(), 5);
    let retried = provider.body(4);
    let messages = retried["messages"].as_array().unwrap();
    assert_eq!(messages[0]["content"][0]["type"], "compaction");
    assert!(retried["messages"].to_string().contains(CONTINUE_NOTE));
    assert_eq!(context_note(&store, 3), None);
    assert_rebuilds(&store, &agent);
}

#[tokio::test]
async fn a_retry_after_a_pre_turn_compaction_sends_the_head_and_the_message_again() {
    let store = MemoryStore::default();
    let provider = scripted(vec![
        answer("Hello.", 5_000),
        compaction(SUMMARY),
        failing(server_error()),
        answer("Retried.", 1_000),
    ]);
    let mut live = Live::start(
        on(&provider, &store, super::auto::LOW).with_clock(Arc::new(crate::clock::StillClock)),
    );
    assert!(completed(&live.turn("u1", "first").await));
    let failed = live.turn("u2", "second").await;
    assert!(offered(&failed), "{failed:?}");
    let outcome = retry_on(&mut live, 2, None, 1).await;
    assert!(completed(&outcome), "{outcome:?}");
    let (agent, _) = live.close().await;
    assert_eq!(provider.sent().len(), 4);
    assert_eq!(provider.body(3), provider.body(2));
    assert_rebuilds(&store, &agent);
}

#[tokio::test]
async fn a_retry_after_a_mid_turn_compaction_sends_the_head_and_the_continue_note() {
    let store = MemoryStore::default();
    let provider = scripted(vec![
        answer("Hello.", 1_000),
        named_call("toolu_1", "bash", json!({"command": "ls"}), 5_000),
        compaction(SUMMARY),
        failing(server_error()),
        answer("Retried.", 1_000),
    ]);
    let mut live = Live::start(on(&provider, &store, super::auto::LOW));
    assert!(completed(&live.turn("u1", "first").await));
    let failed = live.turn("u2", "second").await;
    assert!(offered(&failed), "{failed:?}");
    let outcome = retry_on(&mut live, 2, None, 1).await;
    assert!(completed(&outcome), "{outcome:?}");
    let (agent, _) = live.close().await;
    assert_eq!(provider.sent().len(), 5);
    assert_eq!(provider.body(4), provider.body(3));
    assert!(
        provider.body(4)["messages"]
            .to_string()
            .contains(CONTINUE_NOTE)
    );
    assert_rebuilds(&store, &agent);
}

#[tokio::test]
async fn a_retry_after_overflow_recovery_of_the_unsaved_message_sends_the_head_and_the_message() {
    let store = MemoryStore::default();
    let provider = scripted(vec![
        answer("Hello.", 1_000),
        answer("Second.", 60_000),
        failing(ProviderError::new(
            ErrorKind::ContextOverflow,
            "prompt is too long",
        )),
        compaction(SUMMARY),
        failing(server_error()),
        answer("Retried.", 1_000),
    ]);
    let mut live = Live::start(logged(provider.clone(), &store));
    assert!(completed(&live.turn("u1", "first").await));
    assert!(completed(&live.turn("u2", "second").await));
    let failed = live.turn("u3", "third").await;
    assert!(offered(&failed), "{failed:?}");
    let outcome = retry_on(&mut live, 3, None, 1).await;
    assert!(completed(&outcome), "{outcome:?}");
    let (agent, _) = live.close().await;
    assert_eq!(provider.sent().len(), 6);
    let (failed, retried) = (provider.body(4), provider.body(5));
    for block in 0..2 {
        assert_eq!(
            retried["messages"][0]["content"][block],
            failed["messages"][0]["content"][block]
        );
    }
    assert_eq!(
        retried["messages"].as_array().map(Vec::len),
        failed["messages"].as_array().map(Vec::len)
    );
    assert!(
        !provider.body(5)["messages"]
            .to_string()
            .contains(CONTINUE_NOTE)
    );
    assert!(provider.body(5)["messages"].to_string().contains("third"));
    assert_rebuilds(&store, &agent);
}

fn leads(body: &serde_json::Value) -> bool {
    body["messages"][0]["role"] == "assistant"
}

#[tokio::test]
async fn a_smaller_window_retry_counts_its_re_sent_message_against_kept_turns() {
    let store = MemoryStore::default();
    let provider = scripted(vec![
        answer("Hello.", 150_000),
        answer("Second.", 300_000),
        failing(server_error()),
        compaction(SUMMARY),
        answer("Retried.", 3_000),
    ]);
    let mut live = Live::start(windows(&store, &provider, WINDOWS));
    assert!(completed(&live.turn("u1", "first").await));
    assert!(completed(&live.turn("u2", &"x".repeat(300_000)).await));
    let failed = live.turn("u3", &"y".repeat(300_000)).await;
    assert!(offered(&failed), "{failed:?}");
    let outcome = retry_on(&mut live, 3, Some("other"), 4).await;
    let (agent, seen) = live.close().await;
    assert!(completed(&outcome), "{outcome:?}");
    assert!(overflow_started(&seen, 4));
    let retried = provider.body(4);
    assert!(
        !leads(&retried),
        "kept turns chosen though the re-sent message leaves no room"
    );
    assert!(retried["messages"].to_string().contains("yyyy"));
    assert_eq!(agent.compaction.overflow.reserve, 0);
    assert_rebuilds(&store, &agent);
}

#[tokio::test]
async fn a_retry_back_on_the_larger_window_drops_the_failed_smaller_window_fallback() {
    let store = MemoryStore::default();
    let provider = scripted(vec![
        answer("Hello.", 150_000),
        answer("Second.", 300_000),
        failing(server_error()),
        failing(server_error()),
        answer("Retried.", 3_000),
    ]);
    let mut live = Live::start(windows(&store, &provider, WINDOWS));
    failed_at_300k(&mut live).await;
    let outcome = retry_on(&mut live, 3, Some("other"), 4).await;
    assert!(offered(&outcome), "{outcome:?}");
    let again = retry_on(&mut live, 4, Some("env"), 5).await;
    assert!(completed(&again), "{again:?}");
    let (agent, seen) = live.close().await;
    assert!(overflow_started(&seen, 4));
    assert!(!overflow_started(&seen, 5));
    assert_eq!(provider.sent().len(), 5);
    assert_eq!(
        provider.body(4)["messages"].as_array().unwrap().len(),
        provider.body(2)["messages"].as_array().unwrap().len()
    );
    assert_eq!(agent.endpoint_name(), "env");
    assert_rebuilds(&store, &agent);
}

const SMALL_ENV: &str = "[models.\"claude-opus-5-5\"]\ncontext_window = 104000\n\n[endpoints.other.models.\"claude-opus-5-5\"]\ncontext_window = 1000000\n";

#[tokio::test]
async fn a_switching_retry_after_an_automatic_compactions_failure_reruns_it_on_the_new_endpoint() {
    let store = MemoryStore::default();
    let provider = scripted(vec![
        answer("Hello.", 100_000),
        server_failed(),
        compaction(SUMMARY),
        answer("Retried.", 2_000),
    ]);
    let mut live = Live::start(no_retries(windows(&store, &provider, SMALL_ENV)));
    assert!(completed(&live.turn("u1", "first").await));
    let failed = live.turn("u2", "second").await;
    assert!(
        matches!(
            failed,
            TurnOutcome::Error {
                kind: ErrorKind::Server,
                offer: Some(_),
                ..
            }
        ),
        "{failed:?}"
    );
    assert_eq!(order(&store, 2), ["failed", "started", "ended"]);
    let outcome = retry_on(&mut live, 2, Some("other"), 1).await;
    assert!(completed(&outcome), "{outcome:?}");
    let (agent, seen) = live.close().await;
    assert!(seen.iter().any(|e| matches!(
        e,
        AgentEvent::CompactionStarted {
            trigger: CompactionTrigger::Auto,
            turn: Some(3),
            ..
        }
    )));
    assert!(!overflow_started(&seen, 3));
    assert_eq!(provider.sent().len(), 4);
    let retried = provider.body(3);
    assert_eq!(retried["messages"][0]["content"][0]["type"], "compaction");
    assert!(retried["messages"].to_string().contains("second"));
    assert_eq!(agent.endpoint_name(), "other");
    assert_rebuilds(&store, &agent);
}
