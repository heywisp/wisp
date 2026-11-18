use super::*;

fn attached(viewed: &Viewed) -> (&str, Vec<u8>) {
    match viewed.run.attachments.as_slice() {
        [one] => (one.media_type.as_str(), one.bytes.clone()),
        other => panic!("{} attachments", other.len()),
    }
}

fn pdf() -> Vec<u8> {
    b"%PDF-1.4\n1 0 obj << /Type /Catalog >> endobj\ntrailer << /Root 1 0 R >>\n%%EOF\n".to_vec()
}

fn zipped(first_entry: &[u8]) -> Vec<u8> {
    let mut bytes = b"PK\x03\x04".to_vec();
    bytes.resize(30, 0);
    bytes.extend_from_slice(first_entry);
    bytes
}

#[tokio::test]
async fn carried_types_are_attached_unchanged_with_their_facts() {
    let rig = Rig::new();
    let files = [
        ("a.png", png(3, 2), "image/png"),
        ("a.gif", encoded(3, 2, image::ImageFormat::Gif), "image/gif"),
        (
            "a.webp",
            encoded(3, 2, image::ImageFormat::WebP),
            "image/webp",
        ),
        (
            "a.jpg",
            encoded(3, 2, image::ImageFormat::Jpeg),
            "image/jpeg",
        ),
        ("doc.pdf", pdf(), "application/pdf"),
        (
            "doc.docx",
            zipped(b"word/document.xml"),
            "application/vnd.openxmlformats-officedocument.wordprocessingml.document",
        ),
        (
            "sheet.xlsx",
            zipped(b"xl/workbook.xml"),
            "application/vnd.openxmlformats-officedocument.spreadsheetml.sheet",
        ),
        (
            "deck.pptx",
            zipped(b"ppt/presentation.xml"),
            "application/vnd.openxmlformats-officedocument.presentationml.presentation",
        ),
        (
            "doc.odt",
            zipped(b"mimetypeapplication/vnd.oasis.opendocument.text"),
            "application/vnd.oasis.opendocument.text",
        ),
        ("named-wrong.txt", png(3, 2), "image/png"),
    ];
    for (name, bytes, media_type) in files {
        rig.file(name, &bytes);
        let viewed = rig.view(name).await;
        assert_eq!(
            viewed.completed(),
            format!(
                "~/work/{name}: {media_type}, {}, attached as is.",
                words::size(bytes.len() as u64)
            )
        );
        assert_eq!(attached(&viewed), (media_type, bytes.clone()), "{name}");
        assert_eq!(
            viewed.run.facts,
            [
                Fact::Media(media_type.to_owned()),
                Fact::Bytes(bytes.len() as u64)
            ]
        );
        assert_eq!(viewed.captured.bytes, 0, "{name}");
    }
}

#[tokio::test]
async fn an_ascii_pdf_is_a_pdf_not_text() {
    let rig = Rig::new();
    let bytes = pdf();
    assert!(bytes.is_ascii());
    rig.file("plain.pdf", &bytes);
    let viewed = rig.view("plain.pdf").await;
    assert_eq!(attached(&viewed).0, "application/pdf");
}

#[tokio::test]
async fn types_the_endpoint_can_t_carry_are_refused_by_name() {
    let rig = Rig::new();
    let mut random: Vec<u8> = (0..4_096u32).map(|i| (i * 7919 % 251) as u8).collect();
    random[0] = 0xff;
    let files = [
        ("a.bmp", encoded(3, 2, image::ImageFormat::Bmp), "image/bmp"),
        (
            "a.zip",
            b"PK\x03\x04\x14\x00\x00\x00\x08\x00".to_vec(),
            "application/zip",
        ),
        ("noise.bin", random, "application/octet-stream"),
    ];
    let mut ole = b"\xD0\xCF\x11\xE0\xA1\xB1\x1A\xE1".to_vec();
    ole.resize(1_024, 0);
    let files = files
        .into_iter()
        .chain(["sheet.xls", "setup.msi"].map(|name| (name, ole.clone(), look::OLE_FILE)));
    for (name, bytes, media_type) in files {
        rig.file(name, &bytes);
        let viewed = rig.view(name).await;
        assert_eq!(
            viewed.refused(),
            wisp_tool_text::files::uncarried(name, media_type, &MEDIA_TYPES, Some("bash")),
            "{name}"
        );
        assert!(!viewed.refused().contains("msword"), "{name}");
    }
}

#[tokio::test]
async fn a_large_video_gets_the_type_refusal_not_the_size_one() {
    let rig = Rig::new();
    rig.sized(
        "clip.mp4",
        b"\x00\x00\x00\x18ftypisom\x00\x00\x02\x00",
        20_000_000,
    );
    let viewed = rig.view("clip.mp4").await;
    assert!(
        viewed
            .refused()
            .starts_with("clip.mp4 is video/mp4, which this server can't carry"),
        "{}",
        viewed.result()
    );
}

#[tokio::test]
async fn a_file_over_the_attachment_limit_is_refused_from_its_size() {
    let rig = Rig::new();
    let mut start = png(2, 2);
    start.truncate(64);
    rig.sized("huge.png", &start, 8_000_000);
    let viewed = rig.view("huge.png").await;
    assert_eq!(
        viewed.refused(),
        "huge.png is 8.0 MB; files over 7.5 MB can't be attached. Make a smaller copy with bash, \
or extract the part you need, and view that."
    );
    let edge = rig.sized("edge.png", &start, wisp_media::MAX_ATTACHMENT_BYTES as u64);
    let viewed = rig.view(&edge.display().to_string()).await;
    assert_eq!(attached(&viewed).1.len(), wisp_media::MAX_ATTACHMENT_BYTES);
}

#[tokio::test]
async fn an_image_over_the_edge_is_refused_with_a_resize_command() {
    let rig = Rig::new();
    rig.file("shots/wide shot.png", &png(2_001, 1));
    rig.file("tall.png", &png(1, 2_000));
    let viewed = rig.view("shots/wide shot.png").await;
    assert_eq!(
        viewed.refused(),
        "shots/wide shot.png is 2001\u{d7}1; images over 2,000 px a side can't be attached. \
Resize a copy with bash (for example, magick 'shots/wide shot.png' -resize 2000x2000 \
'shots/wide shot-small.png') and view that."
    );
    let viewed = rig.view("tall.png").await;
    assert_eq!(attached(&viewed).0, "image/png");
}

#[tokio::test]
async fn the_edge_is_the_endpoint_s_and_an_unreadable_header_is_attached() {
    let mut rig = Rig::new();
    let mut broken = png(4, 4);
    broken.truncate(12);
    broken.extend_from_slice(&[0xab; 40]);
    rig.file("broken.png", &broken);
    let viewed = rig.view("broken.png").await;
    assert_eq!(attached(&viewed), ("image/png", broken));
    rig.file("big.png", &png(2_500, 1));
    rig.edge = None;
    let viewed = rig.view("big.png").await;
    assert_eq!(attached(&viewed).0, "image/png");
    rig.edge = Some(10);
    rig.file("doc.pdf", &pdf());
    let viewed = rig.view("doc.pdf").await;
    assert_eq!(attached(&viewed).0, "application/pdf");
}

#[tokio::test]
async fn the_count_sees_only_the_media_block_and_its_estimate_is_shown() {
    let mut rig = Rig::new();
    let (count, asked) = counting(TokenCount::Counted(2_960));
    rig.count = count;
    let bytes = png(5, 5);
    rig.file("a.png", &bytes);
    let viewed = rig.view("a.png").await;
    assert_eq!(
        viewed.completed(),
        format!(
            "~/work/a.png: image/png, {} bytes, attached as is (about 2,960 tokens, the server's \
estimate).",
            bytes.len()
        )
    );
    assert_eq!(viewed.run.facts.last(), Some(&Fact::Tokens(2_960)));
    let asked = asked.lock().unwrap().clone();
    assert_eq!(asked, [("image/png".to_owned(), bytes)]);
}

#[tokio::test]
async fn a_count_over_the_limit_refuses_and_an_unknown_count_attaches() {
    let mut rig = Rig::new();
    rig.file("spec.pdf", &pdf());
    rig.count = counting(TokenCount::Counted(182_000)).0;
    let viewed = rig.view("spec.pdf").await;
    assert_eq!(
        viewed.refused(),
        "spec.pdf would add about 182,000 tokens (the server's estimate), more than one view \
may add. Cut out the part you need with bash (qpdf --pages, pdftotext) and view that."
    );
    rig.count = counting(TokenCount::Counted(MAX_VIEW_TOKENS)).0;
    assert_eq!(attached(&rig.view("spec.pdf").await).0, "application/pdf");
    for answer in [
        TokenCount::Unsupported,
        TokenCount::Failed("HTTP 500".to_owned()),
    ] {
        rig.count = counting(answer).0;
        let viewed = rig.view("spec.pdf").await;
        assert!(
            viewed.completed().ends_with(", attached as is."),
            "{}",
            viewed.result()
        );
        assert_eq!(viewed.run.facts.len(), 2);
    }
}

#[tokio::test]
async fn a_stop_during_the_count_attaches_nothing() {
    let mut rig = Rig::new();
    rig.file("a.png", &png(2, 2));
    let cancel = rig.cancel.clone();
    rig.count = Some(Arc::new(move |_, _| {
        cancel.set();
        Box::pin(std::future::pending())
    }));
    let viewed = rig.view("a.png").await;
    assert_eq!(viewed.run.outcome, Outcome::Cancelled);
    assert_eq!(viewed.result(), STOPPED);
    assert!(viewed.run.attachments.is_empty());
}

#[tokio::test(start_paused = true)]
async fn a_count_that_never_answers_attaches_with_the_size_only_before_the_view_s_limit() {
    let mut rig = Rig::new();
    rig.file("a.png", &png(2, 2));
    rig.count = Some(Arc::new(|_, _| Box::pin(std::future::pending())));
    let started = tokio::time::Instant::now();
    let viewed = rig.view("a.png").await;
    let waited = started.elapsed();
    assert!(
        viewed.completed().ends_with(", attached as is."),
        "{}",
        viewed.result()
    );
    assert_eq!(attached(&viewed).0, "image/png");
    assert_eq!(viewed.run.facts.len(), 2);
    assert!(
        waited >= Duration::from_millis(TOKEN_COUNT_LIMIT_MS),
        "{waited:?}"
    );
    assert!(
        waited < Duration::from_millis(meta().timeouts.default_ms),
        "{waited:?}"
    );
}
