#![allow(
    clippy::unwrap_used,
    clippy::panic,
    reason = "fixture helpers fail the test on any read, write or parse error"
)]

mod support;

use serde_json::json;
use support::{golden, record, sha};
use wisp_protocol::log::{
    BlobSource, ImageRef, LOG_SCHEMA_VERSION, LogEvent, LogRecord, LoggedInput, NO_HASH,
    PROVISIONAL_SINCE, Rebuilt, TurnLead, WithdrawnBlock, no_images, rebuild_history,
    rebuild_history_with, rebuild_log_with,
};
use wisp_protocol::{
    AgentEvent, AttachmentKind, CompactionEngine, CompactionTrigger, ContentBlock, Effort,
    ImageType, InputId, Message, Role, TurnOutcome, TurnStats, WithdrawnAttachment,
};

const NOTE: &str = "[image #1 couldn't be processed: Could not process image]";
const SECOND: &str = "[image #2 couldn't be processed: Could not process image]";

fn image_ref(n: u8) -> ImageRef {
    ImageRef {
        sha256: sha(n),
        media_type: ImageType::Png,
        bytes: 4,
        width: 1,
        height: 1,
    }
}

fn data(n: u8) -> String {
    format!("image-{n}")
}

fn step_ended(turn: u64, step: u32, accepted: bool) -> LogEvent {
    LogEvent::StepEnded {
        list_usd: None,
        model: None,
        turn,
        step,
        usage: None,
        cost_usd: None,
        lower_bound: false,
        prompt_tokens: None,
        response_id: None,
        window: None,
        compact_at: None,
        used: None,
        first_pass: None,
        accepted,
        view_len: None,
        started_at: None,
        ttl: None,
        prefix: None,
        queued_ms: None,
        thread: None,
        thread_error: None,
        betas: Vec::new(),
    }
}

fn withdrawn(turn: u64, items: &[(usize, u8, &str)]) -> LogEvent {
    LogEvent::AttachmentsWithdrawn {
        turn,
        step: 1,
        items: items
            .iter()
            .map(|(ordinal, n, note)| WithdrawnBlock {
                ordinal: *ordinal,
                sha256: sha(*n),
                note: (*note).into(),
            })
            .collect(),
    }
}

fn started(turn: u64, text: &str, images: Vec<ImageRef>) -> LogEvent {
    LogEvent::TurnStarted {
        ends: Vec::new(),
        turn,
        origin: None,
        lead: TurnLead::default(),
        text: text.into(),
        model: None,
        effort: None,
        effort_marker: None,
        images,
        endpoint_name: None,
        input_id: None,
        inputs: Vec::new(),
        cache_ttl: None,
        debug_cache: None,
        refusal_fallback: None,
        binding_controls: None,
        retry_of: None,
        kept_tools_sha256: None,
        tool_message: Vec::new(),
    }
}

fn committed(turn: u64, role: Role, content: Vec<ContentBlock>) -> LogEvent {
    LogEvent::MessageCommitted {
        turn,
        message: Message { role, content }.into(),
        model: None,
        stop_reason: None,
    }
}

fn text(t: &str) -> ContentBlock {
    ContentBlock::Text { text: t.into() }
}

fn image(n: u8) -> ContentBlock {
    ContentBlock::image(ImageType::Png, data(n))
}

fn tool_call() -> Vec<ContentBlock> {
    vec![ContentBlock::ToolUse {
        id: "toolu_1".into(),
        name: "snap".into(),
        input: json!({}),
    }]
}

fn tool_result(images: &[u8]) -> Vec<ContentBlock> {
    let mut content = vec![text("a screenshot")];
    content.extend(images.iter().map(|n| image(*n)));
    vec![ContentBlock::ToolResult {
        tool_use_id: "toolu_1".into(),
        content,
        is_error: false,
    }]
}

fn load(image: &ImageRef) -> Result<String, String> {
    let n = u8::from_str_radix(&image.sha256[..2], 16).map_err(|e| e.to_string())?;
    Ok(data(n))
}

struct Hashing;

impl BlobSource for Hashing {
    fn image(&mut self, image: &ImageRef) -> Result<String, String> {
        load(image)
    }

    fn sha256(&mut self, base64: &str) -> Result<String, String> {
        let n: u8 = base64
            .strip_prefix("image-")
            .and_then(|n| n.parse().ok())
            .ok_or_else(|| format!("not a test image: {base64}"))?;
        Ok(sha(n))
    }
}

fn tail_events(accepted: bool) -> Vec<LogEvent> {
    vec![
        started(1, "take a screenshot", Vec::new()),
        committed(1, Role::Assistant, tool_call()),
        step_ended(1, 1, true),
        committed(1, Role::User, tool_result(&[7])),
        step_ended(1, 2, accepted),
    ]
}

fn records(events: Vec<LogEvent>, version: u16) -> Vec<LogRecord> {
    events
        .into_iter()
        .zip(0..)
        .map(|(event, seq)| LogRecord {
            schema_version: version,
            ..record(seq, event)
        })
        .collect()
}

fn current(events: Vec<LogEvent>) -> Rebuilt {
    rebuild_log_with(records(events, PROVISIONAL_SINCE).iter(), Hashing).unwrap()
}

#[test]
fn the_withdrawal_types_match_their_fixtures() {
    assert_eq!(PROVISIONAL_SINCE, 15);
    const { assert!(LOG_SCHEMA_VERSION >= PROVISIONAL_SINCE) };
    golden(
        "event_attachments_withdrawn",
        &AgentEvent::AttachmentsWithdrawn {
            step: 2,
            items: vec![
                WithdrawnAttachment {
                    call_id: None,
                    ordinal: 0,
                    kind: AttachmentKind::Image,
                    label: "#2".into(),
                    reason: "Could not process image".into(),
                    note: SECOND.into(),
                },
                WithdrawnAttachment {
                    call_id: Some("toolu_01".into()),
                    ordinal: 1,
                    kind: AttachmentKind::Image,
                    label: "#1".into(),
                    reason: "Could not process image".into(),
                    note: NOTE.into(),
                },
            ],
        },
    );
    golden(
        "log_attachments_withdrawn",
        &record(9, withdrawn(4, &[(0, 1, NOTE), (1, 2, SECOND)])),
    );
    golden(
        "log_step_ended_not_accepted",
        &record(10, step_ended(4, 2, false)),
    );
}

#[test]
fn an_accepted_step_writes_no_flag_and_a_missing_flag_reads_as_accepted() {
    let line = serde_json::to_value(record(3, step_ended(1, 1, true))).unwrap();
    assert!(line.get("accepted").is_none(), "{line}");
    let refused = serde_json::to_value(record(3, step_ended(1, 1, false))).unwrap();
    assert_eq!(refused["accepted"], json!(false));
    let old: LogRecord = serde_json::from_value(json!({
        "schema_version": 14, "seq": 3, "at": "2026-10-20T12:00:00Z",
        "type": "step_ended", "turn": 1, "step": 1, "usage": null, "cost_usd": null,
        "lower_bound": false, "prompt_tokens": null, "response_id": null
    }))
    .unwrap();
    assert!(matches!(
        old.event,
        LogEvent::StepEnded { accepted: true, .. }
    ));
}

#[test]
fn a_withdrawn_pasted_image_rebuilds_as_its_note_without_reading_the_store() {
    let events = [
        started(1, "what are these?", vec![image_ref(1), image_ref(2)]),
        withdrawn(1, &[(1, 2, SECOND)]),
        committed(1, Role::Assistant, vec![text("one red square")]),
        step_ended(1, 1, true),
    ];
    let rebuilt = rebuild_history(events.iter(), |r| {
        if r.sha256 == sha(2) {
            panic!("the withdrawn image was read from the store");
        }
        load(r)
    })
    .unwrap();
    assert_eq!(
        rebuilt.messages[0].content,
        vec![image(1), text(SECOND), text("what are these?")]
    );
    assert_eq!(rebuilt.provisional, None);
    let all = [
        started(1, "what are these?", vec![image_ref(1), image_ref(2)]),
        withdrawn(1, &[(0, 1, NOTE), (1, 2, SECOND)]),
        committed(1, Role::Assistant, vec![text("ok")]),
    ];
    let rebuilt = rebuild_history(all.iter(), no_images).unwrap();
    assert_eq!(
        rebuilt.messages[0].content,
        vec![text(NOTE), text(SECOND), text("what are these?")]
    );
}

#[test]
fn a_withdrawn_steer_image_applies_to_the_inputs_committed_after_it() {
    let events = [
        started(1, "take a screenshot", Vec::new()),
        committed(1, Role::Assistant, tool_call()),
        step_ended(1, 1, true),
        committed(1, Role::User, tool_result(&[])),
        withdrawn(1, &[(0, 3, NOTE)]),
        LogEvent::InputCommitted {
            turn: 1,
            step: 2,
            inputs: vec![LoggedInput {
                id: InputId("tui-2".into()),
                text: "this one too".into(),
                images: vec![image_ref(3)],
                note: None,
                checklist_spans: Vec::new(),
            }],
        },
        committed(1, Role::Assistant, vec![text("ok")]),
        step_ended(1, 2, true),
    ];
    let rebuilt = rebuild_history(events.iter(), no_images).unwrap();
    let tail = &rebuilt.messages[2].content;
    assert_eq!(&tail[1..], &[text(NOTE), text("this one too")]);
}

fn late_marker_turn(second: &[(usize, u8, &str)]) -> Vec<LogEvent> {
    vec![
        started(1, "look", vec![image_ref(1)]),
        withdrawn(1, &[(0, 1, NOTE)]),
        withdrawn(1, second),
        committed(
            1,
            Role::System,
            vec![ContentBlock::Effort {
                level: Effort::Low,
                rendered: true,
            }],
        ),
        LogEvent::InputCommitted {
            turn: 1,
            step: 2,
            inputs: vec![LoggedInput {
                id: InputId("tui-2".into()),
                text: "and this".into(),
                images: vec![image_ref(2)],
                note: None,
                checklist_spans: Vec::new(),
            }],
        },
        committed(1, Role::Assistant, vec![text("ok")]),
    ]
}

#[test]
fn withdrawals_past_a_late_marker_count_every_earlier_image_and_a_repeat_fails() {
    let rebuilt = rebuild_history(late_marker_turn(&[(1, 2, SECOND)]).iter(), no_images).unwrap();
    assert_eq!(rebuilt.messages[0].content, [text(NOTE), text("look")]);
    assert!(rebuilt.messages[1].effort_marker().is_some());
    assert_eq!(
        rebuilt.messages[2].content,
        [text(SECOND), text("and this")]
    );
    let repeated = late_marker_turn(&[(0, 2, SECOND)]);
    let error = rebuild_history(repeated.iter(), no_images).unwrap_err();
    assert_eq!(error.turn, 1);
    assert_eq!(error.sha256, sha(2));
    assert!(error.reason.contains("more than once"), "{error}");
}

#[test]
fn a_withdrawal_that_doesnt_match_fails_the_rebuild_naming_the_turn() {
    let wrong_hash = [
        started(4, "look", vec![image_ref(1)]),
        withdrawn(4, &[(0, 9, NOTE)]),
        committed(4, Role::Assistant, vec![text("ok")]),
    ];
    let error = rebuild_history(wrong_hash.iter(), load).unwrap_err();
    assert_eq!(error.turn, 4);
    assert_eq!(error.sha256, sha(9));
    assert!(error.to_string().starts_with("turn 4:"), "{error}");
    let no_such_block = [
        started(5, "look", vec![image_ref(1)]),
        withdrawn(5, &[(1, 1, NOTE)]),
        committed(5, Role::Assistant, vec![text("ok")]),
    ];
    let error = rebuild_history(no_such_block.iter(), load).unwrap_err();
    assert_eq!(error.turn, 5);
    let mut tail = tail_events(false);
    tail.push(started(2, "and now?", Vec::new()));
    tail.push(withdrawn(2, &[(0, 8, NOTE)]));
    let error = rebuild_history_with(tail.iter(), Hashing).unwrap_err();
    assert_eq!(error.turn, 2);
    assert!(error.reason.contains(&sha(7)), "{error}");
}

#[test]
fn a_tool_result_tail_no_accepted_request_carried_is_provisional() {
    let rebuilt = current(tail_events(false));
    assert_eq!(rebuilt.provisional, Some(2));
    let mut two_failures = tail_events(false);
    two_failures.extend([
        LogEvent::TurnEnded {
            turn: 1,
            outcome: TurnOutcome::Cancelled {
                usage: None,
                cost: None,
                committed_steps: 1,
            },
            stats: TurnStats::default(),
            partial_output: None,
        },
        started(2, "again", vec![image_ref(1)]),
        step_ended(2, 1, false),
    ]);
    let rebuilt = current(two_failures);
    assert_eq!(rebuilt.provisional, Some(2));
    assert_eq!(
        rebuilt.messages.len(),
        3,
        "the failed turn's input isn't kept"
    );
    let accepted = current(tail_events(true));
    assert_eq!(accepted.provisional, None);
    let mut answered = tail_events(true);
    answered.push(committed(1, Role::Assistant, vec![text("done")]));
    let answered = current(answered);
    assert_eq!(answered.provisional, None);
}

#[test]
fn events_without_their_versions_are_never_provisional() {
    let events = tail_events(false);
    assert_eq!(current(events.clone()).provisional, Some(2));
    let rebuilt = rebuild_history(events.iter(), no_images).unwrap();
    assert_eq!(rebuilt.provisional, None);
    let rebuilt = rebuild_history_with(events.iter(), Hashing).unwrap();
    assert_eq!(rebuilt.provisional, None);
}

#[test]
fn a_tail_written_before_the_bump_is_never_provisional() {
    let mut crashed = tail_events(false);
    crashed.pop();
    let old = records(crashed.clone(), PROVISIONAL_SINCE - 1);
    let rebuilt = rebuild_log_with(old.iter(), Hashing).unwrap();
    assert_eq!(rebuilt.provisional, None);
    let new = records(crashed, PROVISIONAL_SINCE);
    let rebuilt = rebuild_log_with(new.iter(), Hashing).unwrap();
    assert_eq!(rebuilt.provisional, Some(2));
}

#[test]
fn a_withdrawn_tool_result_image_rebuilds_as_its_note_after_the_tools_text() {
    let mut events = tail_events(false);
    events.extend([
        started(2, "what do you see?", Vec::new()),
        withdrawn(2, &[(0, 7, NOTE)]),
        committed(2, Role::Assistant, vec![text("a note")]),
        step_ended(2, 1, true),
    ]);
    let rebuilt = rebuild_history_with(events.iter(), Hashing).unwrap();
    let ContentBlock::ToolResult { content, .. } = &rebuilt.messages[2].content[0] else {
        panic!("{:?}", rebuilt.messages[2])
    };
    assert_eq!(content, &vec![text("a screenshot"), text(NOTE)]);
    assert_eq!(rebuilt.provisional, None);
    let error = rebuild_history(events.iter(), no_images).unwrap_err();
    assert_eq!(error.reason, NO_HASH);
}

fn compacted(from: usize) -> LogEvent {
    LogEvent::ContextCompacted {
        list_usd: None,
        model: None,
        lower_bound: false,
        plan_usd: None,
        sections: Vec::new(),
        turn: None,
        epoch: 1,
        trigger: CompactionTrigger::Manual,
        engine: CompactionEngine::Wisp,
        focus: None,
        before: 0,
        after: 0,
        partial: false,
        covers: None,
        from,
        head: Message {
            role: Role::User,
            content: vec![text("<compacted_context/>")],
        },
        summary: Some("so far".into()),
        block: None,
        usage: None,
        cost_usd: None,
        duration_ms: 0,
        switch: None,
        placement: wisp_protocol::Placement::Head,
        cut: None,
        additions: Vec::new(),
        instructions_by: Vec::new(),
        strategy: None,
        instruction: None,
        requested_by: None,
    }
}

#[test]
fn a_head_closes_the_open_tail_before_it() {
    let mut events = tail_events(false);
    events.push(compacted(3));
    assert_eq!(current(events.clone()).provisional, None);
    events.extend([
        started(2, "and now?", vec![image_ref(5)]),
        withdrawn(2, &[(0, 5, NOTE)]),
        committed(2, Role::Assistant, vec![text("ok")]),
    ]);
    let rebuilt = current(events);
    assert_eq!(rebuilt.messages[2].content, tool_result(&[7]));
    assert_eq!(rebuilt.messages[3].content, [text(NOTE), text("and now?")]);
}
