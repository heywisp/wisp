#![allow(
    clippy::unwrap_used,
    clippy::panic,
    reason = "fixture helpers fail the test on any read, write or parse error"
)]

mod support;

use serde_json::json;
use support::fixture::RED_PIXEL as PIXEL;
use support::{golden, record, sha};
use wisp_protocol::log::{
    BlobSource, ImageRef, LogEvent, LoggedBlock, LoggedMessage, StoredSource, no_images,
    rebuild_history_with,
};
use wisp_protocol::{
    AgentEvent, AttachmentKind, AttachmentRef, BlobRef, ContentBlock, Fact, FileSlice, ImageType,
    MAX_MIME_LEN, MediaSource, Message, Mime, Role, ToolOutcome, ToolOutputView,
    WithdrawnAttachment,
};

const PDF: &str = "JVBERi0xLjQK";

fn mime(text: &str) -> Mime {
    text.parse().unwrap()
}

fn pdf_ref() -> AttachmentRef {
    AttachmentRef {
        media_type: mime("application/pdf"),
        sha256: sha(7),
        bytes: 912_004,
        saved: Some("~/.local/share/wisp/attachments/0707.pdf".into()),
        slice: None,
    }
}

fn finished_with(attachments: Vec<AttachmentRef>) -> AgentEvent {
    AgentEvent::ToolCallFinished {
        call_id: "toolu_09".into(),
        outcome: ToolOutcome::Completed {
            is_error: false,
            exit: None,
        },
        output: ToolOutputView::default(),
        facts: vec![
            Fact::Media {
                media_type: mime("application/pdf"),
            },
            Fact::Bytes { n: 912_004 },
        ],
        state: None,
        duration_ms: 12,
        result: Some("docs/spec.pdf: application/pdf, 912004 bytes, attached as is.".into()),
        process: None,
        attachments,
        notes: Vec::new(),
        typed: None,
    }
}

fn results_message() -> Message {
    Message {
        role: Role::User,
        content: vec![
            ContentBlock::ToolResult {
                tool_use_id: "toolu_09".into(),
                content: vec![
                    ContentBlock::Text {
                        text: "docs/spec.pdf: application/pdf".into(),
                    },
                    ContentBlock::media(mime("application/pdf"), PDF),
                ],
                is_error: false,
            },
            ContentBlock::image(ImageType::Png, PIXEL),
            ContentBlock::Text {
                text: "and this".into(),
            },
        ],
    }
}

fn blob_of(source: &MediaSource) -> BlobRef {
    BlobRef {
        media_type: source.media_type().clone(),
        sha256: sha(7),
        bytes: 9,
    }
}

struct Blobs(Option<String>);

impl BlobSource for Blobs {
    fn image(&mut self, image: &ImageRef) -> Result<String, String> {
        no_images(image)
    }

    fn media(&mut self, blob: &BlobRef) -> Result<String, String> {
        self.0
            .clone()
            .ok_or_else(|| format!("no file for {}", blob.sha256))
    }
}

#[test]
fn attachment_wire_types_match_their_fixtures() {
    golden(
        "content_block_media_pdf",
        &ContentBlock::media(mime("application/pdf"), PDF),
    );
    golden(
        "fact_media",
        &Fact::Media {
            media_type: mime("image/png"),
        },
    );
    golden(
        "fact_part",
        &Fact::Part {
            sent: 2_000,
            total: 5_120,
            more_than: false,
        },
    );
    golden("fact_tokens", &Fact::Tokens { n: 1_840 });
    golden(
        "fact_hits",
        &Fact::Hits {
            tools: 1,
            skills: 2,
            documents: 0,
        },
    );
    golden("attachment_ref", &pdf_ref());
    golden(
        "attachment_ref_unsaved",
        &AttachmentRef {
            saved: None,
            ..pdf_ref()
        },
    );
    golden(
        "event_tool_call_finished_attachments",
        &finished_with(vec![pdf_ref()]),
    );
    golden(
        "event_attachments_withdrawn_file",
        &AgentEvent::AttachmentsWithdrawn {
            step: 2,
            items: vec![WithdrawnAttachment {
                call_id: Some("toolu_09".into()),
                ordinal: 0,
                kind: AttachmentKind::File,
                label: "docs/spec.pdf".into(),
                reason: "The PDF is password protected.".into(),
                note: "[docs/spec.pdf (application/pdf) couldn't be processed: The PDF is \
password protected. Convert or cut it with bash into something you can read, then view or read \
that.]"
                    .into(),
            }],
        },
    );
    golden(
        "log_message_committed_blob",
        &record(
            9,
            LogEvent::MessageCommitted {
                turn: 3,
                message: LoggedMessage::stored(&results_message(), |s| Some(blob_of(s))),
                model: None,
                stop_reason: None,
            },
        ),
    );
    golden(
        "log_tool_call_finished_attachments",
        &record(
            8,
            LogEvent::ToolCallFinished {
                turn: 3,
                call_id: "toolu_09".into(),
                outcome: ToolOutcome::Completed {
                    is_error: false,
                    exit: None,
                },
                output: ToolOutputView::default(),
                facts: vec![Fact::Media {
                    media_type: mime("application/pdf"),
                }],
                state: None,
                duration_ms: 12,
                result: None,
                process: None,
                attachments: vec![pdf_ref()],
                notes: None,
                typed: None,
            },
        ),
    );
}

#[test]
fn a_media_block_reads_like_an_image_block_with_any_media_type() {
    assert_eq!(
        serde_json::to_value(ContentBlock::media(mime("image/png"), PIXEL)).unwrap(),
        json!({"type": "media", "source": {"type": "base64", "media_type": "image/png", "data": PIXEL}})
    );
    assert_eq!(
        serde_json::to_value(ContentBlock::image(ImageType::Png, PIXEL)).unwrap()["source"],
        serde_json::to_value(ContentBlock::media(mime("image/png"), PIXEL)).unwrap()["source"],
    );
    for bad in [
        json!({"type": "media", "source": {"type": "base64", "media_type": "Image/PNG", "data": "AA=="}}),
        json!({"type": "media", "source": {"type": "url", "url": "https://example.com/a.pdf"}}),
        json!({"type": "media", "source": {"type": "base64", "media_type": "text/plain; charset=utf-8", "data": "AA=="}}),
    ] {
        assert!(
            serde_json::from_value::<ContentBlock>(bad.clone()).is_err(),
            "{bad}"
        );
    }
}

#[test]
fn a_media_type_is_lowercase_type_and_subtype_without_parameters() {
    for good in [
        "image/png",
        "application/pdf",
        "image/svg+xml",
        "text/x-c++",
        "application/vnd.openxmlformats-officedocument.wordprocessingml.document",
        "audio/mp4",
    ] {
        let parsed = mime(good);
        assert_eq!(parsed.as_str(), good);
        assert_eq!(parsed.to_string(), good);
        assert_eq!(serde_json::from_value::<Mime>(json!(good)).unwrap(), parsed);
    }
    let longest = format!("a/{}", "b".repeat(MAX_MIME_LEN - 2));
    assert!(longest.parse::<Mime>().is_ok());
    let too_long = format!("a/{}", "b".repeat(MAX_MIME_LEN - 1));
    for bad in [
        "",
        "image",
        "image/",
        "/png",
        "Image/png",
        "image/PNG",
        "image/png; charset=x",
        "image /png",
        "a/b/c",
        "image/.png",
        ".image/png",
        "image/p\u{e9}g",
        too_long.as_str(),
    ] {
        assert!(bad.parse::<Mime>().is_err(), "{bad:?}");
        assert!(
            serde_json::from_value::<Mime>(json!(bad)).is_err(),
            "{bad:?}"
        );
    }
    let pdf = mime("application/pdf");
    assert_eq!((pdf.top_level(), pdf.subtype()), ("application", "pdf"));
    assert_eq!(Mime::from(ImageType::Jpeg), mime("image/jpeg"));
}

#[test]
fn a_tool_call_finished_without_attachments_writes_no_field() {
    let plain = serde_json::to_value(finished_with(Vec::new())).unwrap();
    assert!(plain.get("attachments").is_none(), "{plain}");
    let mut old = plain.clone();
    old.as_object_mut().unwrap().remove("attachments");
    assert_eq!(
        serde_json::from_value::<AgentEvent>(old).unwrap(),
        finished_with(Vec::new())
    );
    let unsaved = serde_json::to_value(AttachmentRef {
        saved: None,
        ..pdf_ref()
    })
    .unwrap();
    assert!(unsaved.get("saved").is_none(), "{unsaved}");
    assert!(unsaved.get("slice").is_none(), "{unsaved}");
}

#[test]
fn a_logged_message_keeps_references_and_resolves_back_to_base64() {
    let message = results_message();
    let stored = LoggedMessage::stored(&message, |s| Some(blob_of(s)));
    let text = serde_json::to_string(&stored).unwrap();
    assert!(!text.contains(PDF), "{text}");
    assert!(
        text.contains(
            r#"{"type":"media","source":{"type":"blob","media_type":"application/pdf","sha256":"#
        ),
        "{text}"
    );
    assert!(text.contains(PIXEL), "pasted images stay inline: {text}");
    let back: LoggedMessage = serde_json::from_str(&text).unwrap();
    assert_eq!(back, stored);
    let LoggedBlock::ToolResult { content, .. } = &back.content[0] else {
        panic!("{back:?}")
    };
    assert!(matches!(
        content[1],
        LoggedBlock::Media {
            source: StoredSource::Blob(_),
            slice: None
        }
    ));
    assert_eq!(back.inline(), None, "a reference needs the store");
    assert_eq!(back.resolve(&mut Blobs(Some(PDF.into()))).unwrap(), message);
    let (missing, reason) = back.resolve(&mut Blobs(None)).unwrap_err();
    assert_eq!(
        (missing, reason),
        (sha(7), format!("no file for {}", sha(7)))
    );

    let inline = LoggedMessage::from(message.clone());
    let text = serde_json::to_string(&inline).unwrap();
    assert_eq!(text, serde_json::to_string(&message).unwrap());
    let back: LoggedMessage = serde_json::from_str(&text).unwrap();
    assert_eq!(back.inline(), Some(message));
}

#[test]
fn file_slice_validates_bounds_and_pins_json_shape() {
    for (slice, wire) in [
        (
            FileSlice::pages(5, 5),
            r#"{"type":"pages","start":5,"end":5}"#,
        ),
        (
            FileSlice::pages(14, 16),
            r#"{"type":"pages","start":14,"end":16}"#,
        ),
        (
            FileSlice::seconds(30, 90),
            r#"{"type":"seconds","start":30,"end":90}"#,
        ),
        (FileSlice::sheet(2), r#"{"type":"sheet","index":2}"#),
    ] {
        let slice = slice.unwrap();
        assert_eq!(serde_json::to_string(&slice).unwrap(), wire);
        assert_eq!(serde_json::from_str::<FileSlice>(wire).unwrap(), slice);
    }
    for wire in [
        r#"{"type":"pages","start":0,"end":2}"#,
        r#"{"type":"pages","start":2,"end":1}"#,
        r#"{"type":"pages","start":1}"#,
        r#"{"type":"pages","start":1,"end":4294967296}"#,
        r#"{"type":"seconds","start":2,"end":2}"#,
        r#"{"type":"seconds","start":2,"end":1}"#,
        r#"{"type":"seconds","start":-1,"end":1}"#,
        r#"{"type":"sheet","index":0}"#,
        r#"{"type":"sheet","index":4294967296}"#,
    ] {
        assert!(serde_json::from_str::<FileSlice>(wire).is_err(), "{wire}");
    }
    let slice = FileSlice::pages(14, 16).unwrap();
    let attachment = AttachmentRef {
        slice: Some(slice),
        ..pdf_ref()
    };
    let wire = serde_json::to_value(&attachment).unwrap();
    assert_eq!(
        wire["slice"],
        json!({"type": "pages", "start": 14, "end": 16})
    );
    assert_eq!(
        serde_json::from_value::<AttachmentRef>(wire).unwrap(),
        attachment
    );
    assert_eq!(slice.read_only("–"), "read only pages 14–16");
    assert_eq!(slice.to_string(), "pages 14-16");
}

#[test]
fn same_blob_with_two_slices_rebuilds_distinct_slices() {
    let slices = [
        FileSlice::pages(5, 5).unwrap(),
        FileSlice::pages(14, 16).unwrap(),
    ];
    let message = Message {
        role: Role::User,
        content: vec![ContentBlock::ToolResult {
            tool_use_id: "toolu_sliced".into(),
            content: slices
                .iter()
                .map(|slice| ContentBlock::media_sliced(mime("application/pdf"), PDF, Some(*slice)))
                .collect(),
            is_error: false,
        }],
    };
    let stored = LoggedMessage::stored(&message, |source| Some(blob_of(source)));
    let LoggedBlock::ToolResult { content, .. } = &stored.content[0] else {
        panic!("{stored:?}")
    };
    for (block, slice) in content.iter().zip(slices) {
        assert!(
            matches!(block, LoggedBlock::Media { source: StoredSource::Blob(blob), slice: Some(found) } if blob.sha256 == sha(7) && *found == slice)
        );
    }
    assert_eq!(content.len(), 2);
    assert_eq!(
        serde_json::to_value(&content[0]).unwrap(),
        json!({
            "type": "media",
            "source": {"type": "blob", "media_type": "application/pdf", "sha256": sha(7), "bytes": 9},
            "slice": {"type": "pages", "start": 5, "end": 5},
        })
    );
    let events = [LogEvent::MessageCommitted {
        turn: 4,
        message: stored,
        model: None,
        stop_reason: None,
    }];
    let events: [LogEvent; 1] =
        serde_json::from_slice(&serde_json::to_vec(&events).unwrap()).unwrap();
    let rebuilt = rebuild_history_with(events.iter(), Blobs(Some(PDF.into()))).unwrap();
    assert_eq!(rebuilt.messages, [message]);
}

#[test]
fn the_rebuild_restores_media_from_the_store_and_names_the_turn_it_cannot() {
    let events = [LogEvent::MessageCommitted {
        turn: 4,
        message: LoggedMessage::stored(&results_message(), |s| Some(blob_of(s))),
        model: None,
        stop_reason: None,
    }];
    let rebuilt = rebuild_history_with(events.iter(), Blobs(Some(PDF.into()))).unwrap();
    assert_eq!(rebuilt.messages, [results_message()]);
    let error = rebuild_history_with(events.iter(), Blobs(None)).unwrap_err();
    assert_eq!((error.turn, error.sha256.as_str()), (4, sha(7).as_str()));
    assert_eq!(
        error.to_string(),
        format!(
            "turn 4: attachment {} can't be restored: no file for {}",
            sha(7),
            sha(7)
        )
    );
}

#[test]
fn a_lost_attachment_reads_as_its_note_in_every_place_it_was_sent() {
    let committed = LogEvent::MessageCommitted {
        turn: 4,
        message: LoggedMessage::stored(&results_message(), |s| Some(blob_of(s))),
        model: None,
        stop_reason: None,
    };
    let error = rebuild_history_with([&committed], Blobs(None)).unwrap_err();
    assert_eq!(error.kind, wisp_protocol::log::DamageKind::Image);
    let note = "[attachment no longer available: application/pdf, 1 KB]";
    let lost = LogEvent::ImageLost {
        turn: 4,
        sha256: sha(7),
        note: note.into(),
    };
    let rebuilt = rebuild_history_with([&committed, &lost], Blobs(None)).unwrap();
    let text = |t: &str| ContentBlock::Text { text: t.into() };
    let expected = Message {
        role: Role::User,
        content: vec![
            ContentBlock::ToolResult {
                tool_use_id: "toolu_09".into(),
                content: vec![text("docs/spec.pdf: application/pdf"), text(note)],
                is_error: false,
            },
            ContentBlock::image(ImageType::Png, PIXEL),
            text("and this"),
        ],
    };
    assert_eq!(rebuilt.messages, [expected]);
    let other = LogEvent::ImageLost {
        turn: 3,
        sha256: sha(7),
        note: note.into(),
    };
    assert!(rebuild_history_with([&committed, &other], Blobs(None)).is_err());
}

struct Answers;

impl BlobSource for Answers {
    fn image(&mut self, image: &ImageRef) -> Result<String, String> {
        Ok(format!("image {}", image.sha256))
    }

    fn media(&mut self, blob: &BlobRef) -> Result<String, String> {
        Ok(format!("media {}", blob.sha256))
    }

    fn sha256(&mut self, base64: &str) -> Result<String, String> {
        Ok(format!("hash {base64}"))
    }
}

fn through_mut<B: BlobSource>(
    mut source: B,
) -> (
    Result<String, String>,
    Result<String, String>,
    Result<String, String>,
) {
    let image = ImageRef {
        sha256: sha(1),
        media_type: ImageType::Png,
        bytes: 9,
        width: 1,
        height: 1,
    };
    let blob = BlobRef {
        media_type: mime("application/pdf"),
        sha256: sha(2),
        bytes: 9,
    };
    (
        source.image(&image),
        source.media(&blob),
        source.sha256("AAAA"),
    )
}

#[test]
fn a_borrowed_blob_source_answers_every_call_as_its_store_would() {
    let mut store = Answers;
    assert_eq!(
        through_mut(&mut store),
        (
            Ok(format!("image {}", sha(1))),
            Ok(format!("media {}", sha(2))),
            Ok("hash AAAA".to_owned()),
        )
    );
}
