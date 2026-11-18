use wisp_protocol::log::{LogEvent, LogRecord, TurnLead};
use wisp_protocol::{AgentEvent, Role, TurnOutcome};

use super::*;
use crate::agent::testing::{completed, noted, stream_error, turn, withdrawn, without_stamps};
use crate::agent::{Agent, default_session, retry};
use crate::provider::Refused;
use crate::session::{MemoryStore, SessionLog, SessionStore, parse_jsonl, rebuild_checked};
use crate::test_guard::{
    self, RED_PIXEL as RED, ScratchDir, error_json, head_and_messages, http_400,
    http_404_image_input, http_529, log_context, pick, records, retries,
};

const CANT_PROCESS: &str = "Could not process image";
const NOTE: &str = "[image #1 couldn't be processed: Could not process image]";
const V14_TAIL: &str = include_str!("../../../../tests/fixtures/logs/v14-tool-result-tail.jsonl");

fn reply() -> String {
    http::sse(THINKING_AND_TEXT)
}

fn bodies(seen: &Arc<Mutex<Vec<String>>>) -> Vec<String> {
    seen.lock()
        .unwrap()
        .iter()
        .map(|r| http::body(r).to_owned())
        .collect()
}

fn image_object(body: &str) -> &str {
    let start = body
        .find(r#"{"type":"image","source":"#)
        .unwrap_or_else(|| panic!("no image in {body}"));
    let end = body[start..].find(r#""}}"#).unwrap() + start + 3;
    &body[start..end]
}

fn without_image(body: &str, note: &str) -> String {
    let text = format!(r#"{{"type":"text","text":{}}}"#, json!(note));
    body.replacen(image_object(body), &text, 1)
}

fn agent(base: &str, store: &MemoryStore) -> Agent {
    let provider = AnthropicProvider::new(&api_key("k"), base).unwrap();
    Agent::new(Arc::new(provider), default_session())
        .with_clock(crate::clock::TickingClock::new())
        .with_retry_policy(retry::quick())
        .with_log(
            SessionLog::new(Box::new(store.clone()), "s1"),
            log_context(),
        )
}

fn error_message(outcome: &TurnOutcome) -> String {
    match outcome {
        TurnOutcome::Error { message, .. } => message.clone(),
        other => panic!("expected an error, got {other:?}"),
    }
}

fn kinds(records: &[LogRecord]) -> Vec<&'static str> {
    records
        .iter()
        .map(|r| match &r.event {
            LogEvent::AttachmentsWithdrawn { .. } => "attachments_withdrawn",
            LogEvent::MessageCommitted { .. } => "message_committed",
            LogEvent::InputCommitted { .. } => "input_committed",
            LogEvent::StepEnded { .. } => "step_ended",
            _ => "other",
        })
        .filter(|k| *k != "other")
        .collect()
}

fn step_ends(records: &[LogRecord]) -> Vec<(u64, bool)> {
    records
        .iter()
        .filter_map(|r| match &r.event {
            LogEvent::StepEnded { turn, accepted, .. } => Some((*turn, *accepted)),
            _ => None,
        })
        .collect()
}

fn extends(earlier: &str, later: &str) -> bool {
    let (head_a, msgs_a) = head_and_messages(earlier);
    let (head_b, msgs_b) = head_and_messages(later);
    head_a == head_b && msgs_b.starts_with(msgs_a.strip_suffix(']').unwrap())
}

#[tokio::test]
async fn a_refused_pasted_image_is_withdrawn_and_the_session_carries_on() {
    let (base, seen) = mock_server(vec![http_400(CANT_PROCESS), reply(), reply()]).await;
    let store = MemoryStore::default();
    let mut agent = agent(&base, &store);
    let (outcome, events) = turn(&mut agent, "what is this?", &[RED]).await;
    completed(&outcome);
    assert_eq!(retries(&events), 0, "a re-send is not a retry");
    let items = withdrawn(&events);
    assert_eq!(items.len(), 1);
    assert_eq!(items[0].call_id, None);
    assert_eq!(items[0].ordinal, 0);
    assert_eq!(items[0].label, "#1");
    assert_eq!(items[0].reason, CANT_PROCESS);
    assert_eq!(items[0].note, NOTE);

    let (outcome, _) = turn(&mut agent, "and now?", &[]).await;
    completed(&outcome);
    let sent = bodies(&seen);
    assert_eq!(sent.len(), 3);
    assert_eq!(
        sent[1],
        without_image(&sent[0], NOTE),
        "the re-send is the refused request with the note in the image's place"
    );
    assert!(!sent[1].contains(r#""type":"image""#));
    assert!(sent[2].contains(&json!(NOTE).to_string()));
    assert!(!sent[2].contains(r#""type":"image""#));

    let records = records(&store);
    let order = kinds(&records);
    let withdrawal = order
        .iter()
        .position(|k| *k == "attachments_withdrawn")
        .unwrap();
    let first_commit = order
        .iter()
        .position(|k| *k == "message_committed")
        .unwrap();
    assert!(withdrawal < first_commit, "{order:?}");
    assert_eq!(
        order
            .iter()
            .filter(|k| **k == "attachments_withdrawn")
            .count(),
        1
    );
    assert_eq!(step_ends(&records), vec![(1, true), (2, true)]);
    let rebuilt = rebuild_checked(&store, &records, None).unwrap().unwrap();
    assert_eq!(rebuilt.provisional, None);
    assert_eq!(
        serde_json::to_string(&rebuilt.messages).unwrap(),
        serde_json::to_string(&*agent.history()).unwrap()
    );
    let user = &rebuilt.messages[0];
    assert!(
        user.content
            .iter()
            .any(|b| matches!(b, ContentBlock::Text { text } if text == NOTE))
    );
}

#[tokio::test]
async fn steps_after_a_withdrawal_share_one_prefix_and_leave_the_refused_request_out() {
    let (base, seen) = mock_server(vec![http_400(CANT_PROCESS), reply(), reply(), reply()]).await;
    let store = MemoryStore::default();
    let mut agent = agent(&base, &store);
    turn(&mut agent, "what is this?", &[RED]).await;
    turn(&mut agent, "and now?", &[]).await;
    turn(&mut agent, "one more", &[]).await;
    let sent = bodies(&seen);
    assert_eq!(sent.len(), 4);
    let accepted = &sent[1..];
    for pair in accepted.windows(2) {
        assert!(extends(&pair[0], &pair[1]), "\n{}\n{}", pair[0], pair[1]);
    }
    assert!(
        !extends(&sent[0], &sent[2]),
        "the refused request set no prefix"
    );
    let (refused_head, _) = head_and_messages(&sent[0]);
    let (resend_head, _) = head_and_messages(&sent[1]);
    assert_eq!(
        refused_head, resend_head,
        "the re-send reads the same cached prefix"
    );
}

#[tokio::test]
async fn a_refused_resend_restores_the_image_and_ends_the_turn_with_its_error() {
    let limit = "You have reached your specified API usage limits.";
    let (base, seen) = mock_server(vec![http_400(CANT_PROCESS), http_400(limit)]).await;
    let store = MemoryStore::default();
    let mut agent = agent(&base, &store);
    let (outcome, events) = turn(&mut agent, "what is this?", &[RED]).await;
    assert_eq!(error_message(&outcome), limit);
    assert!(withdrawn(&events).is_empty());
    let sent = bodies(&seen);
    assert_eq!(sent.len(), 2);
    assert_eq!(sent[1], without_image(&sent[0], NOTE));
    let records = records(&store);
    assert!(!kinds(&records).contains(&"attachments_withdrawn"));
    assert_eq!(step_ends(&records), vec![(1, false)]);
    assert!(agent.history().is_empty());
}

#[tokio::test]
async fn a_529_on_the_resend_goes_to_resilience_whose_retry_still_carries_the_image() {
    let (base, seen) = mock_server(vec![
        http_400(CANT_PROCESS),
        http_529(),
        http_400(CANT_PROCESS),
        reply(),
    ])
    .await;
    let store = MemoryStore::default();
    let mut agent = agent(&base, &store);
    let (outcome, events) = turn(&mut agent, "what is this?", &[RED]).await;
    completed(&outcome);
    assert_eq!(retries(&events), 1);
    assert_eq!(withdrawn(&events).len(), 1);
    let sent = bodies(&seen);
    assert_eq!(sent.len(), 4);
    assert_eq!(sent[2], sent[0], "the retry carries the image again");
    assert_eq!(sent[3], sent[1], "a later refusal withdraws it again");
    assert_eq!(sent[1], without_image(&sent[0], NOTE));
    let records = records(&store);
    assert_eq!(step_ends(&records), vec![(1, true)]);
}

#[tokio::test]
async fn a_413_and_a_first_event_error_chunk_count_as_refusals() {
    let too_large = http::json(
        "413 Payload Too Large",
        &error_json(
            "request_too_large",
            "Request exceeds the maximum allowed number of bytes.",
        ),
    );
    let manifest_chunk = http::sse(&format!(
        "data: {}\n\n",
        json!({"error": {"message": CANT_PROCESS, "type": "invalid_request_error", "code": null}})
    ));
    let anthropic_chunk = http::sse(&http::event(
        "error",
        error_json("invalid_request_error", CANT_PROCESS),
    ));
    let openai_overflow = http::json(
        "400 Bad Request",
        &json!({"error": {"message": "This model's maximum context length is exceeded", "type": "invalid_request_error", "code": "context_length_exceeded"}}).to_string(),
    );
    for (name, first, reason) in [
        (
            "a 413",
            too_large,
            "Request exceeds the maximum allowed number of bytes.",
        ),
        ("Manifest's first-event chunk", manifest_chunk, CANT_PROCESS),
        (
            "an Anthropic first-event chunk",
            anthropic_chunk,
            CANT_PROCESS,
        ),
        (
            "an OpenAI-style context overflow",
            openai_overflow,
            "This model's maximum context length is exceeded",
        ),
    ] {
        let (base, seen) = mock_server(vec![first, reply()]).await;
        let store = MemoryStore::default();
        let mut agent = agent(&base, &store);
        let (outcome, events) = turn(&mut agent, "what is this?", &[RED]).await;
        completed(&outcome);
        let items = withdrawn(&events);
        assert_eq!(items.len(), 1, "{name}");
        assert_eq!(items[0].reason, reason, "{name}");
        let sent = bodies(&seen);
        assert_eq!(sent.len(), 2, "{name}");
        assert_eq!(sent[1], without_image(&sent[0], &items[0].note), "{name}");
    }
}

#[tokio::test]
async fn an_overflow_with_provisional_blocks_is_withdrawn_before_the_overflow_path() {
    let overflow = "prompt is too long: 1000123 tokens > 1000000 maximum";
    let (base, seen) = mock_server(vec![http_400(overflow), reply()]).await;
    let store = MemoryStore::default();
    let mut agent = agent(&base, &store);
    let (outcome, events) = turn(&mut agent, "what is this?", &[RED]).await;
    completed(&outcome);
    assert_eq!(withdrawn(&events).len(), 1);
    assert_eq!(bodies(&seen).len(), 2);
}

#[tokio::test]
async fn an_overflow_that_outlives_the_withdrawal_takes_the_overflow_path() {
    let overflow = "prompt is too long: 1000123 tokens > 1000000 maximum";
    let still = "prompt is too long: 1000050 tokens > 1000000 maximum";
    let (base, seen) = mock_server(vec![http_400(overflow), http_400(still)]).await;
    let store = MemoryStore::default();
    let mut agent = agent(&base, &store);
    let (outcome, events) = turn(&mut agent, "what is this?", &[RED]).await;
    match &outcome {
        TurnOutcome::Error { kind, message, .. } => {
            assert_eq!(*kind, ErrorKind::ContextOverflow);
            assert_eq!(
                message,
                "This message alone is too large for the model's window."
            );
        }
        other => panic!("{other:?}"),
    }
    assert!(withdrawn(&events).is_empty());
    assert_eq!(bodies(&seen).len(), 2);
}

#[tokio::test]
async fn a_request_with_no_provisional_blocks_never_withdraws() {
    let (base, seen) = mock_server(vec![reply(), http_400(CANT_PROCESS)]).await;
    let store = MemoryStore::default();
    let mut agent = agent(&base, &store);
    let (outcome, _) = turn(&mut agent, "what is this?", &[RED]).await;
    completed(&outcome);
    let (outcome, events) = turn(&mut agent, "just text", &[]).await;
    assert_eq!(error_message(&outcome), CANT_PROCESS);
    assert!(withdrawn(&events).is_empty());
    let sent = bodies(&seen);
    assert_eq!(sent.len(), 2, "no re-send");
    assert!(
        sent[1].contains(r#""type":"image""#),
        "an accepted image stays"
    );
    let records = records(&store);
    assert_eq!(step_ends(&records), vec![(1, true), (2, false)]);
}

fn snap_call() -> Message {
    Message {
        role: Role::Assistant,
        content: vec![ContentBlock::ToolUse {
            id: "toolu_1".into(),
            name: "snap".into(),
            input: json!({}),
        }],
    }
}

fn snap_result() -> Message {
    Message {
        role: Role::User,
        content: vec![ContentBlock::ToolResult {
            tool_use_id: "toolu_1".into(),
            content: vec![
                ContentBlock::Text {
                    text: "a screenshot".into(),
                },
                ContentBlock::image(wisp_protocol::ImageType::Png, RED),
            ],
            is_error: false,
        }],
    }
}

fn started(turn: u64, text: &str) -> LogEvent {
    LogEvent::TurnStarted {
        ends: Vec::new(),
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
        tool_message: Vec::new(),
    }
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

fn committed(turn: u64, message: Message) -> LogEvent {
    LogEvent::MessageCommitted {
        turn,
        message: message.into(),
        model: None,
        stop_reason: None,
    }
}

fn turn_failed(turn: u64) -> LogEvent {
    LogEvent::TurnEnded {
        turn,
        outcome: TurnOutcome::Error {
            kind: ErrorKind::InvalidRequest,
            message: CANT_PROCESS.into(),
            usage: None,
            cost: None,
            committed_steps: 1,
            detail: None,
            attempts: 0,
            gave_up: None,
            offer: None,
        },
        stats: wisp_protocol::TurnStats::default(),
        partial_output: None,
    }
}

fn tail_log(store: &MemoryStore, last_step_accepted: bool, failed_turns: u64) -> Vec<LogRecord> {
    let mut log = SessionLog::new(Box::new(store.clone()), "old");
    log.record(started(1, "take a screenshot"));
    log.record(committed(1, snap_call()));
    log.record(step_ended(1, 1, true));
    log.record(committed(1, snap_result()));
    log.record(step_ended(1, 2, last_step_accepted));
    log.record(turn_failed(1));
    for turn in 2..=failed_turns {
        log.record(started(turn, "try again"));
        log.record(step_ended(turn, 1, false));
        log.record(turn_failed(turn));
    }
    store.read("old").unwrap()
}

async fn resume_and_send(
    records: &[LogRecord],
    store: &MemoryStore,
    responses: Vec<String>,
) -> (TurnOutcome, Vec<AgentEvent>, Agent, Vec<String>) {
    let rebuilt = rebuild_checked(store, records, None).unwrap().unwrap();
    let (base, seen) = mock_server(responses).await;
    let mut agent = agent(&base, store).resumed(rebuilt, records);
    let (outcome, events) = turn(&mut agent, "what do you see?", &[]).await;
    let sent = bodies(&seen);
    (outcome, events, agent, sent)
}

#[tokio::test]
async fn an_attachment_refused_after_resume_is_withdrawn() {
    for failed_turns in [1, 2] {
        let store = MemoryStore::default();
        let records = tail_log(&store, false, failed_turns);
        let rebuilt = rebuild_checked(&store, &records, None).unwrap().unwrap();
        assert_eq!(rebuilt.provisional, Some(2), "{failed_turns} failed turns");
        let (outcome, events, agent, sent) =
            resume_and_send(&records, &store, vec![http_400(CANT_PROCESS), reply()]).await;
        completed(&outcome);
        let items = withdrawn(&events);
        assert_eq!(items.len(), 1);
        assert_eq!(items[0].call_id.as_deref(), Some("toolu_1"));
        assert_eq!(sent.len(), 2);
        assert_eq!(sent[1], without_image(&sent[0], NOTE));
        let result = &agent.history()[2];
        let ContentBlock::ToolResult { content, .. } = &result.content[0] else {
            panic!("{result:?}")
        };
        assert_eq!(
            content[1],
            ContentBlock::Text { text: NOTE.into() },
            "the note takes the attachment's place after the tool's text"
        );
        let all = test_guard::records(&store);
        let resumed: Vec<&LogEvent> = records.iter().chain(all.iter()).map(|r| &r.event).collect();
        let rebuilt = crate::session::rebuild(&store, resumed.iter().copied()).unwrap();
        assert_eq!(rebuilt.messages[2], agent.history()[2]);
    }
}

#[tokio::test]
async fn a_refused_resend_after_resume_restores_the_attachment_in_history() {
    let store = MemoryStore::default();
    let records = tail_log(&store, false, 1);
    let (outcome, events, agent, sent) = resume_and_send(
        &records,
        &store,
        vec![http_400(CANT_PROCESS), http_400("still refused")],
    )
    .await;
    assert_eq!(error_message(&outcome), "still refused");
    assert!(withdrawn(&events).is_empty());
    assert_eq!(sent.len(), 2);
    assert_eq!(agent.history()[2], snap_result());
}

#[tokio::test]
async fn a_log_whose_step_was_accepted_resumes_with_nothing_provisional() {
    let store = MemoryStore::default();
    let records = tail_log(&store, true, 1);
    let rebuilt = rebuild_checked(&store, &records, None).unwrap().unwrap();
    assert_eq!(rebuilt.provisional, None);
    let (outcome, events, agent, sent) =
        resume_and_send(&records, &store, vec![http_400(CANT_PROCESS)]).await;
    assert_eq!(error_message(&outcome), CANT_PROCESS);
    assert!(withdrawn(&events).is_empty());
    assert_eq!(sent.len(), 1);
    assert_eq!(agent.history()[2], snap_result());
}

#[tokio::test]
async fn a_log_from_before_the_bump_resumes_with_nothing_provisional() {
    let store = MemoryStore::default();
    let records = parse_jsonl(V14_TAIL).unwrap();
    assert!(records.iter().all(|r| r.schema_version == 14));
    assert!(records.iter().any(|r| matches!(
        &r.event,
        LogEvent::MessageCommitted { message, .. } if message.role == Role::User
    )));
    let rebuilt = rebuild_checked(&store, &records, None).unwrap().unwrap();
    assert_eq!(rebuilt.provisional, None);
    assert_eq!(rebuilt.messages.last().unwrap().role, Role::User);
    let (outcome, events, _, sent) =
        resume_and_send(&records, &store, vec![http_400(CANT_PROCESS)]).await;
    assert_eq!(error_message(&outcome), CANT_PROCESS);
    assert!(withdrawn(&events).is_empty());
    assert_eq!(sent.len(), 1);
}

async fn refusal(response: String, provisional: bool, sends_media: bool) -> ProviderError {
    let (base, _) = mock_server(vec![response]).await;
    let provider = AnthropicProvider::new(&api_key("k"), &base).unwrap();
    let s = session(None, CacheTtl::OneHour);
    let rendering = crate::provider::Rendering {
        provisional,
        sends_media,
        ..crate::provider::Rendering::default()
    };
    let request = Request {
        rendering: &rendering,
        ..hi_request(&s)
    };
    stream_error(&provider, &request).await
}

#[tokio::test]
async fn the_adapter_marks_only_refusals_of_requests_with_provisional_blocks() {
    let start = http::event(
        "message_start",
        r#"{"type":"message_start","message":{"id":"m1","model":"claude-opus-5-5","usage":{"input_tokens":10,"output_tokens":1}}}"#,
    );
    let mid_stream = http::sse(&format!(
        "{start}{}",
        http::event("error", error_json("invalid_request_error", CANT_PROCESS))
    ));
    let binding = http_400(THINKING_BINDING_TEXT);
    let cases = [
        ("a 400", http_400(CANT_PROCESS), true, true),
        (
            "a 413",
            http::json(
                "413 Payload Too Large",
                &error_json("request_too_large", "too big"),
            ),
            true,
            false,
        ),
        (
            "a buffered gateway's 200 carrying a JSON error",
            http::json("200 OK", &error_json("invalid_request_error", CANT_PROCESS)),
            true,
            true,
        ),
        ("a 529", http_529(), false, false),
        ("an error after the first event", mid_stream, false, false),
        ("a thinking-binding 400", binding, false, false),
        (
            "a 404",
            http::json(
                "404 Not Found",
                &error_json("not_found_error", "no such model"),
            ),
            false,
            false,
        ),
        ("an image-input 404", http_404_image_input(), true, true),
        ("a 400 whose code is its status", coded_400(), true, true),
        (
            "a 400 whose code is known",
            http::json(
                "400 Bad Request",
                &json!({"type": "error", "error": {"type": "invalid_request_error", "code": "unsupported_parameter", "message": CANT_PROCESS}}).to_string(),
            ),
            true,
            true,
        ),
        (
            "a 400 of an unknown type",
            http::json("400 Bad Request", &error_json("brand_new_error", CANT_PROCESS)),
            false,
            false,
        ),
    ];
    for (name, response, attachment, content) in cases {
        let marked = refusal(response.clone(), true, true).await;
        assert_eq!(
            marked.refused == Some(Refused::Attachment),
            attachment,
            "{name}: {marked:?}"
        );
        assert_eq!(marked.content_refused, content, "{name}: {marked:?}");
        let accepted = refusal(response.clone(), false, true).await;
        assert_ne!(accepted.refused, Some(Refused::Attachment), "{name}");
        assert_eq!(accepted.content_refused, content, "{name}: {accepted:?}");
        let plain = refusal(response, false, false).await;
        assert_ne!(plain.refused, Some(Refused::Attachment), "{name}");
        assert!(!plain.content_refused, "{name}");
    }
}

fn images_in(body: &str) -> usize {
    body.matches(r#"{"type":"image","source":"#).count()
}

#[tokio::test]
async fn an_image_an_accepted_request_carried_is_never_withdrawn() {
    let (base, seen) = mock_server(vec![reply(), http_400(CANT_PROCESS), reply()]).await;
    let store = MemoryStore::default();
    let mut agent = agent(&base, &store);
    let (outcome, _) = turn(&mut agent, "what is this?", &[RED]).await;
    completed(&outcome);
    let (outcome, events) = turn(&mut agent, "and this one?", &[RED]).await;
    completed(&outcome);
    let items = withdrawn(&events);
    assert_eq!(items.len(), 1, "{items:#?}");
    assert_eq!(items[0].ordinal, 0);
    assert_eq!(items[0].label, "#1");
    let sent = bodies(&seen);
    assert_eq!(sent.len(), 3);
    assert_eq!(images_in(&sent[0]), 1);
    assert_eq!(images_in(&sent[1]), 2);
    assert_eq!(images_in(&sent[2]), 1);
    assert!(
        extends(&sent[0], &sent[2]),
        "the re-send keeps turn 1's image and prefix\n{}\n{}",
        sent[0],
        sent[2]
    );
    assert!(sent[2].contains(&json!(NOTE).to_string()));
    let records = records(&store);
    let rebuilt = rebuild_checked(&store, &records, None).unwrap().unwrap();
    assert_eq!(rebuilt.messages, &*agent.history());
}

const SNAP_PDF_CALL: &str = r#"event: message_start
data: {"type":"message_start","message":{"id":"msg_snap","model":"claude-opus-5-5","usage":{"input_tokens":12,"output_tokens":1}}}

event: content_block_start
data: {"type":"content_block_start","index":0,"content_block":{"type":"tool_use","id":"toolu_snap","name":"snap","input":{}}}

event: content_block_delta
data: {"type":"content_block_delta","index":0,"delta":{"type":"input_json_delta","partial_json":"{\"pdf\": true}"}}

event: content_block_stop
data: {"type":"content_block_stop","index":0}

event: message_delta
data: {"type":"message_delta","delta":{"stop_reason":"tool_use"},"usage":{"output_tokens":20}}

event: message_stop
data: {"type":"message_stop"}

"#;

const FILE_NOTE: &str = "[snap.pdf (application/pdf) couldn't be processed: Could not process image. \
Convert or cut it with bash into something you can read, then view or read that.]";

fn document_object(body: &str) -> &str {
    let start = body
        .find(r#"{"type":"document","source":"#)
        .unwrap_or_else(|| panic!("no document in {body}"));
    let end = body[start..].find(r#""}}"#).unwrap() + start + 3;
    &body[start..end]
}

#[tokio::test]
async fn a_refused_tool_file_is_withdrawn_and_every_accepted_step_shares_one_prefix() {
    let (base, seen) = mock_server(vec![
        http::sse(SNAP_PDF_CALL),
        http_400(CANT_PROCESS),
        reply(),
        reply(),
    ])
    .await;
    let store = MemoryStore::default();
    let scratch = ScratchDir::new("wisp-withdraw-", "snap");
    let mut agent = with_snap(agent(&base, &store), &scratch);
    let (outcome, events) = turn(&mut agent, "snap the spec", &[]).await;
    completed(&outcome);
    let items = withdrawn(&events);
    assert_eq!(items.len(), 1, "{items:#?}");
    assert_eq!(items[0].call_id.as_deref(), Some("toolu_snap"));
    assert_eq!(items[0].label, "snap.pdf");
    assert_eq!(items[0].note, FILE_NOTE);
    let (outcome, _) = turn(&mut agent, "and now?", &[]).await;
    completed(&outcome);

    let sent = bodies(&seen);
    assert_eq!(sent.len(), 4);
    let text = format!(r#"{{"type":"text","text":{}}}"#, json!(FILE_NOTE));
    assert_eq!(
        sent[2],
        sent[1].replacen(document_object(&sent[1]), &text, 1),
        "the re-send is the refused request with the note in the file's place"
    );
    assert!(extends(&sent[0], &sent[1]));
    assert!(extends(&sent[0], &sent[2]));
    assert!(extends(&sent[2], &sent[3]));
    assert!(
        !extends(&sent[1], &sent[3]),
        "the refused request set no prefix"
    );
    assert!(!sent[3].contains(r#""type":"document""#));
    let records = records(&store);
    let rebuilt = rebuild_checked(&store, &records, None).unwrap().unwrap();
    assert_eq!(rebuilt.provisional, None);
    assert_eq!(
        serde_json::to_string(&rebuilt.messages).unwrap(),
        serde_json::to_string(&*agent.history()).unwrap()
    );
}

#[cfg(unix)]
const VIEW_PNG_CALL: &str = r#"event: message_start
data: {"type":"message_start","message":{"id":"msg_view","model":"claude-opus-5-5","usage":{"input_tokens":12,"output_tokens":1}}}

event: content_block_start
data: {"type":"content_block_start","index":0,"content_block":{"type":"tool_use","id":"toolu_view","name":"view_file","input":{}}}

event: content_block_delta
data: {"type":"content_block_delta","index":0,"delta":{"type":"input_json_delta","partial_json":"{\"path\": \"shot.png\"}"}}

event: content_block_stop
data: {"type":"content_block_stop","index":0}

event: message_delta
data: {"type":"message_delta","delta":{"stop_reason":"tool_use"},"usage":{"output_tokens":20}}

event: message_stop
data: {"type":"message_stop"}

"#;

#[cfg(unix)]
const VIEW_NOTE: &str = "[shot.png (image/png) couldn't be processed: Could not process image. \
It can't be read in this session.]";

#[cfg(unix)]
#[tokio::test]
async fn a_refused_view_file_attachment_is_withdrawn_and_every_accepted_step_shares_one_prefix() {
    let (base, seen) = mock_server(vec![
        http::sse(VIEW_PNG_CALL),
        http_400(CANT_PROCESS),
        reply(),
        reply(),
    ])
    .await;
    let store = MemoryStore::default();
    let scratch = ScratchDir::new("wisp-withdraw-", "view");
    let workspace = scratch.workspace();
    let png = wisp_media::decode_base64(RED).unwrap();
    std::fs::write(workspace.root.join("shot.png"), &png).unwrap();
    let mut agent = agent(&base, &store).with_count_tokens(false).with_tools(
        crate::tool::Toolbox::new([crate::tool::testkit::view_file_without_bash()]),
        workspace,
    );
    let (outcome, events) = turn(&mut agent, "look at shot.png", &[]).await;
    completed(&outcome);
    let items = withdrawn(&events);
    assert_eq!(items.len(), 1, "{items:#?}");
    assert_eq!(items[0].call_id.as_deref(), Some("toolu_view"));
    assert_eq!(items[0].label, "shot.png");
    assert_eq!(items[0].note, VIEW_NOTE);
    let (outcome, _) = turn(&mut agent, "and now?", &[]).await;
    completed(&outcome);

    let sent = bodies(&seen);
    assert_eq!(sent.len(), 4);
    assert!(sent[0].contains(r#""name":"view_file""#));
    assert_eq!(
        sent[2],
        without_image(&sent[1], VIEW_NOTE),
        "the re-send is the refused request with the note in the image's place"
    );
    assert!(extends(&sent[0], &sent[1]));
    assert!(extends(&sent[0], &sent[2]));
    assert!(extends(&sent[2], &sent[3]));
    assert!(
        !extends(&sent[1], &sent[3]),
        "the refused request set no prefix"
    );
    assert!(!sent[3].contains(r#""type":"image""#));
    let records = records(&store);
    let rebuilt = rebuild_checked(&store, &records, None).unwrap().unwrap();
    assert_eq!(rebuilt.provisional, None);
    assert_eq!(
        serde_json::to_string(&rebuilt.messages).unwrap(),
        serde_json::to_string(&*agent.history()).unwrap()
    );
}

const TEXT_MODEL: &str = "text-model";
const MODEL_NOTE: &str = "[image/png file not sent: this model can't take it]";

fn with_snap(agent: Agent, scratch: &ScratchDir) -> Agent {
    agent.with_tools(
        crate::tool::Toolbox::new([
            crate::tool::snap::tool(),
            crate::tool::testkit::bash_entry(),
        ]),
        scratch.workspace(),
    )
}

async fn switch_to(agent: &mut Agent, model: &str) {
    let (ev_tx, _ev_rx) = tokio::sync::mpsc::channel(64);
    agent
        .change_settings(None, Some(model.into()), None, &ev_tx)
        .await;
}

fn note_records(records: &[LogRecord]) -> Vec<(&str, &str)> {
    pick!(records.iter().map(|r| &r.event), LogEvent::AttachmentsAsNotes { endpoint_name, model } => (endpoint_name.as_str(), model.as_str()))
}

fn notes_marks(records: &[LogRecord]) -> usize {
    pick!(records.iter().map(|r| &r.event), LogEvent::RebuildMarked { reason, .. } => reason)
        .into_iter()
        .filter(|reason| *reason == "attachments_as_notes")
        .count()
}

fn coded_400() -> String {
    http::json(
        "400 Bad Request",
        &json!({"error": {"message": CANT_PROCESS, "code": 400}}).to_string(),
    )
}

async fn switched_with_image(
    responses: Vec<String>,
) -> (Agent, MemoryStore, Arc<Mutex<Vec<String>>>) {
    let mut script = vec![reply()];
    script.extend(responses);
    let (base, seen) = mock_server(script).await;
    let store = MemoryStore::default();
    let mut agent = agent(&base, &store);
    let (outcome, _) = turn(&mut agent, "what is this?", &[RED]).await;
    completed(&outcome);
    switch_to(&mut agent, TEXT_MODEL).await;
    (agent, store, seen)
}

#[tokio::test]
async fn a_model_without_a_row_gets_notes_for_accepted_attachments_and_the_switch_back_sends_them()
{
    for refusal in [http_400(CANT_PROCESS), http_404_image_input(), coded_400()] {
        let (mut agent, store, seen) =
            switched_with_image(vec![refusal.clone(), http_400("still refused")]).await;
        let (outcome, events) = turn(&mut agent, "What does the error say?", &[]).await;
        assert_eq!(error_message(&outcome), "still refused");
        assert!(withdrawn(&events).is_empty());
        let sent = bodies(&seen);
        assert_eq!(sent.len(), 3);
        assert_eq!(sent[2], without_image(&sent[1], MODEL_NOTE));
        assert!(noted(&events).is_empty());
        assert!(note_records(&records(&store)).is_empty());

        let (mut agent, store, seen) =
            switched_with_image(vec![refusal, reply(), reply(), reply()]).await;
        let (outcome, events) = turn(&mut agent, "What does the error say?", &[]).await;
        completed(&outcome);
        assert_eq!(retries(&events), 0, "a re-send is not a retry");
        assert!(withdrawn(&events).is_empty());
        assert_eq!(noted(&events), [(TEXT_MODEL.to_owned(), 1, 0)]);
        let (outcome, events) = turn(&mut agent, "And now?", &[]).await;
        completed(&outcome);
        assert!(noted(&events).is_empty());
        switch_to(&mut agent, crate::agent::DEFAULT_MODEL).await;
        let (outcome, _) = turn(&mut agent, "Back again.", &[]).await;
        completed(&outcome);

        let sent = bodies(&seen);
        assert_eq!(sent.len(), 5);
        assert_eq!(sent[2], without_image(&sent[1], MODEL_NOTE));
        assert!(extends(&sent[2], &sent[3]), "\n{}\n{}", sent[2], sent[3]);
        assert_eq!(images_in(&sent[3]), 0);
        assert!(extends(&sent[0], &sent[4]), "\n{}\n{}", sent[0], sent[4]);
        assert_eq!(images_in(&sent[4]), 1);
        let records = records(&store);
        assert_eq!(note_records(&records), [("env", TEXT_MODEL)]);
        assert_eq!(notes_marks(&records), 0, "the switch marked first");
        let rebuilt = rebuild_checked(&store, &records, None).unwrap().unwrap();
        assert_eq!(rebuilt.messages, &*agent.history());
    }

    let scratch = ScratchDir::new("wisp-withdraw-", "same-pair");
    let (base, _) = mock_server(vec![
        http::sse(SNAP_PDF_CALL),
        reply(),
        http_400(CANT_PROCESS),
        reply(),
    ])
    .await;
    let store = MemoryStore::default();
    let mut agent = with_snap(agent(&base, &store), &scratch);
    switch_to(&mut agent, TEXT_MODEL).await;
    completed(&turn(&mut agent, "snap the spec", &[]).await.0);
    let (outcome, events) = turn(&mut agent, "and now?", &[]).await;
    completed(&outcome);
    assert_eq!(noted(&events), [(TEXT_MODEL.to_owned(), 0, 1)]);
    let records = test_guard::records(&store);
    assert_eq!(
        notes_marks(&records),
        1,
        "a same-pair change is an expected rebuild"
    );
}

#[tokio::test]
async fn a_resumed_session_on_a_learned_model_sends_notes_first() {
    let (mut live, store, seen) =
        switched_with_image(vec![http_404_image_input(), reply(), reply()]).await;
    let (outcome, _) = turn(&mut live, "What does the error say?", &[]).await;
    completed(&outcome);
    let records = records(&store);
    let (outcome, _) = turn(&mut live, "And now?", &[]).await;
    completed(&outcome);
    let live_next = bodies(&seen).pop().unwrap();

    let resumed_store = store.sharing_blobs();
    let mut writer = resumed_store.clone();
    for record in &records {
        writer.append("s1", record).unwrap();
    }
    let (base, resumed_seen) = mock_server(vec![reply()]).await;
    let provider = AnthropicProvider::new(&api_key("k"), &base).unwrap();
    let seq = u64::try_from(records.len()).unwrap();
    let mut resumed = Agent::new(Arc::new(provider), default_session())
        .with_clock(crate::clock::TickingClock::new())
        .with_retry_policy(retry::quick())
        .with_log(
            SessionLog::continuing(Box::new(resumed_store.clone()), "s1", seq),
            log_context(),
        )
        .resume(&records, 0)
        .unwrap();
    let (outcome, events) = turn(&mut resumed, "And now?", &[]).await;
    completed(&outcome);
    assert!(noted(&events).is_empty(), "the pair was learned before");
    let sent = bodies(&resumed_seen);
    assert_eq!(sent.len(), 1, "no refusal");
    assert!(sent[0].contains(&json!(MODEL_NOTE).to_string()));
    assert_eq!(without_stamps(&sent[0]), without_stamps(&live_next));
}

#[tokio::test]
async fn a_tool_file_in_the_trial_step_is_withdrawn_and_the_older_image_goes_as_a_note() {
    let again = SNAP_PDF_CALL.replace("toolu_snap", "toolu_snap_2");
    let (agent, store, seen) = switched_with_image(vec![
        http::sse(SNAP_PDF_CALL),
        http_400(CANT_PROCESS),
        http_400(CANT_PROCESS),
        reply(),
        http::sse(&again),
        reply(),
    ])
    .await;
    let scratch = ScratchDir::new("wisp-withdraw-", "notes");
    let mut agent = with_snap(agent, &scratch);
    let (outcome, events) = turn(&mut agent, "snap the spec", &[]).await;
    completed(&outcome);
    let items = withdrawn(&events);
    assert_eq!(items.len(), 1, "{items:#?}");
    assert_eq!(items[0].label, "snap.pdf");
    assert_eq!(items[0].note, FILE_NOTE);
    assert_eq!(noted(&events), [(TEXT_MODEL.to_owned(), 1, 0)]);
    let sent = bodies(&seen);
    assert_eq!(sent.len(), 5);
    let accepted = &sent[4];
    assert!(accepted.contains(&json!(FILE_NOTE).to_string()));
    assert!(accepted.contains(&json!(MODEL_NOTE).to_string()));
    assert!(!accepted.contains(r#""type":"image""#) && !accepted.contains(r#""type":"document""#));
    let history = serde_json::to_string(&*agent.history()).unwrap();
    assert_eq!(images_in(&history), 1);
    assert!(!history.contains(&wisp_media::encode_base64(crate::tool::snap::PDF)[..64]));
    assert!(history.contains(&json!(FILE_NOTE).to_string()));

    let (outcome, _) = turn(&mut agent, "snap it again", &[]).await;
    completed(&outcome);
    let sent = bodies(&seen);
    assert_eq!(sent.len(), 7);
    let refused =
        wisp_tool_text::files::uncarried("snap.pdf", "application/pdf", &[], Some("bash"));
    assert!(sent[6].contains(&refused), "{}", sent[6]);
    assert!(!sent[6].contains(r#""type":"document""#));
    assert_eq!(note_records(&records(&store)).len(), 1);
}
