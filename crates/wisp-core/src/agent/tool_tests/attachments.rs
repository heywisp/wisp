use wisp_media::{MAX_ATTACHMENT_BYTES, MAX_SESSION_MEDIA_BASE64};
use wisp_protocol::context::{Category, ItemEstimate};
use wisp_protocol::log::LogRecord;
use wisp_protocol::{
    AttachmentKind, AttachmentRef, Fact, ImageType, MediaSource, Mime, WithdrawnAttachment,
};

use super::*;
use crate::agent::images::{PreparedImage, media_budget_sentence};
use crate::agent::testing::withdrawn;
use crate::provider::Refused;
use crate::provider::anthropic::capabilities;
use crate::session::{Blob, JsonlStore, rebuild, sha256_bytes_hex};
use crate::test_guard::text;
use crate::tool::{Toolbox, snap};

const REFUSED: &str = "Could not process file.";

pub(super) fn with_bash() -> crate::tool::ToolEntry {
    crate::tool::testkit::bash_entry()
}

fn snap_agent(provider: Arc<Fake>, env: &Env, store: Option<Box<dyn SessionStore>>) -> Agent {
    let agent = Agent::new(provider, default_session())
        .with_clock(TickingClock::new())
        .with_tools(Toolbox::new([snap::tool(), with_bash()]), env.workspace())
        .with_platform("Linux")
        .with_resources(None)
        .with_cancel_grace(Duration::from_millis(300));
    match store {
        Some(store) => agent.with_log(SessionLog::new(store, "s1"), log_context()),
        None => agent,
    }
}

fn snap(id: &str, input: Value) -> B {
    call(id, "snap", input)
}

fn png_base64() -> String {
    wisp_media::encode_base64(&snap::png())
}

fn pdf_base64() -> String {
    wisp_media::encode_base64(snap::PDF)
}

fn media(media_type: &str, data: String) -> ContentBlock {
    ContentBlock::media(snap::mime(media_type), data)
}

fn attachment_refused() -> Step {
    let error = classified(
        ErrorKind::InvalidRequest,
        REFUSED,
        FailurePhase::Response,
        RetryClass::Never,
    )
    .refusing(Refused::Attachment);
    Step::from(vec![Ok(ProviderEvent::Sending), Err(error)])
}

struct Done {
    call_id: String,
    outcome: ToolOutcome,
    result: Option<String>,
    facts: Vec<Fact>,
    attachments: Vec<AttachmentRef>,
}

fn calls_done(events: &[AgentEvent]) -> Vec<Done> {
    pick!(
        events,
        AgentEvent::ToolCallFinished {
            call_id,
            outcome,
            result,
            facts,
            attachments,
            ..
        } => Done {
            call_id: call_id.clone(),
            outcome: outcome.clone(),
            result: result.clone(),
            facts: facts.clone(),
            attachments: attachments.clone(),
        }
    )
}

fn result_blocks(message: &Message) -> Vec<(&str, &[ContentBlock], bool)> {
    pick!(
        &message.content,
        ContentBlock::ToolResult {
            tool_use_id,
            content,
            is_error,
        } => (tool_use_id.as_str(), content.as_slice(), *is_error)
    )
}

fn media_count(messages: &[Message]) -> usize {
    fn count(blocks: &[ContentBlock]) -> usize {
        blocks
            .iter()
            .map(|b| match b {
                ContentBlock::Media { .. } => 1,
                ContentBlock::ToolResult { content, .. } => count(content),
                _ => 0,
            })
            .sum()
    }
    messages.iter().map(|m| count(&m.content)).sum()
}

fn png_ref(saved: Option<String>) -> AttachmentRef {
    let bytes = snap::png();
    AttachmentRef {
        media_type: snap::mime("image/png"),
        sha256: sha256_bytes_hex(&bytes),
        bytes: u64::try_from(bytes.len()).unwrap(),
        saved,
        slice: None,
    }
}

#[tokio::test]
async fn a_result_carries_its_text_then_its_file_and_calls_keep_their_order() {
    let env = Env::new("attach-order");
    let scripted = scripted(vec![
        uses(&[snap("a", json!({})), snap("b", json!({"pdf": true}))]),
        done("ok"),
    ]);
    let mut agent = snap_agent(scripted.clone(), &env, None);
    let (outcome, events) = turn(&mut agent, "take two").await;
    assert!(
        matches!(outcome, TurnOutcome::Completed { .. }),
        "{events:#?}"
    );
    let requests = scripted.sent();
    assert_eq!(requests.len(), 2);
    let sent = requests[1].messages.last().unwrap();
    let png_text = format!(
        "snap.png: image/png, {} bytes, attached as is.",
        snap::png().len()
    );
    let pdf_text = format!(
        "snap.pdf: application/pdf, {} bytes, attached as is.",
        snap::PDF.len()
    );
    assert_eq!(
        result_blocks(sent),
        [
            (
                "a",
                &[text(&png_text), media("image/png", png_base64())][..],
                false
            ),
            (
                "b",
                &[text(&pdf_text), media("application/pdf", pdf_base64())][..],
                false
            ),
        ]
    );
    assert_eq!(agent.history()[2], *sent);

    let wire: Value = requests[1].body();
    let results = &wire["messages"][2]["content"];
    assert_eq!(results[0]["content"][1]["type"], "image");
    assert_eq!(results[1]["content"][1]["type"], "document");
    assert_eq!(
        results[1]["content"][1]["source"]["data"],
        json!(pdf_base64())
    );

    let done = calls_done(&events);
    let ids: Vec<&str> = done.iter().map(|d| d.call_id.as_str()).collect();
    assert_eq!(ids, ["a", "b"]);
    assert_eq!(done[0].attachments, [png_ref(None)]);
    assert_eq!(done[1].attachments.len(), 1);
    assert_eq!(
        done[1].attachments[0].media_type,
        snap::mime("application/pdf")
    );
    assert_eq!(
        done[1].attachments[0].bytes,
        u64::try_from(snap::PDF.len()).unwrap()
    );
    let facts: Vec<&Vec<wisp_protocol::Fact>> = pick!(
        &events,
        AgentEvent::ToolCallFinished { facts, .. } => facts
    );
    assert_eq!(
        facts[0][0],
        wisp_protocol::Fact::Media {
            media_type: snap::mime("image/png")
        }
    );
}

#[tokio::test]
async fn the_media_budget_counts_pasted_images_and_earlier_calls_of_the_step() {
    let env = Env::new("attach-budget");
    let scripted = scripted(vec![
        uses(&[snap("a", json!({})), snap("b", json!({"name": "b.png"}))]),
        done("ok"),
    ]);
    let mut agent = snap_agent(scripted.clone(), &env, None);
    let room = MAX_SESSION_MEDIA_BASE64 - png_base64().len();
    agent.history.push(Message {
        role: Role::User,
        content: vec![
            ContentBlock::image(ImageType::Png, "A".repeat(room)),
            text("big"),
        ],
    });
    agent.history.push(Message {
        role: Role::Assistant,
        content: vec![text("seen")],
    });
    let (outcome, events) = turn(&mut agent, "two more").await;
    assert!(
        matches!(outcome, TurnOutcome::Completed { .. }),
        "{events:#?}"
    );
    let sentence = "This file would take this conversation past 20 MB of attachments.";
    let done = calls_done(&events);
    assert_eq!(done[0].attachments.len(), 1, "the first fits exactly");
    assert!(
        done[0]
            .facts
            .iter()
            .any(|f| matches!(f, Fact::Media { .. }))
    );
    assert_eq!(done[1].attachments, []);
    assert_eq!(done[1].facts, [], "no file, so no facts about one");
    assert_eq!(
        done[1].outcome,
        ToolOutcome::Completed {
            is_error: true,
            exit: None
        }
    );
    assert_eq!(done[1].result.as_deref(), Some(sentence));
    let requests = scripted.sent();
    let sent = requests[1].messages.last().unwrap();
    let results = result_blocks(sent);
    assert_eq!(results[0].1.len(), 2);
    assert_eq!(results[1], ("b", &[text(sentence)][..], true));
    assert_eq!(media_budget_sentence(1), sentence);
    assert_eq!(
        media_budget_sentence(2),
        "These files would take this conversation past 20 MB of attachments."
    );
}

#[test]
fn the_pasted_image_count_counts_image_attachments_but_not_other_files() {
    let env = Env::new("attach-count");
    let row = capabilities::row(DEFAULT_MODEL).unwrap();
    let caps = row.images.unwrap();
    let mut agent = snap_agent(scripted(Vec::new()), &env, None);
    let pasted = PreparedImage {
        sha256: String::new(),
        media_type: ImageType::Png,
        bytes: Vec::new(),
        base64: "AA==".into(),
        width: 1,
        height: 1,
    };
    let mut content = vec![text("files")];
    content.extend((0..caps.per_request - 1).map(|_| media("image/png", "AA==".into())));
    content.extend((0..3).map(|_| media("application/pdf", "AA==".into())));
    agent.history.push(Message {
        role: Role::User,
        content: vec![ContentBlock::ToolResult {
            tool_use_id: "a".into(),
            content,
            is_error: false,
        }],
    });
    assert!(
        agent
            .check_caps(DEFAULT_MODEL, caps, std::slice::from_ref(&pasted), None)
            .is_ok()
    );
    let refused = agent
        .check_caps(DEFAULT_MODEL, caps, &[pasted.clone(), pasted], None)
        .unwrap_err();
    assert_eq!(
        refused,
        format!(
            "{DEFAULT_MODEL} takes at most {} images in a request, and this conversation would send {}.",
            caps.per_request,
            caps.per_request + 1
        )
    );
}

#[tokio::test]
async fn a_cancelled_call_carries_no_attachment() {
    let env = Env::new("attach-cancel");
    let store = MemoryStore::default();
    let scripted = scripted(vec![uses(&[snap("a", json!({"wait": true}))])]);
    let mut agent = snap_agent(scripted.clone(), &env, Some(Box::new(store.clone())));
    let (outcome, events) = turn_with(&mut agent, "wait", Some(on_started)).await;
    assert!(
        matches!(outcome, TurnOutcome::Cancelled { .. }),
        "{outcome:?}"
    );
    let done = calls_done(&events);
    assert_eq!(done.len(), 1);
    assert_eq!(done[0].outcome, ToolOutcome::Cancelled);
    assert_eq!(done[0].attachments, []);
    assert_eq!(media_count(&agent.history()), 0);
    assert_eq!(store.blob_names().unwrap(), Vec::<String>::new());
}

#[tokio::test]
async fn blobs_are_saved_before_the_call_finishes_and_the_log_keeps_references() {
    let env = Env::new("attach-log");
    let scripted = scripted(vec![uses(&[snap("a", json!({}))]), done("ok")]);
    let ops = Ops::default();
    let store = RecordingStore {
        read_gate: None,
        inner: MemoryStore::default(),
        ops: ops.clone(),
    };
    let mut agent = snap_agent(scripted.clone(), &env, Some(Box::new(store.clone())));
    let (outcome, events) = turn(&mut agent, "snap").await;
    assert!(
        matches!(outcome, TurnOutcome::Completed { .. }),
        "{events:#?}"
    );
    let ops: Vec<&str> = ops
        .lock()
        .unwrap()
        .iter()
        .copied()
        .filter(|op| *op != "sync")
        .collect();
    let started = ops.iter().position(|op| *op == "started").unwrap();
    assert_eq!(ops[started + 1..started + 3], ["blob", "append"], "{ops:?}");
    let records: Vec<LogRecord> = records(&store.inner);
    let at = records
        .iter()
        .position(|r| matches!(r.event, LogEvent::ToolCallStarted { .. }))
        .unwrap();
    let LogEvent::ToolCallFinished { attachments, .. } = &records[at + 1].event else {
        panic!("{:?}", records[at + 1])
    };
    assert_eq!(*attachments, [png_ref(None)]);
    let sha = png_ref(None).sha256;
    assert_eq!(store.inner.blob_names().unwrap(), [format!("{sha}.png")]);

    let png = png_base64();
    let logged = serde_json::to_string(&records).unwrap();
    assert!(!logged.contains(&png[..64]), "the log holds no base64");
    assert!(
        logged.contains(&format!(
            r#"{{"type":"media","source":{{"type":"blob","media_type":"image/png","sha256":"{sha}","bytes":"#
        )),
        "{logged}"
    );

    let events = log_events(&store.inner);
    let rebuilt = rebuild(&store.inner, events.iter()).unwrap().messages;
    assert_eq!(
        serde_json::to_string(&rebuilt).unwrap(),
        serde_json::to_string(&*agent.history()).unwrap()
    );
    let last = scripted.sent().last().unwrap().clone();
    let n = last.messages.len();
    let replay = Sent {
        messages: rebuilt[..n].to_vec(),
        ..last.clone()
    };
    assert_eq!(replay.text(), last.text());

    store.inner.remove_blob(&sha).unwrap();
    let missing = rebuild(&store.inner, events.iter()).unwrap_err();
    assert_eq!((missing.turn, missing.sha256.as_str()), (1, sha.as_str()));
    assert!(
        missing.to_string().starts_with("turn 1: attachment "),
        "{missing}"
    );
    store
        .inner
        .replace_blob(
            &sha,
            Blob {
                media_type: snap::mime("image/png"),
                bytes: b"not the picture".to_vec(),
            },
        )
        .unwrap();
    let corrupt = rebuild(&store.inner, events.iter()).unwrap_err();
    assert_eq!(corrupt.turn, 1);
    assert!(
        corrupt.reason.contains("doesn't match its hash"),
        "{corrupt}"
    );
}

#[tokio::test]
async fn a_saved_file_reports_where_it_lives() {
    let env = Env::new("attach-saved");
    let scripted = scripted(vec![uses(&[snap("a", json!({}))]), done("ok")]);
    let folder = env.root.join("data").join("attachments");
    let store = JsonlStore::open(env.root.join("data").join("sessions"))
        .unwrap()
        .with_attachments_dir(&folder);
    let mut agent = snap_agent(scripted, &env, None).with_log(
        SessionLog::new(Box::new(store), crate::session::new_session_id()),
        log_context(),
    );
    let (_, events) = turn(&mut agent, "snap").await;
    let sha = png_ref(None).sha256;
    let path = folder.join(format!("{sha}.png"));
    assert!(path.exists(), "{events:#?}");
    assert_eq!(fs::read(&path).unwrap(), snap::png());
    assert_eq!(
        calls_done(&events)[0].attachments,
        [png_ref(Some(path.display().to_string()))]
    );
}

#[tokio::test]
async fn a_failing_store_turns_logging_off_and_still_sends_the_file() {
    let env = Env::new("attach-full");
    let scripted = scripted(vec![uses(&[snap("a", json!({}))]), done("ok")]);
    let mut agent = snap_agent(
        scripted.clone(),
        &env,
        Some(Box::new(MemoryStore::failing())),
    );
    let (outcome, events) = turn(&mut agent, "snap").await;
    assert!(matches!(outcome, TurnOutcome::Completed { .. }));
    let disabled = events
        .iter()
        .filter(|e| matches!(e, AgentEvent::LogDisabled { .. }))
        .count();
    assert_eq!(disabled, 1);
    let requests = scripted.sent();
    assert_eq!(media_count(&requests[1].messages), 1);
    assert_eq!(calls_done(&events)[0].attachments, [png_ref(None)]);
}

#[tokio::test]
async fn a_refused_file_is_withdrawn_with_a_bash_hint_and_rebuilds() {
    let env = Env::new("attach-withdraw");
    let store = MemoryStore::default();
    let scripted = scripted(vec![
        uses(&[snap("a", json!({"name": "shots/a.png"}))]),
        attachment_refused(),
        done("ok"),
    ]);
    let mut agent = snap_agent(scripted.clone(), &env, Some(Box::new(store.clone())));
    let (outcome, events) = turn(&mut agent, "snap").await;
    assert!(
        matches!(
            outcome,
            TurnOutcome::Completed {
                committed: true,
                ..
            }
        ),
        "{events:#?}"
    );
    let note = "[shots/a.png (image/png) couldn't be processed: Could not process file. Convert \
or cut it with bash into something you can read, then view or read that.]";
    assert_eq!(
        withdrawn(&events),
        [WithdrawnAttachment {
            call_id: Some("a".into()),
            ordinal: 0,
            kind: AttachmentKind::File,
            label: "shots/a.png".into(),
            reason: REFUSED.into(),
            note: note.into(),
        }]
    );
    let requests = scripted.sent();
    assert_eq!(requests.len(), 3);
    assert_eq!(media_count(&requests[1].messages), 1);
    let resent = requests[2].messages.last().unwrap();
    let results = result_blocks(resent);
    assert_eq!(results[0].1[1], text(note));
    assert_eq!(media_count(&agent.history()), 0);
    let events = log_events(&store);
    let rebuilt = rebuild(&store, events.iter()).unwrap();
    assert_eq!(rebuilt.messages, &*agent.history());
    assert_eq!(rebuilt.provisional, None);
}

#[tokio::test]
async fn two_refused_files_are_withdrawn_together_and_say_so() {
    let env = Env::new("attach-withdraw-two");
    let store = MemoryStore::default();
    let scripted = scripted(vec![
        uses(&[snap("a", json!({})), snap("b", json!({"pdf": true}))]),
        attachment_refused(),
        done("ok"),
    ]);
    let mut agent = snap_agent(scripted.clone(), &env, Some(Box::new(store.clone())));
    let (outcome, events) = turn(&mut agent, "snap twice").await;
    assert!(matches!(outcome, TurnOutcome::Completed { .. }));
    let items = withdrawn(&events);
    let notes: Vec<(&str, usize, &str)> = items
        .iter()
        .map(|i| (i.label.as_str(), i.ordinal, i.note.as_str()))
        .collect();
    assert_eq!(
        notes,
        [
            (
                "snap.png",
                0,
                "[snap.png (image/png) was withdrawn with 1 other attachment when the API refused \
this step: Could not process file. It may be fine: view it again on its own, and convert it with \
bash only if that is refused too.]"
            ),
            (
                "snap.pdf",
                1,
                "[snap.pdf (application/pdf) was withdrawn with 1 other attachment when the API \
refused this step: Could not process file. It may be fine: view it again on its own, and convert \
it with bash only if that is refused too.]"
            ),
        ]
    );
    let rebuilt = rebuild(&store, log_events(&store).iter()).unwrap();
    assert_eq!(rebuilt.messages, &*agent.history());
}

#[tokio::test]
async fn the_context_report_names_a_file_by_its_target_and_leaves_it_unestimated() {
    let env = Env::new("attach-context");
    let scripted = scripted(vec![
        uses(&[snap("a", json!({"name": "docs/spec.png"}))]),
        done("ok"),
    ]);
    let mut agent = snap_agent(scripted, &env, None);
    turn(&mut agent, "snap").await;
    let rows = agent.categories(agent.history().len(), false);
    let row = rows
        .iter()
        .find(|r| r.category == Category::Attachments)
        .unwrap();
    assert_eq!(
        row.items,
        [ItemEstimate {
            name: "docs/spec.png".into(),
            tokens: 0,
            turn: Some(1),
            unestimated: true,
        }]
    );
    assert_eq!(row.tokens, 0);
}

#[test]
fn a_media_source_reads_its_type_and_data() {
    let source = MediaSource::Base64 {
        media_type: Mime::from(ImageType::Png),
        data: "AA==".into(),
    };
    assert_eq!(source.media_type().as_str(), "image/png");
    assert_eq!(source.data(), "AA==");
}

fn refused_alone(done: &Done, sentence: &str) {
    assert_eq!(done.attachments, []);
    assert_eq!(done.facts, []);
    assert_eq!(
        done.outcome,
        ToolOutcome::Completed {
            is_error: true,
            exit: None
        }
    );
    assert_eq!(done.result.as_deref(), Some(sentence));
}

#[tokio::test]
async fn a_type_the_endpoint_cant_carry_is_refused_in_the_kernel_with_a_bash_hint() {
    const KEY: &str = "CANARY-unsupported-attachment-key";
    crate::session::register_secrets([KEY.to_owned()]);
    let env = Env::new("attach-type");
    let store = MemoryStore::default();
    let scripted = scripted(vec![
        uses(&[
            snap(
                "a",
                json!({"name": format!("{KEY}.mp3"), "type": "audio/mpeg"}),
            ),
            snap("b", json!({"pdf": true, "slice": "1"})),
        ]),
        done("ok"),
    ]);
    let mut agent = snap_agent(scripted.clone(), &env, Some(Box::new(store.clone())));
    let (outcome, events) = turn(&mut agent, "listen").await;
    assert!(
        matches!(outcome, TurnOutcome::Completed { .. }),
        "{events:#?}"
    );
    let sentence = "[redacted].mp3 is audio/mpeg, which this server can't carry (it takes \
application/pdf, image/gif, image/jpeg, image/png, image/webp). Convert it with bash to one of \
those or to text, or inspect it another way (file, xxd, strings).";
    let calls = calls_done(&events);
    refused_alone(&calls[0], sentence);
    let sliced = "No file was sent: snap.pdf can't be sliced with `pages` on this model (this model does not slice files). Cut out only the part you need with bash and view that copy, or omit `pages` to send and store the whole file.";
    refused_alone(&calls[1], sliced);
    let requests = scripted.sent();
    assert_eq!(requests.len(), 2, "the next request goes out");
    assert_eq!(media_count(&requests[1].messages), 0);
    let sent = results(requests[1].messages.last().unwrap());
    assert_eq!(sent[1].1, sliced);
    assert_eq!(
        results(requests[1].messages.last().unwrap())[0].1,
        wisp_tool_text::output::with_held_key_note(sentence)
    );
    let rebuilt = rebuild(&store, log_events(&store).iter()).unwrap().messages;
    assert_eq!(rebuilt, &*agent.history());
    assert_eq!(media_count(&agent.history()), 0);
    assert_eq!(store.blob_names().unwrap(), Vec::<String>::new());
    assert_eq!(
        wisp_tool_text::files::uncarried("x.bin", "application/zip", &[], Some("bash")),
        "x.bin is application/zip, which this server can't carry (it takes no files). Convert \
it with bash to text, or inspect it another way (file, xxd, strings)."
    );
}

struct Sh;

impl crate::plugin::testing::TestTool for Sh {
    fn name(&self) -> String {
        "sh".to_owned()
    }

    fn prepare(&self, input: &str) -> Result<Box<dyn wisp_plugin_api::tools::Prepared>, String> {
        Ok(crate::plugin::testing::Call::boxed(
            crate::plugin::testing::fallback("sh", input),
            (),
        ))
    }

    fn run<'a>(
        &'a self,
        _call: Box<dyn wisp_plugin_api::tools::Prepared>,
        _ctx: &'a dyn wisp_plugin_api::host::CallContext,
    ) -> futures::future::BoxFuture<'a, wisp_plugin_api::types::ToolRun> {
        Box::pin(async { wisp_plugin_api::types::ToolRun::completed() })
    }
}

#[tokio::test]
async fn a_shell_tool_of_another_name_is_named_in_the_notes_until_it_is_turned_off() {
    let env = Env::new("attach-sh");
    let scripted = scripted(vec![
        uses(&[
            snap("a", json!({"name": "song.mp3", "type": "audio/mpeg"})),
            snap("b", json!({"pdf": true, "slice": "1"})),
        ]),
        done("ok"),
        uses(&[
            snap("c", json!({"name": "song.mp3", "type": "audio/mpeg"})),
            snap("d", json!({"pdf": true, "slice": "1"})),
        ]),
        done("ok"),
    ]);
    let sh = crate::plugin::testing::providing(Arc::new(Sh), crate::tool::SHELL);
    let mut agent = Agent::new(scripted.clone(), default_session())
        .with_clock(TickingClock::new())
        .with_tools(Toolbox::new([snap::tool(), sh]), env.workspace())
        .with_platform("Linux")
        .with_resources(None);
    let (_, events) = turn(&mut agent, "listen").await;
    refused_alone(
        &calls_done(&events)[0],
        "song.mp3 is audio/mpeg, which this server can't carry (it takes application/pdf, \
image/gif, image/jpeg, image/png, image/webp). Convert it with sh to one of those or to text, or \
inspect it another way (file, xxd, strings).",
    );
    refused_alone(
        &calls_done(&events)[1],
        "No file was sent: snap.pdf can't be sliced with `pages` on this model (this model does not slice files). Cut out only the part you need with sh and view that copy, or omit `pages` to send and store the whole file.",
    );
    agent.toolbox.set_enabled("sh", false);
    let (_, events) = turn(&mut agent, "again").await;
    refused_alone(
        &calls_done(&events)[0],
        "song.mp3 is audio/mpeg, which this server can't carry (it takes application/pdf, \
image/gif, image/jpeg, image/png, image/webp), so it can't be read in this session.",
    );
    refused_alone(
        &calls_done(&events)[1],
        "No file was sent: snap.pdf can't be sliced with `pages` on this model (this model does not slice files). Omit `pages` to read the whole file, or switch to a model that supports `pages`; either retry sends and stores the whole file.",
    );
}

#[tokio::test]
async fn without_bash_the_kernel_refusals_name_no_bash() {
    let env = Env::new("attach-no-bash");
    let scripted = scripted(vec![
        uses(&[snap("a", json!({"name": "song.mp3", "type": "audio/mpeg"}))]),
        uses(&[snap(
            "b",
            json!({"name": "big.pdf", "size": MAX_ATTACHMENT_BYTES + 100_000}),
        )]),
        done("ok"),
    ]);
    let mut agent = Agent::new(scripted.clone(), default_session())
        .with_clock(TickingClock::new())
        .with_tools(Toolbox::new([snap::tool()]), env.workspace())
        .with_platform("Linux")
        .with_resources(None);
    let (outcome, events) = turn(&mut agent, "listen").await;
    assert!(
        matches!(outcome, TurnOutcome::Completed { .. }),
        "{events:#?}"
    );
    let done = calls_done(&events);
    refused_alone(
        &done[0],
        "song.mp3 is audio/mpeg, which this server can't carry (it takes application/pdf, \
image/gif, image/jpeg, image/png, image/webp), so it can't be read in this session.",
    );
    refused_alone(
        &done[1],
        "big.pdf is 7.6 MB; files over 7.5 MB can't be attached, so it can't be read in this \
session.",
    );
}

#[tokio::test]
async fn a_file_over_the_cap_is_refused_unsaved_and_one_at_the_cap_rebuilds() {
    const KEY: &str = "CANARY-over-cap-attachment-key";
    crate::session::register_secrets([KEY.to_owned()]);
    let env = Env::new("attach-cap");
    let store = MemoryStore::default();
    let scripted = scripted(vec![
        uses(&[
            snap(
                "a",
                json!({"name": "fits.pdf", "size": MAX_ATTACHMENT_BYTES}),
            ),
            snap(
                "b",
                json!({"name": format!("{KEY}.pdf"), "size": MAX_ATTACHMENT_BYTES + 1}),
            ),
        ]),
        done("ok"),
    ]);
    let mut agent = snap_agent(scripted.clone(), &env, Some(Box::new(store.clone())));
    let (outcome, events) = turn(&mut agent, "two").await;
    assert!(
        matches!(outcome, TurnOutcome::Completed { .. }),
        "{events:#?}"
    );
    let done = calls_done(&events);
    assert_eq!(done[0].attachments.len(), 1);
    let sentence = "[redacted].pdf is 7.6 MB; files over 7.5 MB can't be attached. Make a smaller copy with bash, \
or extract the part you need, and use that instead.";
    refused_alone(&done[1], sentence);
    let requests = scripted.sent();
    assert_eq!(
        result_blocks(requests[1].messages.last().unwrap())[1].1,
        &[text(&wisp_tool_text::output::with_held_key_note(sentence))]
    );
    assert_eq!(store.blob_names().unwrap().len(), 1);
    assert_eq!(media_count(&agent.history()), 1);
    let rebuilt = rebuild(&store, log_events(&store).iter()).unwrap().messages;
    assert_eq!(rebuilt, &*agent.history());
}

#[tokio::test]
async fn a_call_the_kernel_stopped_attaches_nothing_whatever_its_tool_reports() {
    let env = Env::new("attach-stubborn");
    let store = MemoryStore::default();
    let scripted = scripted(vec![uses(&[snap("a", json!({"stubborn": true}))])]);
    let mut agent = snap_agent(scripted.clone(), &env, Some(Box::new(store.clone())));
    let (outcome, events) = turn_with(&mut agent, "wait", Some(on_started)).await;
    assert!(
        matches!(outcome, TurnOutcome::Cancelled { .. }),
        "{outcome:?}"
    );
    let done = calls_done(&events);
    assert_eq!(done[0].outcome, ToolOutcome::Cancelled);
    assert_eq!(done[0].attachments, []);
    assert_eq!(done[0].facts, []);
    assert_eq!(
        done[0].result.as_deref(),
        Some("Stopped before the file was attached.")
    );
    assert_eq!(media_count(&agent.history()), 0);
    assert_eq!(store.blob_names().unwrap(), Vec::<String>::new());
}

#[tokio::test]
async fn a_call_stopped_by_its_time_limit_times_out_whatever_its_tool_reports() {
    let env = Env::new("attach-limit");
    let scripted = scripted(vec![
        uses(&[snap("a", json!({"stubborn": true}))]),
        done("ok"),
    ]);
    let mut agent = Agent::new(scripted, default_session())
        .with_clock(TickingClock::new())
        .with_tools(
            Toolbox::new([snap::limited(Duration::from_millis(200))]),
            env.workspace(),
        )
        .with_platform("Linux")
        .with_resources(None)
        .with_cancel_grace(Duration::from_millis(300));
    let (outcome, events) = turn(&mut agent, "wait").await;
    assert!(
        matches!(outcome, TurnOutcome::Completed { .. }),
        "{events:#?}"
    );
    let done = calls_done(&events);
    assert_eq!(done[0].outcome, ToolOutcome::TimedOut { after_ms: 200 });
    assert_eq!(done[0].attachments, []);
    assert_eq!(done[0].facts, []);
    assert_eq!(
        done[0].result.as_deref(),
        Some("Stopped before the file was attached.")
    );
    assert_eq!(media_count(&agent.history()), 0);
}

#[cfg(unix)]
mod viewing {
    use super::*;

    fn edged(scripted: Arc<Fake>, edge: u32) -> Arc<Fake> {
        let row = capabilities::row(DEFAULT_MODEL).unwrap();
        let images = crate::provider::ImageCaps {
            many_image_edge: edge,
            ..row.images.unwrap()
        };
        let caps = crate::provider::ModelCapabilities {
            images: Some(images),
            ..*row
        };
        let caps: &'static _ = Box::leak(Box::new(caps));
        scripted.view(|k| k.caps = Some(Caps::Fixed(Some(caps))))
    }

    fn sized_png(width: u32, height: u32) -> Vec<u8> {
        let picture = image::RgbImage::from_pixel(width, height, image::Rgb([1, 2, 3]));
        let mut bytes = std::io::Cursor::new(Vec::new());
        image::DynamicImage::ImageRgb8(picture)
            .write_to(&mut bytes, image::ImageFormat::Png)
            .unwrap();
        bytes.into_inner()
    }

    fn view(id: &str, path: &str) -> B {
        call(id, "view_file", json!({ "path": path }))
    }

    #[tokio::test]
    async fn a_view_uses_the_model_s_image_edge_and_streams_text_to_the_row() {
        let env = Env::new("view-edge");
        let small = sized_png(4, 2);
        fs::write(env.work.join("wide.png"), sized_png(40, 2)).unwrap();
        fs::write(env.work.join("small.png"), &small).unwrap();
        fs::write(env.work.join("notes.txt"), "one\ntwo\n").unwrap();
        let scripted = scripted(vec![
            uses(&[
                view("a", "wide.png"),
                view("b", "small.png"),
                view("c", "notes.txt"),
            ]),
            done("ok"),
        ]);
        let tools = Toolbox::new([crate::tool::testkit::view_file()]);
        let mut agent = Agent::new(edged(scripted.clone(), 10), default_session())
            .with_clock(TickingClock::new())
            .with_tools(tools, env.workspace())
            .with_platform("Linux")
            .with_resources(None);
        let (outcome, events) = turn(&mut agent, "look").await;
        assert!(
            matches!(outcome, TurnOutcome::Completed { .. }),
            "{events:#?}"
        );
        let requests = scripted.sent();
        let sent = requests[1].messages.last().unwrap();
        let small_text = format!(
            "~/work/small.png: image/png, {} bytes, attached as is.",
            small.len()
        );
        assert_eq!(
            result_blocks(sent),
            [
                (
                    "a",
                    &[text(
                        "wide.png is 40\u{d7}2; images over 10 px a side can't be attached. Resize a \
copy with bash (for example, magick wide.png -resize 10x10 wide-small.png) and view that."
                    )][..],
                    true
                ),
                (
                    "b",
                    &[
                        text(&small_text),
                        media("image/png", wisp_media::encode_base64(&small))
                    ][..],
                    false
                ),
                (
                    "c",
                    &[text(
                        "~/work/notes.txt: text, 2 lines (8 bytes), all below (about 3 tokens).\n\
one\ntwo"
                    )][..],
                    false
                ),
            ]
        );
        let streamed: String = events
            .iter()
            .filter_map(|e| match e {
                AgentEvent::ToolOutput { call_id, text } if call_id == "c" => Some(text.as_str()),
                _ => None,
            })
            .collect();
        assert_eq!(streamed, "one\ntwo\n");
        let done = calls_done(&events);
        assert_eq!(
            done[2].facts,
            [
                Fact::Part {
                    sent: 2,
                    total: 2,
                    more_than: false
                },
                Fact::Tokens { n: 3 }
            ]
        );
        let files: Vec<(String, Option<String>)> = pick!(
            &events,
            AgentEvent::ToolCallFinished {
                call_id, output, ..
            } => (call_id.clone(), output.file.clone())
        );
        let dir = agent.output_dir().unwrap();
        let kept = dir.path().join("0003-view_file.out");
        assert_eq!(
            files,
            [
                ("a".to_owned(), None),
                ("b".to_owned(), None),
                ("c".to_owned(), Some(kept.display().to_string())),
            ]
        );
        assert_eq!(fs::read_to_string(&kept).unwrap(), "one\ntwo\n");
        let mut names: Vec<String> = fs::read_dir(dir.path())
            .unwrap()
            .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
            .collect();
        names.sort();
        assert_eq!(names, ["0003-view_file.out"]);
    }
}

fn search_suspect() -> Step {
    let error = classified(
        ErrorKind::InvalidRequest,
        "tools.1: unknown tool type",
        FailurePhase::Response,
        RetryClass::Never,
    )
    .refusing(Refused::SearchSuspect);
    Step::from(vec![Ok(ProviderEvent::Sending), Err(error)])
}

fn search_records(store: &MemoryStore) -> usize {
    log_events(store)
        .iter()
        .filter(|e| matches!(e, LogEvent::NativeWebSearch { .. }))
        .count()
}

#[tokio::test]
async fn a_refused_image_on_a_gateway_is_withdrawn_and_search_stays_on() {
    let env = Env::new("attach-suspect-both");
    let store = MemoryStore::default();
    let scripted = scripted(vec![
        uses(&[snap("a", json!({"name": "shots/a.png"}))]),
        search_suspect(),
        attachment_refused(),
        done("ok"),
    ]);
    let mut agent =
        snap_agent(scripted.clone(), &env, Some(Box::new(store.clone()))).with_web_search(true);
    let (outcome, events) = turn(&mut agent, "snap").await;
    assert!(
        matches!(outcome, TurnOutcome::Completed { .. }),
        "{events:#?}"
    );
    let requests = scripted.sent();
    assert_eq!(requests.len(), 4);
    let searched: Vec<bool> = requests
        .iter()
        .map(|r| !r.server_tools.is_empty())
        .collect();
    assert_eq!(searched, [true, true, false, true]);
    let images: Vec<usize> = requests.iter().map(|r| media_count(&r.messages)).collect();
    assert_eq!(images, [0, 1, 1, 0]);
    assert_eq!(withdrawn(&events).len(), 1);
    assert_eq!(search_records(&store), 0);
    assert!(
        !events
            .iter()
            .any(|e| matches!(e, AgentEvent::ToolsChanged { .. }))
    );
}

#[tokio::test]
async fn a_gateway_refusing_both_the_image_and_the_tool_drops_each_once() {
    let env = Env::new("attach-suspect-twice");
    let store = MemoryStore::default();
    let scripted = scripted(vec![
        uses(&[snap("a", json!({"name": "shots/a.png"}))]),
        search_suspect(),
        attachment_refused(),
        search_suspect(),
        done("ok"),
    ]);
    let mut agent =
        snap_agent(scripted.clone(), &env, Some(Box::new(store.clone()))).with_web_search(true);
    let (outcome, events) = turn(&mut agent, "snap").await;
    assert!(
        matches!(outcome, TurnOutcome::Completed { .. }),
        "{events:#?}"
    );
    let requests = scripted.sent();
    let searched: Vec<bool> = requests
        .iter()
        .map(|r| !r.server_tools.is_empty())
        .collect();
    assert_eq!(searched, [true, true, false, true, false]);
    let images: Vec<usize> = requests.iter().map(|r| media_count(&r.messages)).collect();
    assert_eq!(images, [0, 1, 1, 0, 0]);
    assert_eq!(withdrawn(&events).len(), 1);
    assert_eq!(search_records(&store), 1);
}

#[tokio::test]
async fn a_new_image_on_a_gateway_without_the_tool_keeps_the_image_when_only_the_tool_was_refused()
{
    let env = Env::new("attach-suspect-tool");
    let store = MemoryStore::default();
    let scripted = scripted(vec![
        uses(&[snap("a", json!({"name": "shots/a.png"}))]),
        search_suspect(),
        done("ok"),
    ]);
    let mut agent =
        snap_agent(scripted.clone(), &env, Some(Box::new(store.clone()))).with_web_search(true);
    let (outcome, events) = turn(&mut agent, "snap").await;
    assert!(
        matches!(outcome, TurnOutcome::Completed { .. }),
        "{events:#?}"
    );
    let requests = scripted.sent();
    let searched: Vec<bool> = requests
        .iter()
        .map(|r| !r.server_tools.is_empty())
        .collect();
    assert_eq!(searched, [true, true, false]);
    let images: Vec<usize> = requests.iter().map(|r| media_count(&r.messages)).collect();
    assert_eq!(images, [0, 1, 1]);
    assert!(withdrawn(&events).is_empty());
    assert_eq!(search_records(&store), 1);
}

#[tokio::test]
async fn a_gateway_refusing_both_forever_stops_after_a_bounded_number_of_requests() {
    let env = Env::new("attach-suspect-loop");
    let store = MemoryStore::default();
    let mut steps = vec![uses(&[snap("a", json!({"name": "shots/a.png"}))])];
    for _ in 0..10 {
        steps.push(search_suspect());
        steps.push(attachment_refused());
    }
    let scripted = scripted(steps);
    let mut agent =
        snap_agent(scripted.clone(), &env, Some(Box::new(store.clone()))).with_web_search(true);
    let (outcome, _) = turn(&mut agent, "snap").await;
    assert!(matches!(outcome, TurnOutcome::Error { .. }), "{outcome:?}");
    let shapes: Vec<(bool, usize)> = scripted
        .sent()
        .iter()
        .map(|r| (!r.server_tools.is_empty(), media_count(&r.messages)))
        .collect();
    assert_eq!(
        shapes,
        [(true, 0), (true, 1), (false, 1), (true, 0), (false, 0)],
        "{shapes:?}"
    );
    assert_eq!(search_records(&store), 0);
}

#[tokio::test]
async fn a_transient_failure_on_the_second_probe_retries_exactly_that_probe() {
    let env = Env::new("attach-suspect-retry");
    let store = MemoryStore::default();
    let scripted = scripted(vec![
        uses(&[snap("a", json!({"name": "shots/a.png"}))]),
        search_suspect(),
        attachment_refused(),
        search_suspect(),
        turned_away(),
        done("seen"),
    ]);
    let mut agent = snap_agent(scripted.clone(), &env, Some(Box::new(store.clone())))
        .with_web_search(true)
        .with_retry_policy(super::super::retry::quick());
    let (outcome, _) = turn(&mut agent, "snap").await;
    assert!(
        matches!(outcome, TurnOutcome::Completed { .. }),
        "{outcome:?}"
    );
    let requests = scripted.sent();
    let shapes: Vec<(bool, usize)> = requests
        .iter()
        .map(|r| (!r.server_tools.is_empty(), media_count(&r.messages)))
        .collect();
    assert_eq!(
        shapes,
        [
            (true, 0),
            (true, 1),
            (false, 1),
            (true, 0),
            (false, 0),
            (false, 0)
        ],
        "{shapes:?}"
    );
    assert_eq!(requests[5].messages, requests[4].messages);
    assert_eq!(search_records(&store), 1);
}

#[tokio::test]
async fn a_gateway_suspecting_every_request_stops_after_one_probe() {
    let env = Env::new("attach-suspect-only");
    let store = MemoryStore::default();
    let mut steps = vec![uses(&[snap("a", json!({"name": "shots/a.png"}))])];
    for _ in 0..10 {
        steps.push(search_suspect());
    }
    let scripted = scripted(steps);
    let mut agent =
        snap_agent(scripted.clone(), &env, Some(Box::new(store.clone()))).with_web_search(true);
    let (outcome, _) = turn(&mut agent, "snap").await;
    assert!(matches!(outcome, TurnOutcome::Error { .. }), "{outcome:?}");
    assert!(scripted.sent().len() <= 5, "{}", scripted.sent().len());
    assert_eq!(search_records(&store), 0);
}

#[tokio::test]
async fn a_gateway_refusing_both_ends_with_neither_the_tool_nor_the_image() {
    let env = Env::new("attach-suspect-loop");
    let store = MemoryStore::default();
    let mut steps = vec![uses(&[snap("a", json!({"name": "shots/a.png"}))])];
    for _ in 0..6 {
        steps.push(search_suspect());
        steps.push(attachment_refused());
    }
    steps.push(done("ok"));
    let scripted = scripted(steps);
    let mut agent =
        snap_agent(scripted.clone(), &env, Some(Box::new(store.clone()))).with_web_search(true);
    let (outcome, _) = turn(&mut agent, "snap").await;
    assert!(matches!(outcome, TurnOutcome::Error { .. }), "{outcome:?}");
    let requests = scripted.sent();
    let shapes: Vec<(bool, usize)> = requests
        .iter()
        .map(|r| (!r.server_tools.is_empty(), media_count(&r.messages)))
        .collect();
    for (i, (tool, images)) in shapes.iter().enumerate().skip(1) {
        let answered_suspect = i % 2 == 1;
        assert_eq!(
            *tool, answered_suspect,
            "request {i}: a refusing gateway answers a tool with a suspect and a bad image without the tool with an image refusal: {shapes:?}"
        );
        if !answered_suspect && i < shapes.len() - 1 {
            assert_eq!(*images, 1, "{shapes:?}");
        }
    }
    assert!(
        requests.len() <= 6,
        "a gateway that refuses both the tool and the image looped for {} requests: {shapes:?}",
        requests.len()
    );
}
