#![allow(
    clippy::unwrap_used,
    clippy::panic,
    reason = "fixture helpers fail the test on any read, write or parse error"
)]

mod support;

use support::golden;
use wisp_protocol::log::{
    ImagesOnly, LOG_SCHEMA_VERSION, LogEvent, LogRecord, RebuildError, Rebuilt, TurnLead,
    no_images, rebuild_history, rebuild_log_with,
};
use wisp_protocol::{
    AgentEvent, CacheEffect, ClientCommand, CompactionNote, ContentBlock, Dropped, Effort,
    ImageData, InputId, Message, PointInfo, RebuildReason, RequestId, ReturnedInput, RewindPoint,
    RewindQuote, RewindReason, Role, Warmth,
};

fn started(turn: u64, text: &str) -> LogEvent {
    LogEvent::TurnStarted {
        turn,
        origin: None,
        lead: TurnLead::default(),
        text: text.into(),
        model: None,
        effort: None,
        effort_marker: None,
        images: Vec::new(),
        endpoint_name: None,
        input_id: None,
        inputs: Vec::new(),
        cache_ttl: None,
        debug_cache: None,
        refusal_fallback: None,
        binding_controls: None,
        retry_of: None,
        kept_tools_sha256: None,
        ends: Vec::new(),
        tool_message: Vec::new(),
    }
}

fn answered(turn: u64, text: &str) -> LogEvent {
    LogEvent::MessageCommitted {
        turn,
        message: Message {
            role: Role::Assistant,
            content: vec![ContentBlock::Text { text: text.into() }],
        }
        .into(),
        model: None,
        stop_reason: None,
    }
}

fn rewound(to: u64, turn: u64, messages: usize, dropped: &[u64]) -> LogEvent {
    LogEvent::Rewound {
        to,
        point: RewindPoint::Turn { turn },
        reason: RewindReason::User,
        messages,
        blocks: usize::from(messages > 0),
        dropped: Dropped {
            turns: dropped.to_vec(),
            messages: 2 * dropped.len(),
        },
        cache: CacheEffect::Kept,
    }
}

fn turns(texts: &[(u64, &str)]) -> Vec<LogEvent> {
    texts
        .iter()
        .flat_map(|(turn, text)| [started(*turn, text), answered(*turn, text)])
        .collect()
}

fn rebuild(events: Vec<LogEvent>) -> Result<Rebuilt, RebuildError> {
    let records: Vec<LogRecord> = events
        .into_iter()
        .zip(0..)
        .map(|(event, seq)| LogRecord {
            schema_version: LOG_SCHEMA_VERSION,
            seq,
            at: "2026-10-25T12:00:00Z".into(),
            event,
        })
        .collect();
    rebuild_log_with(records.iter(), ImagesOnly(no_images))
}

fn texts(rebuilt: &Rebuilt) -> Vec<String> {
    rebuilt
        .messages
        .iter()
        .flat_map(|m| &m.content)
        .filter_map(|b| match b {
            ContentBlock::Text { text } => Some(text.clone()),
            _ => None,
        })
        .collect()
}

#[test]
fn a_rewind_masks_the_turns_it_left() {
    let mut events = turns(&[(1, "one"), (2, "two"), (3, "three")]);
    events.push(rewound(1, 2, 2, &[2, 3]));
    events.extend(turns(&[(4, "two b")]));
    let rebuilt = rebuild(events).unwrap();
    assert_eq!(texts(&rebuilt), ["one", "one", "two b", "two b"]);
}

#[test]
fn a_rewind_inside_a_later_rewinds_range_is_masked_without_its_check() {
    let mut events = turns(&[(1, "one"), (2, "two"), (3, "three")]);
    events.push(rewound(3, 3, 999, &[3]));
    events.extend(turns(&[(4, "three b")]));
    events.push(rewound(1, 2, 2, &[2, 4]));
    events.extend(turns(&[(5, "two b")]));
    let rebuilt = rebuild(events).unwrap();
    assert_eq!(texts(&rebuilt), ["one", "one", "two b", "two b"]);
}

#[test]
fn a_rewind_into_a_range_an_earlier_rewind_left_fails_cleanly() {
    let mut events = turns(&[(1, "one"), (2, "two"), (3, "three")]);
    events.push(rewound(1, 2, 2, &[2, 3]));
    events.extend(turns(&[(4, "two b")]));
    events.push(rewound(3, 3, 4, &[4]));
    let error = rebuild(events).unwrap_err();
    assert!(
        error.reason.contains("inside the turns"),
        "{}",
        error.reason
    );
}

#[test]
fn a_tampered_rewind_fails_its_check() {
    let mut events = turns(&[(1, "one"), (2, "two")]);
    events.push(rewound(1, 2, 3, &[2]));
    let error = rebuild(events).unwrap_err();
    assert_eq!(error.turn, 2);
    assert!(
        error.reason.contains("keeps 3 messages"),
        "{}",
        error.reason
    );
}

#[test]
fn a_rewind_that_keeps_a_later_record_fails() {
    let mut events = turns(&[(1, "one")]);
    events.push(rewound(5, 1, 0, &[1]));
    assert!(rebuild(events).is_err());
}

#[test]
fn a_log_without_rewinds_rebuilds_unchanged() {
    let events = turns(&[(1, "one"), (2, "two")]);
    let rebuilt = rebuild(events).unwrap();
    assert_eq!(texts(&rebuilt), ["one", "one", "two", "two"]);
}

#[test]
fn an_unknown_reason_reads_as_text() {
    let json = r#"{"type":"rewound","to":1,"point":{"type":"turn","turn":2},"reason":"by_a_script","messages":2,"blocks":1,"dropped":{"turns":[2],"messages":2},"cache":{"type":"kept"}}"#;
    let event: LogEvent = serde_json::from_str(json).unwrap();
    assert!(matches!(
        event,
        LogEvent::Rewound {
            reason: RewindReason::Other(ref r),
            ..
        } if r == "by_a_script"
    ));
}

#[test]
fn the_rewind_types_match_their_fixtures() {
    golden("rewound_record", &rewound(1, 2, 2, &[2, 3]));
    golden(
        "rewind_command",
        &ClientCommand::Rewind {
            to: RewindPoint::Turn { turn: 2 },
            request: RequestId(7),
        },
    );
    golden(
        "rewind_points_command",
        &ClientCommand::RewindPoints {
            request: RequestId(6),
        },
    );
    let quote = |warm| RewindQuote {
        prefix_tokens: 12_000,
        warm,
        read_micros: Some(6_000),
        rewrite_micros: Some(120_000),
    };
    golden(
        "rewind_points_event",
        &AgentEvent::RewindPoints {
            answers: RequestId(6),
            points: vec![
                PointInfo {
                    point: RewindPoint::Turn { turn: 1 },
                    label: "first".into(),
                    at: "2026-10-25T12:00:00Z".into(),
                    inputs_preview: vec!["first".into()],
                    quote: quote(Warmth::PrefixChanged),
                    compaction: Some(CompactionNote {
                        turn: 3,
                        compacts_first: false,
                    }),
                    processes: vec![2],
                },
                PointInfo {
                    point: RewindPoint::Turn { turn: 2 },
                    label: "second".into(),
                    at: "2026-10-25T12:01:00Z".into(),
                    inputs_preview: Vec::new(),
                    quote: quote(Warmth::OtherEffort {
                        effort: Effort::High,
                    }),
                    compaction: None,
                    processes: Vec::new(),
                },
            ],
        },
    );
    golden(
        "rewound_event",
        &AgentEvent::Rewound {
            answers: Some(RequestId(7)),
            point: RewindPoint::Turn { turn: 2 },
            reason: RewindReason::User,
            dropped: Dropped {
                turns: vec![2, 3],
                messages: 4,
            },
            returned: vec![ReturnedInput {
                id: InputId("tui-2".into()),
                text: "second".into(),
                images: vec![ImageData {
                    data: "aW1hZ2U=".into(),
                }],
                missing: vec![wisp_protocol::MissingImage {
                    ordinal: 2,
                    sha256: "ab".repeat(32),
                }],
            }],
            cache: CacheEffect::Rebuilt {
                reason: RebuildReason::Rewind,
            },
        },
    );
    let cold = match rewound(1, 2, 2, &[2, 3]) {
        LogEvent::Rewound {
            to,
            point,
            reason,
            messages,
            blocks,
            dropped,
            ..
        } => LogEvent::Rewound {
            to,
            point,
            reason,
            messages,
            blocks,
            dropped,
            cache: CacheEffect::Rebuilt {
                reason: RebuildReason::ModelSwitch,
            },
        },
        other => other,
    };
    golden("rewound_record_cold", &cold);
}

fn on(turn: u64, text: &str) -> LogEvent {
    match started(turn, text) {
        LogEvent::TurnStarted {
            turn,
            origin,
            lead,
            text,
            effort_marker,
            images,
            input_id,
            inputs,
            cache_ttl,
            debug_cache,
            refusal_fallback,
            binding_controls,
            retry_of,
            kept_tools_sha256,
            ends,
            ..
        } => LogEvent::TurnStarted {
            turn,
            origin,
            lead,
            text,
            model: Some("claude-opus-5-5".into()),
            effort: Some(Effort::Medium),
            effort_marker,
            images,
            endpoint_name: Some("direct".into()),
            input_id,
            inputs,
            cache_ttl,
            debug_cache,
            refusal_fallback,
            binding_controls,
            retry_of,
            kept_tools_sha256,
            ends,
            tool_message: Vec::new(),
        },
        other => other,
    }
}

fn step(turn: u64, view_len: usize, at: &str) -> LogEvent {
    LogEvent::StepEnded {
        list_usd: None,
        model: None,
        turn,
        step: 1,
        usage: Some(wisp_protocol::Usage::default()),
        cost_usd: None,
        lower_bound: false,
        prompt_tokens: Some(5_000),
        response_id: None,
        window: None,
        compact_at: None,
        used: None,
        first_pass: None,
        accepted: true,
        view_len: Some(view_len),
        started_at: Some(at.into()),
        ttl: Some(wisp_protocol::CacheTtl::OneHour),
        prefix: None,
        queued_ms: None,
        thread: None,
        thread_error: None,
        betas: Vec::new(),
    }
}

fn compacted(epoch: u64) -> LogEvent {
    serde_json::from_value(serde_json::json!({
        "type": "context_compacted",
        "epoch": epoch,
        "trigger": "manual",
        "engine": "wisp",
        "before": 1,
        "after": 1,
        "duration_ms": 1,
        "from": 0,
        "head": {"role": "user", "content": [{"type": "text", "text": "summary"}]},
    }))
    .unwrap()
}

fn refreshed_after(masked: Vec<LogEvent>) -> Option<String> {
    let mut events = vec![
        on(1, "one"),
        answered(1, "one"),
        step(1, 1, "2026-10-25T12:00:00Z"),
        on(2, "two"),
        answered(2, "two"),
        step(2, 3, "2026-10-25T12:10:00Z"),
        on(3, "three"),
    ];
    events.extend(masked);
    events.push(answered(3, "three"));
    events.push(rewound(5, 3, 4, &[3]));
    let rebuilt = rebuild(events).unwrap();
    let point = rebuilt.points.iter().find(|p| p.turn == 2).unwrap();
    assert!(point.baseline.is_some());
    point.refreshed.last().map(|(at, _)| at.clone())
}

#[test]
fn a_masked_accepted_request_refreshes_the_points_baseline_in_its_epoch_only() {
    let later = "2026-10-25T12:30:00Z";
    let before = refreshed_after(Vec::new());
    assert_ne!(before.as_deref(), Some(later));
    assert_eq!(
        refreshed_after(vec![step(2, 3, later)]).as_deref(),
        Some(later)
    );
    assert_eq!(
        refreshed_after(vec![compacted(1), step(2, 3, later)]),
        before
    );
}

#[test]
fn a_masked_request_on_another_model_leaves_the_points_baseline_alone() {
    let mut other = on(3, "three");
    if let LogEvent::TurnStarted { model, .. } = &mut other {
        *model = Some("claude-sonnet-5-5".into());
    }
    let before = refreshed_after(Vec::new());
    let after = refreshed_after_with(other, vec![step(3, 5, "2026-10-25T12:30:00Z")]);
    assert_eq!(after, before);
}

fn refreshed_after_with(third: LogEvent, masked: Vec<LogEvent>) -> Option<String> {
    let mut events = vec![
        on(1, "one"),
        answered(1, "one"),
        step(1, 1, "2026-10-25T12:00:00Z"),
        on(2, "two"),
        answered(2, "two"),
        step(2, 3, "2026-10-25T12:10:00Z"),
        third,
    ];
    events.extend(masked);
    events.push(answered(3, "three"));
    events.push(rewound(5, 3, 4, &[3]));
    let rebuilt = rebuild(events).unwrap();
    let point = rebuilt.points.iter().find(|p| p.turn == 2).unwrap();
    point.refreshed.last().map(|(at, _)| at.clone())
}

fn masked_after_two(masked: Vec<LogEvent>) -> Rebuilt {
    let mut events = turns(&[(1, "one"), (2, "two")]);
    events.push(started(3, "three"));
    events.extend(masked);
    events.push(answered(3, "three"));
    events.push(rewound(3, 3, 4, &[3]));
    rebuild(events).unwrap()
}

fn cut(from: (usize, usize), through: usize) -> LogEvent {
    LogEvent::ThinkingCut {
        from: wisp_protocol::log::BlockPosition {
            message: from.0,
            block: from.1,
        },
        through,
        tools_sha256: Some("tools".into()),
    }
}

#[test]
fn a_masked_tools_cut_inside_the_kept_history_is_kept_and_clamped_to_it() {
    let rebuilt = masked_after_two(vec![cut((1, 0), 9)]);
    assert_eq!(rebuilt.cuts.len(), 1);
    assert_eq!(rebuilt.cuts[0].through, 3);
    assert!(rebuilt.cuts[0].tools);
}

#[test]
fn a_masked_tools_cut_past_the_kept_history_is_dropped() {
    let rebuilt = masked_after_two(vec![cut((4, 0), 5)]);
    assert!(rebuilt.cuts.is_empty(), "{:?}", rebuilt.cuts);
}

#[test]
fn a_masked_search_fallback_stays_refused() {
    let refusal: LogEvent = serde_json::from_value(serde_json::json!({
        "type": "native_web_search",
        "endpoint_name": "direct",
        "state": "fallen_back",
    }))
    .unwrap();
    let rebuilt = masked_after_two(vec![refusal]);
    assert!(rebuilt.search_refused.contains("direct"));
}

#[test]
fn a_masked_accepted_request_makes_the_kept_user_tail_final() {
    let user = LogEvent::MessageCommitted {
        turn: 2,
        message: Message {
            role: Role::User,
            content: vec![ContentBlock::Text {
                text: "and this".into(),
            }],
        }
        .into(),
        model: None,
        stop_reason: None,
    };
    let held = rebuild(vec![started(1, "one"), answered(1, "one"), user.clone()]).unwrap();
    assert_eq!(held.provisional, Some(2));
    let rebuilt = rebuild(vec![
        started(1, "one"),
        answered(1, "one"),
        user,
        step(2, 3, "2026-10-25T12:00:00Z"),
        rewound(2, 2, 3, &[2]),
    ])
    .unwrap();
    assert_eq!(rebuilt.provisional, None);
}

#[test]
fn a_tampered_overlapping_or_forward_rewind_names_its_record() {
    let mut tampered = turns(&[(1, "one"), (2, "two")]);
    tampered.push(rewound(1, 2, 3, &[2]));
    assert_eq!(rebuild(tampered).unwrap_err().rewind_at, Some(4));

    let mut overlapping = turns(&[(1, "one"), (2, "two"), (3, "three")]);
    overlapping.push(rewound(1, 2, 2, &[2, 3]));
    overlapping.extend(turns(&[(4, "two b")]));
    overlapping.push(rewound(3, 3, 4, &[4]));
    assert_eq!(rebuild(overlapping).unwrap_err().rewind_at, Some(9));

    let mut forward = turns(&[(1, "one")]);
    forward.push(rewound(5, 1, 0, &[1]));
    assert_eq!(rebuild(forward).unwrap_err().rewind_at, Some(2));
}

#[test]
fn the_event_only_rebuild_refuses_a_rewind_for_want_of_record_numbers() {
    let mut events = turns(&[(1, "one"), (2, "two")]);
    events.push(rewound(1, 2, 2, &[2]));
    let error = rebuild_history(&events, no_images).unwrap_err();
    assert_eq!(error.reason, wisp_protocol::log::REWIND_NEEDS_RECORDS);
    assert_eq!(error.rewind_at, None);
}

fn three_on_opus() -> Vec<LogEvent> {
    vec![
        on(1, "one"),
        answered(1, "one"),
        step(1, 1, "2026-10-25T12:00:00Z"),
        on(2, "two"),
        answered(2, "two"),
        step(2, 3, "2026-10-25T12:10:00Z"),
        on(3, "three"),
    ]
}

#[test]
fn a_request_after_rewinding_past_a_compaction_refreshes_the_points_of_the_kept_epoch() {
    let later = "2026-10-25T12:40:00Z";
    let mut events = three_on_opus();
    events.extend([
        compacted(1),
        answered(3, "three"),
        rewound(5, 3, 4, &[3]),
        on(4, "four"),
        answered(4, "four"),
        step(4, 5, later),
    ]);
    let rebuilt = rebuild(events).unwrap();
    let two = rebuilt.points.iter().find(|p| p.turn == 2).unwrap();
    assert_eq!(two.epoch, 0);
    assert_eq!(two.refreshed.last().map(|(at, _)| at.as_str()), Some(later));
}

fn restored_after(after: Vec<LogEvent>) -> Option<Option<String>> {
    let mut events = three_on_opus();
    events.extend([answered(3, "three"), rewound(5, 3, 4, &[3])]);
    events.extend(after);
    let rebuilt = rebuild(events).unwrap();
    rebuilt
        .restored
        .map(|baseline| baseline.map(|b| b.started_at))
}

#[test]
fn the_rewound_points_baseline_waits_for_the_next_request_or_compaction() {
    let kept = Some(Some("2026-10-25T12:10:00Z".to_owned()));
    assert_eq!(restored_after(Vec::new()), kept);
    assert_eq!(
        restored_after(vec![on(4, "four"), answered(4, "four")]),
        kept
    );
    assert_eq!(
        restored_after(vec![
            on(4, "four"),
            answered(4, "four"),
            step(4, 5, "2026-10-25T12:40:00Z")
        ]),
        None
    );
    assert_eq!(restored_after(vec![compacted(1)]), None);
    assert_eq!(rebuild(three_on_opus()).unwrap().restored, None);
}

#[test]
fn a_rewound_point_after_a_byte_change_restores_no_baseline() {
    let events = vec![
        on(1, "one"),
        answered(1, "one"),
        step(1, 1, "2026-10-25T12:00:00Z"),
        LogEvent::RebuildMarked {
            turn: 1,
            reason: "tools_change".into(),
        },
        on(2, "two"),
        answered(2, "two"),
        rewound(3, 2, 2, &[2]),
    ];
    assert_eq!(rebuild(events).unwrap().restored, Some(None));
}
