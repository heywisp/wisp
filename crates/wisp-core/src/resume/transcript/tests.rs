use serde_json::json;
use wisp_protocol::log::{FeatureState, LOG_SCHEMA_VERSION};
use wisp_protocol::{
    CitedSource, ErrorKind, RetryFocus, SearchAction, SearchEnd, TurnStats, UnfinishedReason,
    WithdrawnBy,
};

use super::*;
use crate::test_guard::{pick, text};

fn records(events: Vec<LogEvent>) -> Vec<LogRecord> {
    events
        .into_iter()
        .enumerate()
        .map(|(seq, event)| LogRecord {
            schema_version: LOG_SCHEMA_VERSION,
            seq: u64::try_from(seq).unwrap(),
            at: "2026-10-26T09:00:00Z".to_owned(),
            event,
        })
        .collect()
}

#[test]
fn replay_scrubs_legacy_hook_block_messages_and_results_independently() {
    let email = ["CANARYlegacy", "example.invalid"].join("@");
    let finished = |message: String, result: Option<String>| {
        let mut event = AgentEvent::call_finished(
            "c1",
            wisp_protocol::ToolOutcome::Failed {
                message,
                before_start: true,
            },
        );
        if let AgentEvent::ToolCallFinished { result: slot, .. } = &mut event {
            *slot = result;
        }
        event
    };
    let blocked = format!("Blocked by aa/h: {email}");
    let without_result = finished(blocked.clone(), None);
    let with_result = finished("failed".into(), Some(blocked));
    let logs = records(
        [without_result, with_result]
            .iter()
            .map(|event| crate::session::mirror::log_record(event, Some(1)).unwrap())
            .collect(),
    );
    let events = transcript(&logs, &Toolbox::default(), None);
    let clean = "Blocked by aa/h: [redacted]".to_owned();
    assert!(events.contains(&finished(clean.clone(), None)));
    assert!(events.contains(&finished("failed".into(), Some(clean))));
    assert!(serde_json::to_string(&logs).unwrap().contains(&email));
}

#[test]
fn replay_scrubs_terminal_compaction_refusals_even_without_a_visible_failure() {
    use wisp_protocol::{SessionView, link::ToClient};

    let raw = ["CANARYterminal", "example.invalid"].join("@");
    let blocked = format!("Blocked by aa/g: {raw}");
    let strategy = format!(
        "The aa strategy refused the summary, so Wisp kept the conversation as it was: {raw}"
    );
    let compaction = |turn, kind, message: &str| {
        serde_json::from_value::<LogEvent>(json!({
            "type": "compaction_failed", "turn": turn, "trigger": "auto", "kind": kind, "message": message
        })).unwrap()
    };
    let end = |turn, error_kind, given: &str| {
        let mut event = failed(turn, None);
        if let LogEvent::TurnEnded {
            outcome: TurnOutcome::Error { kind, message, .. },
            ..
        } = &mut event
        {
            *kind = error_kind;
            *message = given.to_owned();
        }
        event
    };
    let check = |turn, given: &str| end(turn, ErrorKind::InvalidResponse, given);
    let older = "CANARYOpaqueCredentialBefore";
    let newer = "CANARYOpaqueCredentialAfter";
    let late_at_open = "CANARYOpaqueLegacyLinkSecretNowKnown";
    let old = Redactions::new([older.to_owned()]);
    let newer_registry = Redactions::new([older.to_owned(), newer.to_owned()]);
    let both = format!("{older} joining {newer}");
    let merged_registry = Redactions::new([older.to_owned(), newer.to_owned(), both.clone()]);
    let mixed = format!("{older} {raw}");
    let later_mixed = old.scrub(&mixed);
    assert_eq!(later_mixed, format!("{REDACTED} {raw}"));
    let with_existing = format!("start {both}; then {newer}; end {raw}");
    let earlier_existing = old.scrub(&with_existing);
    let later_existing = merged_registry.scrub(&with_existing);
    assert_eq!(
        later_existing,
        format!("start {REDACTED}; then {REDACTED}; end {raw}")
    );
    let late = format!("{older} {late_at_open}");
    let late_terminal = old.scrub(&late);
    let proxy = format!("Blocked by gateway: {raw}");
    let unrelated = format!("start {REDACTED}; now {raw}");
    let logs = records(vec![
        compaction(1, "blocked", &blocked),
        end(1, ErrorKind::Other, &blocked),
        compaction(2, "check_failed", &strategy),
        check(2, &strategy),
        compaction(3, "check_failed", &raw),
        check(3, &raw),
        compaction(4, "check_failed", &mixed),
        check(4, &later_mixed),
        compaction(5, "check_failed", &earlier_existing),
        check(5, &later_existing),
        compaction(6, "check_failed", older),
        check(6, &newer_registry.scrub(older)),
        compaction(7, "check_failed", &late),
        check(7, &late_terminal),
        check(8, &later_mixed),
        end(9, ErrorKind::Auth, &proxy),
        check(10, &proxy),
        compaction(11, "check_failed", &mixed),
        end(11, ErrorKind::Auth, &later_mixed),
        compaction(12, "check_failed", &earlier_existing),
        check(12, &unrelated),
        end(13, ErrorKind::Auth, &blocked),
    ]);
    let original = serde_json::to_string(&logs).unwrap();
    let shown = transcript(&logs, &Toolbox::default(), None);
    assert!(
        shown
            .iter()
            .all(|event| matches!(event, AgentEvent::TurnEnded { .. }))
    );
    let messages = |events: &[AgentEvent]| pick!(events, AgentEvent::TurnEnded { outcome: TurnOutcome::Error { message, .. }, .. } => message.clone());
    let mut expected = vec![
        "Blocked by aa/g: [redacted]".to_owned(),
        "The aa strategy refused the summary, so Wisp kept the conversation as it was: [redacted]"
            .to_owned(),
        "Compaction stopped: [redacted]".to_owned(),
        "Compaction stopped: [redacted]".to_owned(),
        "Compaction stopped: [redacted]".to_owned(),
        REDACTED.to_owned(),
        format!("Compaction stopped: {late_terminal}"),
        later_mixed.clone(),
        proxy.clone(),
        proxy,
        later_mixed,
        unrelated,
        blocked,
    ];
    assert_eq!(messages(&shown), expected);
    let opening = serde_json::from_str(include_str!(
        "../../../../wisp-protocol/tests/fixtures/opening.json"
    ))
    .unwrap();
    let mut frames = [ToClient::Opened {
        opening,
        view: SessionView {
            events: shown,
            ..SessionView::default()
        },
        now: None,
    }];
    crate::session::register_secrets([late_at_open.to_owned()]);
    expected[6] = "Compaction stopped: [redacted]".to_owned();
    crate::agent::with_scrubbed_link_frames(&mut frames, None, |frames| {
        let [ToClient::Opened { view, .. }] = frames else {
            panic!("expected opening replay frame")
        };
        assert_eq!(messages(&view.events), expected);
    });
    assert_eq!(serde_json::to_string(&logs).unwrap(), original);
}

fn started(value: serde_json::Value) -> LogEvent {
    let mut value = value;
    value["type"] = json!("turn_started");
    serde_json::from_value(value).unwrap()
}

fn assistant(content: Vec<ContentBlock>) -> LogEvent {
    LogEvent::MessageCommitted {
        turn: 1,
        message: wisp_protocol::log::LoggedMessage {
            role: Role::Assistant,
            content: content
                .into_iter()
                .map(wisp_protocol::log::LoggedBlock::Inline)
                .collect(),
        },
        model: None,
        stop_reason: None,
    }
}

fn offer(retry: bool) -> RetryOffer {
    RetryOffer {
        retry,
        switch: !retry,
        focus: RetryFocus::Retry,
        suggest: None,
        retries_at: None,
    }
}

fn failed(turn: u64, offered: Option<RetryOffer>) -> LogEvent {
    LogEvent::TurnEnded {
        turn,
        outcome: TurnOutcome::Error {
            kind: ErrorKind::Server,
            message: "boom".to_owned(),
            usage: None,
            cost: None,
            committed_steps: 0,
            detail: None,
            attempts: 0,
            gave_up: None,
            offer: offered,
        },
        stats: TurnStats::default(),
        partial_output: None,
    }
}

fn offers(events: &[AgentEvent]) -> Vec<Option<RetryOffer>> {
    pick!(
        events,
        AgentEvent::TurnEnded {
            outcome: TurnOutcome::Error { offer, .. },
            ..
        } => offer.clone()
    )
}

fn id(n: &str) -> InputId {
    InputId(n.to_owned())
}

#[test]
fn only_the_last_failed_turn_carries_the_recomputed_offer() {
    let log = records(vec![
        started(json!({"turn": 1, "text": "one"})),
        failed(1, Some(offer(true))),
        started(json!({"turn": 2, "text": "two"})),
        failed(2, Some(offer(true))),
    ]);
    let events = transcript(&log, &Toolbox::default(), Some((2, offer(false))));
    assert_eq!(offers(&events), [None, Some(offer(false))]);
    let none = transcript(&log, &Toolbox::default(), None);
    assert_eq!(offers(&none), [None, None]);
}

#[test]
fn a_turn_logged_with_only_its_text_is_shown_as_its_opening_message() {
    let log = records(vec![started(json!({"turn": 3, "text": "old style"}))]);
    let events = transcript(&log, &Toolbox::default(), None);
    assert!(
        matches!(
            &events[..],
            [
                AgentEvent::TurnStarted { turn: 3, .. },
                AgentEvent::StepStarted { step: 1 },
                AgentEvent::InputAccepted { id: accepted, text, .. },
                AgentEvent::InputDelivered { ids, step: 1 },
            ] if accepted == &id("turn-3") && text == "old style" && ids == &[id("turn-3")]
        ),
        "{events:#?}"
    );
}

#[test]
fn a_continuation_with_no_opening_delivers_nothing() {
    let log = records(vec![started(json!({"turn": 2, "retry_of": 1}))]);
    let events = transcript(&log, &Toolbox::default(), None);
    assert!(
        matches!(
            &events[..],
            [
                AgentEvent::TurnStarted {
                    turn: 2,
                    retry_of: Some(1),
                    ..
                },
                AgentEvent::StepStarted { step: 1 },
            ]
        ),
        "{events:#?}"
    );
}

#[test]
fn messages_that_never_went_out_are_not_shown() {
    let log = records(vec![
        started(json!({"turn": 1, "inputs": [{"id": "a", "text": "go"}]})),
        LogEvent::InputAccepted {
            turn: Some(1),
            id: id("b"),
            mode: DeliveryMode::Steer,
            text: "never sent".to_owned(),
            images: Vec::new(),
            replaces: None,
            held: false,
            from: None,
        },
        LogEvent::InputWithdrawn {
            turn: Some(1),
            ids: vec![id("a"), id("b")],
            by: WithdrawnBy::TurnFailed,
        },
    ]);
    let events = transcript(&log, &Toolbox::default(), None);
    let accepted: Vec<&InputId> = pick!(&events, AgentEvent::InputAccepted { id, .. } => id);
    assert_eq!(accepted, [&id("a")]);
    assert!(
        events.iter().any(|e| matches!(
            e,
            AgentEvent::InputWithdrawn { ids, by: WithdrawnBy::TurnFailed } if ids == &[id("a")]
        )),
        "{events:#?}"
    );
}

#[test]
fn a_queued_message_is_accepted_where_the_log_accepted_it() {
    let log = records(vec![
        started(json!({"turn": 1, "inputs": [{"id": "a", "text": "go"}]})),
        LogEvent::InputAccepted {
            turn: Some(1),
            id: id("q"),
            mode: DeliveryMode::Queue,
            text: "next".to_owned(),
            images: Vec::new(),
            replaces: None,
            held: false,
            from: None,
        },
        LogEvent::TurnEnded {
            turn: 1,
            outcome: TurnOutcome::Cancelled {
                usage: None,
                cost: None,
                committed_steps: 0,
            },
            stats: TurnStats::default(),
            partial_output: None,
        },
        started(json!({"turn": 2, "inputs": [{"id": "q", "text": "next"}]})),
    ]);
    let events = transcript(&log, &Toolbox::default(), None);
    let at = |pick: fn(&AgentEvent) -> bool| events.iter().position(pick).unwrap();
    let accepted = at(|e| matches!(e, AgentEvent::InputAccepted { id, .. } if id.0 == "q"));
    let second = at(|e| matches!(e, AgentEvent::TurnStarted { turn: 2, .. }));
    assert!(accepted < second, "{events:#?}");
}

#[test]
fn a_reply_shows_its_thinking_and_text_and_skips_tool_blocks() {
    let log = records(vec![
        started(json!({"turn": 1, "text": "hi"})),
        assistant(vec![
            ContentBlock::Thinking {
                thinking: "hmm".to_owned(),
                signature: "sig".to_owned(),
            },
            ContentBlock::RedactedThinking {
                data: "x".to_owned(),
            },
            text("hello"),
            ContentBlock::ToolUse {
                id: "c1".to_owned(),
                name: "bash".to_owned(),
                input: json!({}),
            },
        ]),
    ]);
    let events = transcript(&log, &Toolbox::default(), None);
    let blocks: Vec<&AgentEvent> = events
        .iter()
        .skip_while(|e| !matches!(e, AgentEvent::BlockStarted { .. }))
        .collect();
    assert_eq!(
        blocks,
        [
            &AgentEvent::BlockStarted {
                index: 0,
                kind: BlockKind::Thinking
            },
            &AgentEvent::ThinkingDelta {
                index: 0,
                text: "hmm".to_owned()
            },
            &AgentEvent::BlockStopped { index: 0 },
            &AgentEvent::BlockStarted {
                index: 1,
                kind: BlockKind::RedactedThinking
            },
            &AgentEvent::BlockStopped { index: 1 },
            &AgentEvent::BlockStarted {
                index: 2,
                kind: BlockKind::Text
            },
            &AgentEvent::TextDelta {
                index: 2,
                text: "hello".to_owned()
            },
            &AgentEvent::BlockStopped { index: 2 },
        ]
    );
}

#[test]
fn a_quiet_end_replays_no_block() {
    let typed = ContentBlock::Thinking {
        thinking: "hmm".to_owned(),
        signature: "sig".to_owned(),
    };
    let gem_reasoning = crate::provider::envelope(
        crate::provider::gemini::PROVIDER,
        &json!({"type": crate::provider::REASONING_ITEM, "thought": "quiet", "signature": ""}),
        1,
        "id_gem",
    );
    let gem_thought = ContentBlock::Thinking {
        thinking: "quiet".to_owned(),
        signature: String::new(),
    };
    let gem_sig = crate::provider::envelope(
        crate::provider::gemini::PROVIDER,
        &json!({"type": crate::provider::gemini::TEXT_SIGNATURE_ITEM, "signature": "sig_q"}),
        0,
        "id_gem",
    );
    let log = records(vec![
        started(json!({"turn": 1, "text": "hi"})),
        assistant(vec![text(crate::provider::NO_TEXT)]),
        assistant(vec![typed, text(" [no text]\n")]),
        assistant(vec![
            gem_reasoning,
            gem_thought,
            gem_sig,
            text(crate::provider::NO_TEXT),
        ]),
    ]);
    let events = transcript(&log, &Toolbox::default(), None);
    assert!(
        !events
            .iter()
            .any(|e| matches!(e, AgentEvent::BlockStarted { .. })),
        "{events:#?}"
    );
}

#[test]
fn searches_of_a_retried_attempt_are_dropped_and_citations_follow_the_text() {
    let search = |attempt: u32, call: &str| LogEvent::SearchStarted {
        turn: 1,
        step: 1,
        attempt,
        call_id: call.to_owned(),
        query: format!("query {attempt}"),
        action: SearchAction::Search,
    };
    let ended = |attempt: u32, call: &str| LogEvent::SearchEnded {
        turn: Some(1),
        step: 1,
        attempt,
        call_id: call.to_owned(),
        outcome: SearchEnd::Unfinished {
            reason: UnfinishedReason::Stopped,
            possibly_billed: false,
        },
    };
    let log = records(vec![
        started(json!({"turn": 1, "text": "find"})),
        search(1, "s1"),
        ended(1, "s1"),
        search(2, "s2"),
        LogEvent::Citations {
            turn: 1,
            step: 1,
            attempt: 2,
            index: 0,
            sources: vec![CitedSource {
                url: "https://example.com".to_owned(),
                title: None,
            }],
        },
        ended(2, "s2"),
        assistant(vec![text("found")]),
    ]);
    let events = transcript(&log, &Toolbox::default(), None);
    let calls: Vec<&str> = pick!(
        &events,
        AgentEvent::SearchStarted { call_id, .. } | AgentEvent::SearchEnded { call_id, .. } =>
            call_id.as_str()
    );
    assert_eq!(calls, ["s2", "s2"]);
    let stopped = events
        .iter()
        .position(|e| matches!(e, AgentEvent::BlockStopped { index: 0 }))
        .unwrap();
    let cited = events
        .iter()
        .position(|e| matches!(e, AgentEvent::Citations { .. }))
        .unwrap();
    assert!(stopped < cited, "{events:#?}");
}

#[test]
fn a_turn_a_crash_left_open_ends_without_a_turn_end() {
    let log = records(vec![
        started(json!({"turn": 1, "text": "go"})),
        assistant(vec![text("partial")]),
    ]);
    let events = transcript(&log, &Toolbox::default(), None);
    assert!(
        !events
            .iter()
            .any(|e| matches!(e, AgentEvent::TurnEnded { .. })),
        "{events:#?}"
    );
}

fn logged(value: serde_json::Value) -> LogEvent {
    serde_json::from_value(value).unwrap()
}

fn opening(turn: u64, id: &str, text: &str) -> Vec<LogEvent> {
    vec![
        started(json!({"turn": turn, "text": text, "input_id": id})),
        logged(json!({"type": "input_committed", "turn": turn, "step": 1,
            "inputs": [{"id": id, "text": text}]})),
    ]
}

#[test]
fn a_cited_reply_shows_its_text_and_its_sources() {
    let cited = ContentBlock::opaque(
        "anthropic",
        json!({"type": "text", "text": "ripgrep is fast", "citations": [{
            "type": "web_search_result_location",
            "url": "https://example.com/s1",
            "title": "Cited page",
            "encrypted_index": "ei-1",
            "cited_text": "quoted"
        }]}),
    );
    let mut events = opening(1, "tui-1", "search");
    events.push(assistant(vec![
        cited,
        ContentBlock::opaque(
            "gemini",
            json!({"type": "call_signatures", "signatures": {"call": "CANARY_signature"}}),
        ),
    ]));
    let shown = transcript(&records(events), &Toolbox::default(), None);
    assert!(!shown.iter().any(|event| matches!(
        event,
        AgentEvent::BlockStarted {
            kind: BlockKind::Other { .. },
            ..
        }
    )));
    assert!(
        shown
            .iter()
            .any(|e| matches!(e, AgentEvent::TextDelta { text, .. } if text == "ripgrep is fast")),
        "{shown:#?}"
    );
}

#[test]
fn notes_the_live_turn_showed_are_replayed() {
    let mut events = opening(1, "tui-1", "go");
    events.extend([
        logged(
            json!({"type": "retrying", "turn": 1, "attempt": 1, "delay_ms": 2000,
            "reason": "overloaded", "step": 1, "partial_output": "half"}),
        ),
        logged(json!({"type": "fallback", "turn": 1, "from": "a", "to": "b"})),
        logged(
            json!({"type": "cache_miss", "turn": 1, "previous_prompt_tokens": 9000,
            "cache_read_input_tokens": 0, "cache_creation_input_tokens": 9000,
            "diagnostics": null}),
        ),
        logged(
            json!({"type": "cache_miss_repeated", "turn": 1, "count": 2, "usd": 1.75,
            "cause": {"type": "injected_thinking", "first_path": "messages.1.content.0",
            "count": 2}}),
        ),
        logged(
            json!({"type": "settings_changed", "model": "claude-sonnet-5",
            "effort": "high", "cache": {"type": "kept"}, "endpoint_name": "work"}),
        ),
        logged(json!({"type": "context_compacted", "turn": 1, "epoch": 1,
            "trigger": "manual", "engine": "wisp", "before": 9000, "after": 900,
            "from": 2, "head": {"role": "user", "content": [{"type": "text", "text": "s"}]},
            "duration_ms": 5})),
    ]);
    let shown = transcript(&records(events), &Toolbox::default(), None);
    let kinds: Vec<&str> = shown
        .iter()
        .filter_map(|e| match e {
            AgentEvent::Retrying {
                discarded_chars, ..
            } => {
                assert_eq!(*discarded_chars, 4);
                Some("retrying")
            }
            AgentEvent::Fallback { .. } => Some("fallback"),
            AgentEvent::CacheMiss { .. } => Some("cache_miss"),
            AgentEvent::CacheMissRepeated { .. } => Some("cache_miss_repeated"),
            AgentEvent::SettingsChanged {
                endpoint, model, ..
            } => {
                assert_eq!(
                    (endpoint.as_str(), model.as_str()),
                    ("work", "claude-sonnet-5")
                );
                Some("settings")
            }
            AgentEvent::Compacted { before, after, .. } => {
                assert_eq!((*before, *after), (9000, 900));
                Some("compacted")
            }
            _ => None,
        })
        .collect();
    assert_eq!(
        kinds,
        [
            "retrying",
            "fallback",
            "cache_miss",
            "cache_miss_repeated",
            "settings",
            "compacted"
        ],
        "{shown:#?}"
    );
}

#[test]
fn each_resumed_run_reads_its_own_ids() {
    let mut events = opening(1, "tui-1", "first run");
    events.push(logged(
        json!({"type": "session_resumed", "wisp_version": "0.1.0",
        "endpoint": "https://api.anthropic.com", "endpoint_name": "env"}),
    ));
    events.extend(opening(2, "tui-1", "second run"));
    let shown = transcript(&records(events), &Toolbox::default(), None);
    let texts: Vec<&str> = pick!(&shown, AgentEvent::InputAccepted { text, .. } => text.as_str());
    assert_eq!(texts, ["first run", "second run"], "{shown:#?}");
    assert!(
        shown
            .iter()
            .any(|e| matches!(e, AgentEvent::SessionResumed { turns: 1, .. })),
        "{shown:#?}"
    );
}

fn rewound_to(turn: u64, dropped: &[u64], cache: serde_json::Value) -> LogEvent {
    logged(
        json!({"type": "rewound", "to": 1, "point": {"type": "turn", "turn": turn},
        "reason": "user", "messages": 2, "blocks": 1,
        "dropped": {"turns": dropped, "messages": 2 * dropped.len()}, "cache": cache}),
    )
}

fn rewinds(events: &[AgentEvent]) -> Vec<(u64, Vec<u64>, CacheEffect)> {
    events
        .iter()
        .filter_map(|e| match e {
            AgentEvent::Rewound {
                answers,
                point,
                dropped,
                returned,
                cache,
                ..
            } => {
                assert_eq!(*answers, None);
                assert!(returned.is_empty(), "{returned:?}");
                Some((point.turn(), dropped.turns.clone(), cache.clone()))
            }
            _ => None,
        })
        .collect()
}

#[test]
fn a_logged_rewind_is_replayed_with_its_cache_effect_and_without_refilling_the_composer() {
    let mut events = opening(1, "tui-1", "first");
    events.extend(opening(2, "tui-2", "second"));
    events.extend(opening(3, "tui-3", "third"));
    events.push(rewound_to(3, &[3], json!({"type": "kept"})));
    events.push(rewound_to(
        2,
        &[2],
        json!({"type": "rebuilt", "reason": "effort_change"}),
    ));
    let shown = transcript(&records(events), &Toolbox::default(), None);
    assert_eq!(
        rewinds(&shown),
        [
            (3, vec![3], CacheEffect::Kept),
            (
                2,
                vec![2],
                CacheEffect::Rebuilt {
                    reason: RebuildReason::EffortChange
                }
            ),
        ],
        "{shown:#?}"
    );
}

#[test]
fn a_rewind_or_a_rename_after_a_finished_reply_follows_its_step_end() {
    let mut events = opening(1, "tui-1", "first");
    events.push(logged(
        json!({"type": "step_ended", "turn": 1, "step": 1, "usage": null,
        "cost_usd": null, "lower_bound": false, "prompt_tokens": null, "response_id": null}),
    ));
    events.push(logged(
        json!({"type": "agent_renamed", "turn": 1, "agent": "/a", "to": "/b"}),
    ));
    events.push(rewound_to(1, &[1], json!({"type": "kept"})));
    let shown = transcript(&records(events), &Toolbox::default(), None);
    let at = |pick: fn(&AgentEvent) -> bool| shown.iter().position(pick);
    let ended = at(|e| matches!(e, AgentEvent::StepEnded { .. }));
    let renamed = at(|e| {
        matches!(e, AgentEvent::AgentRenamed { request: None, agent, to }
            if agent.as_str() == "/a" && to.as_str() == "/b")
    });
    let rewound = at(|e| matches!(e, AgentEvent::Rewound { .. }));
    assert!(
        ended.is_some() && ended < renamed && renamed < rewound,
        "{shown:#?}"
    );
}

#[test]
fn a_search_refusal_replays_as_the_feature_change_live_emitted() {
    let mut events = vec![logged(
        json!({"type": "session_started", "session_id": "s1", "wisp_version": "0",
            "model": "claude-opus-5-5", "effort": "medium", "cache_ttl": "one_hour",
            "system_prompt": null, "system_prompt_sha256": null,
            "endpoint": "https://gw.example.com", "credential_source": "environment"}),
    )];
    events.extend(opening(1, "tui-1", "go"));
    events.extend([
        logged(
            json!({"type": "native_web_search", "endpoint_name": "manifest",
            "state": "fallen_back"}),
        ),
        logged(
            json!({"type": "settings_changed", "model": "claude-opus-5-5",
            "effort": "medium", "cache": {"type": "rebuilt", "reason": "native_search_refused"},
            "endpoint_name": "manifest"}),
        ),
        logged(
            json!({"type": "settings_changed", "model": "claude-sonnet-5",
            "effort": "medium", "cache": {"type": "rebuilt", "reason": "model_switch"},
            "endpoint_name": "direct"}),
        ),
        logged(
            json!({"type": "native_web_search", "endpoint_name": "direct",
            "state": "fallen_back"}),
        ),
    ]);
    let shown = transcript(&records(events), &Toolbox::default(), None);
    let changes: Vec<(&str, &str, Feature, FeatureState)> = pick!(
        &shown,
        AgentEvent::FeatureStateChanged {
            endpoint,
            model,
            feature,
            state,
        } => (endpoint.as_str(), model.as_str(), *feature, *state)
    );
    assert_eq!(
        changes,
        [
            (
                "manifest",
                "claude-opus-5-5",
                Feature::NativeWebSearch,
                FeatureState::FallenBack
            ),
            (
                "direct",
                "claude-sonnet-5",
                Feature::NativeWebSearch,
                FeatureState::FallenBack
            ),
        ],
        "{shown:#?}"
    );
    let order: Vec<usize> = shown
        .iter()
        .enumerate()
        .filter(|(_, e)| {
            matches!(
                e,
                AgentEvent::FeatureStateChanged { .. } | AgentEvent::SettingsChanged { .. }
            )
        })
        .map(|(i, _)| i)
        .collect();
    assert!(matches!(
        shown[order[0]],
        AgentEvent::FeatureStateChanged { .. }
    ));
    assert!(matches!(
        shown[order[1]],
        AgentEvent::SettingsChanged { .. }
    ));
}

#[test]
fn a_replayed_refusal_takes_the_model_its_turn_ran_on() {
    let mut events = vec![logged(
        json!({"type": "session_started", "session_id": "s1", "wisp_version": "0",
            "model": "claude-opus-5-5", "effort": "medium", "cache_ttl": "one_hour",
            "system_prompt": null, "system_prompt_sha256": null,
            "endpoint": "https://gw.example.com", "credential_source": "environment"}),
    )];
    events.push(started(
        json!({"turn": 1, "text": "go", "input_id": "tui-1", "model": "claude-sonnet-5"}),
    ));
    events.extend([
        logged(
            json!({"type": "settings_changed", "model": "claude-haiku-5-5",
            "effort": "medium", "cache": {"type": "rebuilt", "reason": "model_switch"},
            "endpoint_name": "manifest"}),
        ),
        logged(
            json!({"type": "native_web_search", "endpoint_name": "manifest",
            "state": "fallen_back"}),
        ),
    ]);
    let shown = transcript(&records(events), &Toolbox::default(), None);
    let models: Vec<&str> = pick!(
        &shown,
        AgentEvent::FeatureStateChanged { model, .. } => model.as_str()
    );
    assert_eq!(models, ["claude-sonnet-5"], "{shown:#?}");
}

#[test]
fn a_lowered_window_is_replayed_inside_its_turn() {
    let lowered = LogEvent::WindowLowered {
        endpoint: "manifest".into(),
        model: "claude-opus-5-5".into(),
        tokens: 200_000,
    };
    let log = records(vec![
        started(json!({"turn": 1, "text": "go"})),
        lowered,
        failed(1, None),
    ]);
    let events = transcript(&log, &Toolbox::default(), None);
    let at = |pick: &dyn Fn(&AgentEvent) -> bool| events.iter().position(pick).unwrap();
    let lowered = at(&|e| {
        matches!(
            e,
            AgentEvent::WindowLowered { endpoint, model, tokens: 200_000 }
                if endpoint == "manifest" && model == "claude-opus-5-5"
        )
    });
    let started = at(&|e| matches!(e, AgentEvent::TurnStarted { .. }));
    let ended = at(&|e| matches!(e, AgentEvent::TurnEnded { .. }));
    assert!(started < lowered && lowered < ended, "{events:#?}");
}

fn asked_in(turn: u64, call_id: &str, questions: &[wisp_protocol::Question]) -> Vec<LogEvent> {
    let input = json!({"questions": questions});
    vec![
        LogEvent::ToolCallRequested {
            turn,
            step: 1,
            block: 0,
            call_id: call_id.to_owned(),
            tool: wisp_plugin_ask_user::NAME.to_owned(),
            input,
            display: crate::plugin::convert::display(wisp_plugin_ask_user::display(
                &questions
                    .iter()
                    .map(crate::plugin::convert::question_to_api)
                    .collect::<Vec<_>>(),
            ))
            .unwrap(),
        },
        LogEvent::ToolCallStarted {
            turn,
            call_id: call_id.to_owned(),
            cwd: "~/wisp".to_owned(),
            movable: false,
            file: None,
        },
        LogEvent::QuestionAsked {
            turn,
            call_id: call_id.to_owned(),
            questions: questions.to_vec(),
        },
    ]
}

#[test]
fn an_answered_and_an_interrupted_question_are_rebuilt() {
    let wisp_plugin_ask_user::Call::Ask(parsed) =
        wisp_plugin_ask_user::parse(&json!({"questions": [
            {"header": "Colour", "question": "Which colour should the button be?", "options": [
                {"label": "Red", "description": "The warm one."},
                {"label": "Blue", "description": "The cool one."},
            ]},
        ]}))
        .unwrap()
    else {
        panic!("expected questions");
    };
    let questions: Vec<wisp_protocol::Question> = parsed
        .into_iter()
        .map(crate::plugin::convert::question)
        .collect();
    let answered = wisp_protocol::QuestionEnd::Answered {
        answers: vec![wisp_protocol::Answer::choose(1)],
    };
    let mut events = vec![started(json!({"turn": 1, "text": "pick"}))];
    events.extend(asked_in(1, "toolu_a", &questions));
    events.push(LogEvent::QuestionClosed {
        turn: 1,
        call_id: "toolu_a".to_owned(),
        how: answered.clone(),
    });
    events.push(LogEvent::ToolCallFinished {
        turn: 1,
        call_id: "toolu_a".to_owned(),
        outcome: wisp_protocol::ToolOutcome::Completed {
            is_error: false,
            exit: None,
        },
        output: wisp_protocol::ToolOutputView::default(),
        facts: Vec::new(),
        state: None,
        duration_ms: 900,
        result: Some(crate::tool::question::result_text(&questions, &answered)),
        process: None,
        attachments: Vec::new(),
        notes: None,
        typed: None,
    });
    events.push(started(json!({"turn": 2, "text": "again"})));
    events.extend(asked_in(2, "toolu_b", &questions));
    events.push(LogEvent::QuestionClosed {
        turn: 2,
        call_id: "toolu_b".to_owned(),
        how: wisp_protocol::QuestionEnd::Interrupted,
    });
    let shown = transcript(&records(events), &Toolbox::default(), None);
    let questions_of = |id: &str| {
        shown.iter().find_map(|e| match e {
            AgentEvent::QuestionAsked { call_id, questions } if call_id == id => {
                Some(questions.clone())
            }
            _ => None,
        })
    };
    let end_of = |id: &str| {
        shown
            .iter()
            .filter_map(|e| match e {
                AgentEvent::QuestionClosed { call_id, how } if call_id == id => Some(how.clone()),
                _ => None,
            })
            .collect::<Vec<_>>()
    };
    assert_eq!(questions_of("toolu_a").as_deref(), Some(&questions[..]));
    assert_eq!(questions_of("toolu_b").as_deref(), Some(&questions[..]));
    assert_eq!(end_of("toolu_a"), [answered]);
    assert_eq!(end_of("toolu_b"), [wisp_protocol::QuestionEnd::Interrupted]);
    let finished = shown
        .iter()
        .filter(
            |e| matches!(e, AgentEvent::ToolCallFinished { call_id, .. } if call_id == "toolu_a"),
        )
        .count();
    assert_eq!(finished, 1, "{shown:#?}");
    let at = |pick: &dyn Fn(&AgentEvent) -> bool| shown.iter().position(pick).unwrap();
    let requested =
        at(&|e| matches!(e, AgentEvent::ToolCallRequested { call_id, .. } if call_id == "toolu_b"));
    let asked =
        at(&|e| matches!(e, AgentEvent::QuestionAsked { call_id, .. } if call_id == "toolu_b"));
    let closed =
        at(&|e| matches!(e, AgentEvent::QuestionClosed { call_id, .. } if call_id == "toolu_b"));
    assert!(requested < asked && asked < closed, "{shown:#?}");
}

fn requested_record(call_id: &str, tool: &str, purpose: Option<&str>) -> LogEvent {
    let mut display = json!({"action": {"type": "show"}, "target": "Rename the key",
        "output": {"type": "none"}, "view": {"type": "diff", "files": []}});
    if let Some(purpose) = purpose {
        display["purpose"] = json!(purpose);
    }
    logged(
        json!({"type": "tool_call_requested", "turn": 1, "step": 1, "block": 0,
        "call_id": call_id, "tool": tool, "input": {}, "display": display}),
    )
}

fn purposes(shown: &[AgentEvent]) -> Vec<(String, wisp_protocol::ToolPurpose)> {
    pick!(
        shown,
        AgentEvent::ToolCallRequested {
            call_id, display, ..
        } => (call_id.clone(), display.purpose)
    )
}

#[test]
fn a_replayed_call_takes_its_purpose_from_the_tool_as_built_now() {
    use wisp_protocol::ToolPurpose::{Run, Show};

    let mut events = opening(1, "a", "show me");
    events.extend([
        requested_record("old-diff", wisp_plugin_show_diff::NAME, None),
        requested_record("asked", wisp_plugin_ask_user::NAME, Some("show")),
        requested_record("gone", "a_plugin_tool", Some("show")),
        requested_record("newer", "a_plugin_tool", Some("a_later_purpose")),
    ]);
    let log = records(events);
    let built = crate::tool::testkit::bundled(&["ask_user", "show_diff"]);
    let pair = |id: &str, purpose| (id.to_owned(), purpose);
    assert_eq!(
        purposes(&transcript(&log, &built, None)),
        [
            pair("old-diff", Show),
            pair("asked", Run),
            pair("gone", Show),
            pair("newer", Run),
        ]
    );
    assert_eq!(
        purposes(&transcript(&log, &Toolbox::default(), None)),
        [
            pair("old-diff", Run),
            pair("asked", Show),
            pair("gone", Show),
            pair("newer", Run),
        ]
    );
}

fn settings_record(changes: serde_json::Value) -> LogEvent {
    logged(
        json!({"type": "settings_changed", "model": "claude-sonnet-5",
        "effort": "high", "cache": {"type": "kept"}, "endpoint_name": "work",
        "changes": changes}),
    )
}

fn client_change(name: &str, kind: &str, text: &str) -> serde_json::Value {
    json!({"target": {"name": name}, "value": {"kind": kind, "text": text},
        "layer": "global", "when": "now",
        "saved": {"path_display": "~/.wisp/config.toml"}})
}

fn settings_shown(events: Vec<LogEvent>) -> usize {
    let mut all = opening(1, "tui-1", "go");
    all.extend(events);
    transcript(&records(all), &Toolbox::default(), None)
        .iter()
        .filter(|e| matches!(e, AgentEvent::SettingsChanged { .. }))
        .count()
}

#[test]
fn a_record_of_only_client_owned_changes_draws_no_settings_note_on_resume() {
    let fold = |list: &str| {
        settings_record(json!([client_change(
            "display.sidebar_folded",
            "list",
            list
        )]))
    };
    assert_eq!(
        settings_shown(vec![
            fold("testkit/busy"),
            fold(""),
            fold("context/window,testkit/busy"),
        ]),
        0
    );
    assert_eq!(
        settings_shown(vec![settings_record(json!([client_change(
            "display.theme",
            "text",
            "light"
        )]))]),
        0
    );
}

#[test]
fn a_record_with_no_changes_or_a_kernel_change_still_draws_its_note_on_resume() {
    let bare = logged(
        json!({"type": "settings_changed", "model": "claude-sonnet-5",
        "effort": "high", "cache": {"type": "rebuilt", "reason": "endpoint_switch"},
        "endpoint_name": "direct"}),
    );
    assert_eq!(settings_shown(vec![bare]), 1);
    let mixed = settings_record(json!([
        client_change("display.theme", "text", "light"),
        {"target": {"name": "cache_ttl"}, "value": {"kind": "text", "text": "5m"},
            "layer": "session", "when": "next_turn"},
    ]));
    assert_eq!(settings_shown(vec![mixed]), 1);
}

#[test]
fn a_busy_retry_replays_its_wait() {
    let mut events = opening(1, "tui-1", "go");
    events.extend([
        logged(
            json!({"type": "retrying", "turn": 1, "attempt": 7, "delay_ms": 30000,
            "reason": "HTTP 429: Too many concurrent requests", "step": 1, "kind": "rate_limit",
            "busy": {"waited_ms": 61000, "window_ms": 600000}}),
        ),
        logged(
            json!({"type": "retrying", "turn": 1, "attempt": 8, "delay_ms": 30000,
            "reason": "HTTP 500", "step": 1, "kind": "server"}),
        ),
    ]);
    let shown = transcript(&records(events), &Toolbox::default(), None);
    let busy: Vec<Option<wisp_protocol::BusyWait>> = pick!(
        &shown,
        AgentEvent::Retrying { busy, .. } => *busy
    );
    assert_eq!(
        busy,
        [
            Some(wisp_protocol::BusyWait {
                waited_ms: 61_000,
                window_ms: 600_000,
            }),
            None
        ],
        "{shown:#?}"
    );
}

#[test]
fn a_logged_tool_change_is_replayed_without_its_cache_effect() {
    let mut events = opening(2, "u2", "two");
    events.push(logged(json!({"type": "tools_changed", "turn": 2,
        "via": "inline", "added": ["show_diff"], "removed": ["view_file"]})));
    let replayed = transcript(&records(events), &Toolbox::default(), None);
    let changed: Vec<&AgentEvent> = replayed
        .iter()
        .filter(|e| matches!(e, AgentEvent::ToolsChanged { .. }))
        .collect();
    assert_eq!(
        changed,
        [&AgentEvent::ToolsChanged {
            added: vec!["show_diff".to_owned()],
            removed: vec!["view_file".to_owned()],
            via: wisp_protocol::ToolChangeVia::Inline,
            cache: None,
        }]
    );
}

#[test]
fn a_logged_plugins_change_inside_a_turn_replays_its_lines_without_an_extra_opening() {
    let mut events = opening(2, "u2", "two");
    events.push(assistant(vec![ContentBlock::Text {
        text: "Placeholder reply.".into(),
    }]));
    events.push(logged(json!({"type": "plugins_changed", "turn": 2,
        "removed": [{"id": "todos", "version": "0.1.0", "source": {"type": "bundled"}}],
        "kept": [{"id": "board", "version": "0.1.0", "source": {"type": "bundled"}}],
        "notes": [{"plugin": "board", "message": "failed to start"}, {"plugin":"mcp", "message":"/review/coder added server local"}]})));
    let replayed = transcript(&records(events), &Toolbox::default(), None);
    assert_eq!(
        replayed
            .iter()
            .filter(|event| matches!(event, AgentEvent::TurnStarted { .. }))
            .count(),
        1
    );
    let record = |id: &str| wisp_protocol::plugin::PluginRecord {
        id: id.to_owned(),
        version: "0.1.0".to_owned(),
        source: wisp_protocol::plugin::PluginSource::Bundled,
    };
    let changed: Vec<&AgentEvent> = replayed
        .iter()
        .filter(|e| matches!(e, AgentEvent::PluginsChanged { .. }))
        .collect();
    assert_eq!(
        changed,
        [&AgentEvent::PluginsChanged {
            added: Vec::new(),
            updated: Vec::new(),
            removed: vec![record("todos")],
            kept: vec![record("board")],
            notes: vec![
                wisp_protocol::plugin::PluginNote {
                    plugin: "board".to_owned(),
                    message: "failed to start".to_owned(),
                },
                wisp_protocol::plugin::PluginNote {
                    plugin: "mcp".into(),
                    message: "/review/coder added server local".into()
                }
            ],
            links: None,
        }]
    );
}

fn compacted_record(epoch: u64, requested_by: Option<&str>) -> LogEvent {
    let mut value = json!({"type": "context_compacted", "turn": 1, "epoch": epoch,
        "trigger": "auto", "engine": "wisp", "before": 9000, "after": 900,
        "from": 2, "head": {"role": "user", "content": [{"type": "text", "text": "s"}]},
        "duration_ms": 5});
    if let Some(hook) = requested_by {
        value["requested_by"] = json!(hook);
    }
    logged(value)
}

fn failed_record(kind: serde_json::Value, requested_by: Option<&str>) -> LogEvent {
    let mut value = json!({"type": "compaction_failed", "turn": 1, "trigger": "auto",
        "engine": "native", "kind": kind, "message": "Blocked by aa/gate: not now"});
    if let Some(hook) = requested_by {
        value["requested_by"] = json!(hook);
    }
    logged(value)
}

#[test]
fn a_requests_lines_come_back_on_resume_and_the_cores_compactions_add_nothing() {
    let mut events = opening(1, "tui-1", "go");
    events.extend([
        compacted_record(1, None),
        compacted_record(2, Some("aa/ask")),
        failed_record(json!("check_failed"), Some("aa/ask")),
        failed_record(json!("blocked"), Some("aa/ask")),
        failed_record(json!("check_failed"), None),
    ]);
    let shown = transcript(&records(events), &Toolbox::default(), None);
    let kinds: Vec<String> = shown
        .iter()
        .filter_map(|e| match e {
            AgentEvent::CompactionStarted { requested_by, .. } => {
                Some(format!("started {requested_by:?}"))
            }
            AgentEvent::Compacted { .. } => Some("compacted".to_owned()),
            AgentEvent::CompactionFailed {
                kind,
                requested_by,
                paused,
                ..
            } => Some(format!("failed {kind:?} {requested_by:?} {paused}")),
            _ => None,
        })
        .collect();
    assert_eq!(
        kinds,
        [
            "compacted",
            "started Some(\"aa/ask\")",
            "compacted",
            "started Some(\"aa/ask\")",
            "failed CheckFailed Some(\"aa/ask\") false",
            "started Some(\"aa/ask\")",
            "failed Blocked Some(\"aa/ask\") false",
        ],
        "{shown:#?}"
    );
}

fn feature_changes(record: serde_json::Value) -> Vec<(String, String, Feature, FeatureState)> {
    let mut events = vec![logged(
        json!({"type": "session_started", "session_id": "s1", "wisp_version": "0",
            "model": "claude-opus-5-5", "effort": "medium", "cache_ttl": "one_hour",
            "system_prompt": null, "system_prompt_sha256": null,
            "endpoint": "https://gw.example.com", "credential_source": "environment"}),
    )];
    events.push(started(
        json!({"turn": 1, "text": "go", "input_id": "tui-1", "model": "claude-sonnet-5"}),
    ));
    events.push(logged(record));
    pick!(
        transcript(&records(events), &Toolbox::default(), None),
        AgentEvent::FeatureStateChanged {
            endpoint,
            model,
            feature,
            state,
        } => (endpoint, model, feature, state)
    )
}

fn replays_as_logged(kind: &str, feature: Feature, state: FeatureState) {
    let logged_state = serde_json::to_value(state).unwrap();
    let changes = feature_changes(json!({"type": kind, "model": "claude-haiku-5-5",
        "state": logged_state, "endpoint_name": "gw"}));
    assert_eq!(
        changes,
        [(
            "gw".to_owned(),
            "claude-haiku-5-5".to_owned(),
            feature,
            state
        )]
    );
}

#[test]
fn a_per_message_effort_record_replays_as_the_feature_change_live_emitted() {
    replays_as_logged(
        "per_message_effort",
        Feature::PerMessageEffort,
        FeatureState::Proven,
    );
}

#[test]
fn a_binding_controls_record_replays_as_the_feature_change_live_emitted() {
    replays_as_logged(
        "binding_controls",
        Feature::BindingControls,
        FeatureState::FallenBack,
    );
}

#[test]
fn a_native_compaction_record_replays_as_the_feature_change_live_emitted() {
    replays_as_logged(
        "native_compaction",
        Feature::NativeCompaction,
        FeatureState::Proven,
    );
}

#[test]
fn an_inline_tools_record_replays_as_the_feature_change_live_emitted() {
    replays_as_logged("inline_tools", Feature::InlineTools, FeatureState::Proven);
}

#[test]
fn a_deferred_tools_record_replays_as_the_feature_change_live_emitted() {
    replays_as_logged(
        "deferred_tools",
        Feature::DeferredTools,
        FeatureState::FallenBack,
    );
}

#[test]
fn a_deferred_tools_hold_replays_as_refused_like_a_real_refusal() {
    let mut events = vec![logged(
        json!({"type": "session_started", "session_id": "s1", "wisp_version": "0",
            "model": "claude-opus-5-5", "effort": "medium", "cache_ttl": "one_hour",
            "system_prompt": null, "system_prompt_sha256": null,
            "endpoint": "https://gw.example.com", "credential_source": "environment"}),
    )];
    events.extend(opening(1, "tui-1", "go"));
    events.extend([
        logged(json!({"type": "inline_tools", "model": "claude-opus-5-5",
            "state": "proven", "endpoint_name": "gw"})),
        logged(json!({"type": "deferred_tools", "model": "claude-opus-5-5",
            "state": "fallen_back", "endpoint_name": "gw"})),
    ]);
    let shown = transcript(&records(events), &Toolbox::default(), None);
    let deferred: Vec<FeatureState> = pick!(
        &shown,
        AgentEvent::FeatureStateChanged {
            feature: Feature::DeferredTools,
            state,
            ..
        } => *state
    );
    assert_eq!(deferred, [FeatureState::FallenBack], "{shown:#?}");
}

#[test]
fn a_feature_record_without_an_endpoint_replays_nothing() {
    let changes = feature_changes(json!({"type": "per_message_effort",
        "model": "claude-haiku-5-5", "state": "proven"}));
    assert!(changes.is_empty(), "{changes:?}");
}

#[test]
fn a_cited_openai_reply_replays_each_mark_after_its_span() {
    let pieces = [
        "A — ([a](https://example.com/a)).",
        " B ([b](https://example.com/b)).",
        " C ([a](https://example.com/a)).",
        " D ([x](https://example.com/x)).",
    ];
    let urls = [
        "https://example.com/a",
        "https://example.com/b",
        "https://example.com/a",
        "https://example.com/x",
    ];
    let text = pieces.concat();
    let mut parts = Vec::new();
    for half in [0, 2] {
        let mut annotations = Vec::new();
        let mut at = 0;
        for (piece, url) in pieces[half..half + 2].iter().zip(&urls[half..]) {
            let chars = piece.chars().count();
            let span = if url.ends_with('x') {
                (2, 24)
            } else {
                (at + 2, at + chars)
            };
            annotations.push(json!({"type":"url_citation","start_index":span.0,"end_index":span.1,"url":url,"title":"Page"}));
            at += chars;
        }
        let part = pieces[half..half + 2].concat();
        parts.push(json!({"type":"output_text","text":part,"annotations":annotations}));
    }
    let envelope = |item: serde_json::Value, projections: u64| {
        ContentBlock::opaque(
            "openai",
            json!({"type":"wisp_native_item","item":item,"projection_blocks":projections,"identity":"id"}),
        )
    };
    let search = json!({"type":"web_search_call","id":"ws_1","status":"completed","action":{"type":"search","query":"q"}});
    let message = json!({"type":"message","content":parts});
    let cited = |url: &str| LogEvent::Citations {
        turn: 1,
        step: 1,
        attempt: 0,
        index: 3,
        sources: vec![CitedSource {
            url: url.to_owned(),
            title: None,
        }],
    };
    let mut log = opening(1, "tui-1", "search");
    log.extend(urls.map(cited));
    log.push(assistant(vec![
        envelope(search, 0),
        envelope(message, 1),
        self::text(&text),
    ]));
    let shown: Vec<String> = transcript(&records(log), &Toolbox::default(), None)
        .into_iter()
        .filter_map(|e| match e {
            AgentEvent::TextDelta { index: 3, text } => Some(text),
            AgentEvent::Citations {
                index: 3, sources, ..
            } => Some(format!("[{}]", sources[0].url)),
            AgentEvent::BlockStopped { index: 3 } => Some("|".to_owned()),
            _ => None,
        })
        .collect();
    let [a, b, c, d] = pieces;
    let expected = [
        a,
        "[https://example.com/a]",
        b,
        "[https://example.com/b]",
        c,
        "[https://example.com/a]",
        d,
        "|",
        "[https://example.com/x]",
    ];
    assert_eq!(shown, expected);
}
