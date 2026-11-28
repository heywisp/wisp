use wisp_protocol::ToolPurpose;
use wisp_protocol::context::ItemEstimate;

use super::log::scratch;
use super::*;
use crate::agent::testing::{custom_caps, noted, seeing_connection};
use crate::session::rebuild;
use crate::tool::{Toolbox, snap};

fn snap_call(id: &str, input: Value, tokens: u64) -> Step {
    named_call(id, "snap", input, tokens)
}

pub(super) fn named_call(id: &str, name: &str, input: Value, tokens: u64) -> Step {
    named_call_with(id, name, input, tokens, 20)
}

pub(super) fn named_call_with(id: &str, name: &str, input: Value, tokens: u64, out: u64) -> Step {
    let mut script = vec![start(tokens, 0)];
    script.extend(tool_use(0, id, name, &[&input.to_string()]));
    script.extend(finish(StopReason::ToolUse, tokens, 0, out));
    Step::from(script)
}

fn bytes_of<T: serde::Serialize + ?Sized>(value: &T) -> String {
    serde_json::to_string(value).unwrap()
}

#[tokio::test]
async fn a_tool_s_file_rebuilds_byte_for_byte_and_leaves_the_view_at_a_compaction() {
    let scratch = scratch();
    let store = MemoryStore::default();
    let provider = scripted(vec![
        snap_call("t1", json!({"pdf": true}), 1_000),
        answer("Read it.", 1_100),
        compaction(SUMMARY),
        snap_call("t2", json!({"name": "b.png"}), 1_200),
        answer("Seen it.", 1_300),
    ]);
    let agent = logged(provider.clone(), &store)
        .with_tools(Toolbox::new([snap::tool()]), scratch.workspace())
        .with_platform("Linux")
        .with_resources(None);
    let mut live = Live::start(agent);
    assert!(completed(&live.turn("u1", "read the pdf").await));
    assert!(matches!(
        live.compact(2, None).await,
        AgentEvent::Compacted { .. }
    ));
    assert!(completed(&live.turn("u2", "now the picture").await));
    let (agent, _) = live.close().await;

    let pdf = wisp_media::encode_base64(snap::PDF);
    let png = wisp_media::encode_base64(&snap::png());
    let sent = |n: usize| provider.body(n)["messages"].to_string();
    assert!(sent(1).contains(&pdf));
    assert!(
        sent(2).contains(&pdf),
        "the compaction reads the whole view"
    );
    assert!(!sent(3).contains(&pdf) && !sent(4).contains(&pdf));
    assert!(sent(4).contains(&png));

    let text = head_text(&agent);
    assert!(
        text.contains("- turn 1: snap snap.pdf \u{b7} done \u{b7} attached application/pdf\n"),
        "{text}"
    );
    assert!(text.contains(&format!("{ATTACHED_LEFT}\n</tool_calls>")));
    let head = bytes_of(&agent.compaction.head.as_ref().unwrap().message);
    assert!(
        !head.contains(&pdf[..64]),
        "the head names the file, never inlines it"
    );

    let logged = log_events(&store);
    let cut = logged
        .iter()
        .position(|e| matches!(e, LogEvent::ContextCompacted { .. }))
        .unwrap();
    let from = agent.compaction.head.as_ref().unwrap().from;
    let before = rebuild(&store, logged[..cut].iter()).unwrap();
    assert_eq!(
        bytes_of(&before.messages),
        bytes_of(&agent.history()[..from])
    );
    assert_eq!(bytes_of(&before.view()), bytes_of(&agent.history()[..from]));
    let after = rebuild(&store, logged.iter()).unwrap();
    assert_eq!(bytes_of(&after.messages), bytes_of(&agent.history()));
    assert_eq!(bytes_of(&after.view()), bytes_of(&agent.view_messages()));
    assert!(bytes_of(&after.messages).contains(&pdf));
    assert!(!bytes_of(&after.view()).contains(&pdf));

    let rows = agent.categories(agent.history.len(), false);
    let row = rows
        .iter()
        .find(|r| r.category == Category::Attachments)
        .unwrap();
    assert_eq!(
        row.items,
        [ItemEstimate {
            name: "b.png".into(),
            tokens: 0,
            turn: Some(2),
            unestimated: true,
        }]
    );
    assert_eq!(agent.media_in_use(None, &[]), (1, png.len()));
}

#[test]
fn the_ledger_names_attached_and_withdrawn_files() {
    let provider = scripted(vec![]);
    let mut agent = Agent::new(provider, default_session());
    let requested = |id: &str, target: &str| AgentEvent::ToolCallRequested {
        step: 1,
        block: 0,
        call_id: id.into(),
        tool: "snap".into(),
        input: json!({}),
        display: ToolDisplay {
            action: ToolAction::Read,
            verbs: None,
            target: target.into(),
            output: wisp_protocol::OutputKind::None,
            view: None,
            title: None,
            purpose: ToolPurpose::Run,
        },
        refused: false,
    };
    let finished = |id: &str, files: &[(&str, Option<wisp_protocol::FileSlice>)]| {
        let mut event = AgentEvent::call_finished(
            id.to_owned(),
            ToolOutcome::Completed {
                is_error: false,
                exit: None,
            },
        );
        if let AgentEvent::ToolCallFinished { attachments, .. } = &mut event {
            *attachments = files
                .iter()
                .map(|(t, slice)| wisp_protocol::AttachmentRef {
                    media_type: snap::mime(t),
                    sha256: "0".repeat(64),
                    bytes: 1,
                    saved: None,
                    slice: *slice,
                })
                .collect();
        }
        event
    };
    agent.note_event(&requested("w", "shots/a.png"), None);
    agent.note_event(&finished("w", &[("image/png", None)]), None);
    agent.note_event(
        &AgentEvent::AttachmentsWithdrawn {
            step: 1,
            items: vec![wisp_protocol::WithdrawnAttachment {
                call_id: Some("w".into()),
                ordinal: 0,
                kind: wisp_protocol::AttachmentKind::File,
                label: "shots/a.png".into(),
                reason: "Could not process image".into(),
                note: "[shots/a.png (image/png) couldn't be processed]".into(),
            }],
        },
        None,
    );
    let text = super::wisp_layout(&agent, (1, (1, 1)), None);
    assert!(
        text.contains(
            "- turn 0: snap shots/a.png \u{b7} done \u{b7} attached image/png (withdrawn)\n"
        ),
        "{text}"
    );
    assert!(
        !text.contains(ATTACHED_LEFT),
        "nothing attached is left to name"
    );

    agent.note_event(&requested("p", "docs"), None);
    let slice = wisp_protocol::FileSlice::pages(14, 16).unwrap();
    agent.note_event(
        &finished(
            "p",
            &[("application/pdf", Some(slice)), ("image/png", None)],
        ),
        None,
    );
    let text = super::wisp_layout(&agent, (1, (1, 1)), None);
    assert!(
        text.contains(
            "- turn 0: snap docs \u{b7} done \u{b7} attached application/pdf (read only pages 14-16), image/png\n"
        ),
        "{text}"
    );
    assert!(text.contains(ATTACHED_LEFT));
}

#[test]
fn the_pasted_image_limit_counts_only_what_the_view_sends() {
    let mut agent = Agent::new(scripted(vec![]), default_session());
    let caps = crate::provider::anthropic::capabilities::row(DEFAULT_MODEL)
        .unwrap()
        .images
        .unwrap();
    let image = || ContentBlock::image(wisp_protocol::ImageType::Png, "AA==");
    agent.history.push(Message {
        role: Role::User,
        content: (0..caps.per_request).map(|_| image()).collect(),
    });
    agent.history.push(Message {
        role: Role::Assistant,
        content: vec![ContentBlock::Text {
            text: "seen".into(),
        }],
    });
    let pasted = crate::agent::images::PreparedImage {
        sha256: String::new(),
        media_type: wisp_protocol::ImageType::Png,
        bytes: Vec::new(),
        base64: "AA==".into(),
        width: 1,
        height: 1,
    };
    let pasted = std::slice::from_ref(&pasted);
    assert!(agent.check_caps(DEFAULT_MODEL, caps, pasted, None).is_err());
    agent.compaction.head = Some(Head {
        epoch: 1,
        from: agent.history.len(),
        message: Message::user_text("head"),
        summary: String::new(),
        native: false,
        turns: (1, 1),
        cut: agent.history.len(),
        placement: wisp_protocol::Placement::Head,
        additions: Vec::new(),
        join: None,
    });
    assert!(agent.check_caps(DEFAULT_MODEL, caps, pasted, None).is_ok());
    assert_eq!(agent.media_in_use(None, &[]), (0, 0));
}

fn seeing_endpoints(agent: Agent, provider: &Arc<Fake>) -> Agent {
    let row = crate::provider::anthropic::capabilities::row(DEFAULT_MODEL);
    let connections = vec![
        seeing_connection("a", provider, row),
        seeing_connection("b", provider, custom_caps(None)),
    ];
    agent.with_connections(connections, vec![], "a")
}

const IMAGE_NOTE: &str = "[image/png file not sent: this model can't take it]";

#[tokio::test]
async fn a_model_listed_without_images_gets_notes_from_its_first_request() {
    let store = MemoryStore::default();
    let provider = scripted(vec![
        answer("Seen.", 1_000),
        answer("Noted.", 1_100),
        compaction(SUMMARY),
        answer("Again.", 1_200),
        answer("Back.", 1_300),
    ]);
    let mut live = Live::start(seeing_endpoints(
        logged(provider.clone(), &store),
        &provider,
    ));
    assert!(completed(&live.image_turn("u1", "look").await));
    switch(&mut live, 1, Some("b"), None).await;
    assert!(completed(&live.turn("u2", "what does it say?").await));
    assert!(matches!(
        live.compact(2, None).await,
        AgentEvent::Compacted { .. }
    ));
    live.send(crate::agent::tool_tests::rewind(3, 2)).await;
    live.until(|e| matches!(e, AgentEvent::Rewound { .. }))
        .await;
    assert!(completed(&live.turn("u3", "and now?").await));
    switch(&mut live, 4, Some("a"), None).await;
    assert!(completed(&live.turn("u4", "back").await));
    let (agent, events) = live.close().await;

    let texts = provider.texts();
    assert_eq!(texts.len(), 5);
    assert!(texts[0].contains(r#""type":"image""#));
    for text in &texts[1..4] {
        assert!(text.contains(&json!(IMAGE_NOTE).to_string()), "{text}");
        assert!(!text.contains(r#""type":"image""#), "{text}");
    }
    super::background::extends(&provider, 1, 2);
    let first = |n: usize| {
        strip_cache_control(&provider.body(n))["messages"]
            .as_array()
            .unwrap()[..2]
            .to_vec()
    };
    assert_eq!(first(3), first(1), "the rewound turn keeps the notes");
    super::background::extends(&provider, 0, 4);
    assert!(texts[4].contains(r#""type":"image""#));
    assert_eq!(noted(&events), [(DEFAULT_MODEL.to_owned(), 1, 0)]);
    let records = log_events(&store);
    let learned = pick!(&records, LogEvent::AttachmentsAsNotes { endpoint_name, .. } => endpoint_name.as_str());
    assert_eq!(learned, ["b"]);
    assert_eq!(rebuilt(&store), *agent.history());
}

#[tokio::test]
async fn a_file_left_from_a_stopped_step_is_withdrawn_before_the_first_send_on_a_notes_model() {
    let scratch = scratch();
    let store = MemoryStore::default();
    let provider = scripted(vec![
        snap_call("t1", json!({"pdf": true}), 1_000),
        Step::hang().then(ClientCommand::Cancel),
        answer("Fine.", 1_100),
    ]);
    let agent = logged(provider.clone(), &store)
        .with_tools(Toolbox::new([snap::tool()]), scratch.workspace())
        .with_platform("Linux")
        .with_resources(None);
    let mut live = Live::start(seeing_endpoints(agent, &provider));
    provider.send_to(live.commands.clone().unwrap());
    live.turn("u1", "read the pdf").await;
    provider.send_to(mpsc::channel(1).0);
    switch(&mut live, 1, Some("b"), None).await;
    assert!(completed(&live.turn("u2", "and?").await));
    let (agent, events) = live.close().await;

    let reasons = pick!(&events, AgentEvent::AttachmentsWithdrawn { items, .. } => items.iter().map(|i| i.reason.clone()).collect::<Vec<_>>());
    assert_eq!(reasons, [vec!["this model can't take it".to_owned()]]);
    let pdf = wisp_media::encode_base64(snap::PDF);
    let texts = provider.texts();
    assert!(
        texts[1].contains(&pdf[..64]),
        "the stopped step carried the file"
    );
    assert!(!texts[2].contains(&pdf[..64]) && !texts[2].contains(r#""type":"document""#));
    assert!(!bytes_of(&*agent.history()).contains(&pdf[..64]));
}
