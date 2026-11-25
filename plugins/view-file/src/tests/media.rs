use wisp_plugin_api::types::{PageSlice, SecondSlice};

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
    let mut rig = Rig::new();
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
    rig.media_slices = SliceKind::ALL.to_vec();
    let (count, asked) = counting(TokenCount::Counted(1_680));
    rig.count = count;
    for (name, media_type, args, slice, read) in [
        (
            "doc.pdf",
            "application/pdf",
            json!({"path": "doc.pdf", "pages": "14-16"}),
            FileSlice::Pages(PageSlice { start: 14, end: 16 }),
            "pages 14-16",
        ),
        (
            "sheet.xlsx",
            "application/vnd.openxmlformats-officedocument.spreadsheetml.sheet",
            json!({"path": "sheet.xlsx", "sheet": 2}),
            FileSlice::Sheet(2),
            "sheet 2",
        ),
    ] {
        let bytes = fs::read(rig.cwd.join(name)).unwrap();
        let viewed = rig.view_with(&rig.cwd, args).await;
        assert_eq!(attached(&viewed), (media_type, bytes.clone()));
        assert_eq!(viewed.run.attachments[0].slice, Some(slice));
        assert_eq!(
            viewed.completed(),
            format!(
                "~/work/{name}: {media_type}, {}, attached whole (read only {read}; about 1,680 tokens, the server's estimate).",
                words::size(bytes.len() as u64)
            )
        );
        assert_eq!(
            asked.lock().unwrap().pop(),
            Some((media_type.to_owned(), bytes, Some(slice)))
        );
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
    let bom = [b"\xEF\xBB\xBF".as_slice(), &bytes].concat();
    rig.file("with-bom.pdf", &bom);
    assert_eq!(
        attached(&rig.view("with-bom.pdf").await),
        ("application/pdf", bom)
    );
}

const MP4: &[u8] = b"\x00\x00\x00\x18ftypisom\x00\x00\x02\x00";

#[tokio::test]
async fn carried_audio_and_video_families_attach_under_normalized_names() {
    let mut rig = Rig::new();
    rig.media_types = &["audio/*", "video/*"];
    rig.media_slices = vec![SliceKind::Seconds];
    let (count, asked) = counting(TokenCount::Unsupported);
    rig.count = count;
    let slice = FileSlice::Seconds(SecondSlice { start: 30, end: 90 });
    for (name, bytes, media_type) in [
        (
            "beep.wav",
            b"RIFF\x00\x00\x00\x00WAVEfmt " as &[u8],
            "audio/wav",
        ),
        ("clip.mp4", MP4, "video/mp4"),
    ] {
        rig.file(name, bytes);
        let viewed = rig.view(name).await;
        assert_eq!(attached(&viewed), (media_type, bytes.to_vec()), "{name}");
        assert_eq!(
            viewed.run.facts,
            [
                Fact::Media(media_type.to_owned()),
                Fact::Bytes(bytes.len() as u64)
            ]
        );
        assert_eq!(
            asked.lock().unwrap().pop(),
            Some((media_type.to_owned(), bytes.to_vec(), None))
        );
        let viewed = rig
            .view_with(&rig.cwd, json!({"path": name, "seconds": "30-90"}))
            .await;
        assert_eq!(attached(&viewed), (media_type, bytes.to_vec()));
        assert_eq!(viewed.run.attachments[0].slice, Some(slice));
        assert_eq!(
            viewed.completed(),
            format!(
                "~/work/{name}: {media_type}, {}, attached whole (read only seconds 30-90).",
                words::size(bytes.len() as u64)
            )
        );
        assert_eq!(
            asked.lock().unwrap().pop(),
            Some((media_type.to_owned(), bytes.to_vec(), Some(slice)))
        );
    }
}

#[tokio::test]
async fn text_wins_over_sniffed_types_but_ole_is_never_attached() {
    let mut rig = Rig::new();
    rig.media_types = &["application/msword", "application/pdf", "audio/*"];
    for (name, text) in [
        ("addresses.txt", "MAC addresses can change"),
        ("tags.txt", "ID3 tags describe audio"),
        ("source.rs", "fn main() {}\nconst HEADER: &str = \"%PDF-\";"),
    ] {
        rig.file(name, text.as_bytes());
        let viewed = rig.view(name).await;
        viewed.completed();
        assert!(viewed.run.attachments.is_empty(), "{name}");
        assert!(viewed.captured.bytes > 0, "{name}");
    }
    rig.file("legacy.xls", b"\xD0\xCF\x11\xE0\xA1\xB1\x1A\xE1");
    let viewed = rig.view("legacy.xls").await;
    assert!(
        viewed
            .refused()
            .starts_with(&format!("legacy.xls is {}", look::OLE_FILE))
    );
}

#[tokio::test]
async fn slices_are_refused_before_reading_or_counting_and_bad_inputs_fail_parse() {
    let mut rig = Rig::new();
    let (count, asked) = counting(TokenCount::Counted(1));
    rig.count = count;
    rig.sized(
        "large.pdf",
        &pdf(),
        wisp_media::MAX_ATTACHMENT_BYTES as u64 + 1,
    );
    rig.file("plain.txt", b"ID3 tags describe audio");
    rig.file("image.png", &png(3, 2));
    rig.file("legacy.xls", b"\xD0\xCF\x11\xE0\xA1\xB1\x1A\xE1");
    for bash in [true, false] {
        rig.bash = bash;
        let big = rig
            .view_with(&rig.cwd, json!({"path":"large.pdf", "pages":"1-2"}))
            .await;
        let refusal = big.refused();
        assert!(refusal.starts_with("No file was sent: large.pdf can't be sliced with `pages` on this model (this model does not slice files)."), "{refusal}");
        assert_eq!(refusal.contains("with bash"), bash, "{refusal}");
        assert!(
            refusal.contains("send") && refusal.contains("store"),
            "{refusal}"
        );
        let text = rig
            .view_with(&rig.cwd, json!({"path":"plain.txt", "seconds":"0-10"}))
            .await;
        assert!(text.refused().starts_with(
            "No file was sent: plain.txt is a text file, so `seconds` does not apply."
        ));
        assert_eq!(text.refused().contains("with bash (sed -n)"), bash);
        assert_eq!(text.captured.bytes, 0);
    }
    for (args, prefix) in [
        (
            json!({"path":"image.png", "pages":"1"}),
            "No file was sent: image.png is image/png, which can't be sliced with `pages`.",
        ),
        (
            json!({"path":"large.pdf", "sheet":1}),
            "No file was sent: large.pdf is application/pdf, which takes `pages`, not `sheet`.",
        ),
        (
            json!({"path":"legacy.xls", "sheet":1}),
            "legacy.xls is an OLE compound file",
        ),
    ] {
        let viewed = rig.view_with(&rig.cwd, args).await;
        assert!(viewed.refused().starts_with(prefix), "{}", viewed.result());
    }
    assert!(asked.lock().unwrap().is_empty());

    for (key, values, error) in [
        (
            "pages",
            vec![json!("1,3"), json!(0)],
            "`pages` must be a 1-based page or range like `5` or `14-16`",
        ),
        (
            "seconds",
            vec![json!("90-30"), json!(true)],
            "`seconds` must be a range in whole seconds like `0-30` or `120-210` with end greater than start",
        ),
        (
            "sheet",
            vec![
                json!("2"),
                json!(2.0),
                json!(1.5),
                json!(0),
                json!(-1),
                json!(false),
                json!(u64::MAX),
            ],
            "`sheet` must be a 1-based worksheet number (1 or greater)",
        ),
    ] {
        for value in values {
            let input = json!({"path": "file", (key): value}).to_string();
            assert_eq!(parse(&input).err().as_deref(), Some(error), "{input}");
        }
    }
    assert_eq!(
        parse(r#"{"path":"file","pages":"1","sheet":1}"#)
            .err()
            .as_deref(),
        Some("give at most one of `pages`, `seconds` or `sheet`")
    );
    assert_eq!(
        parse(r#"{"path":"file","unknown":null}"#).err().as_deref(),
        Some("unknown field `unknown`")
    );
    assert_eq!(
        parse(r#"{"path":"file","pages":null,"seconds":null,"sheet":null}"#)
            .unwrap()
            .slice,
        None
    );
    assert_eq!(
        parse(r#"{"path":"file","pages":null,"seconds":null,"sheet":2}"#)
            .unwrap()
            .slice,
        Some(FileSlice::Sheet(2))
    );
}

#[tokio::test]
async fn a_carried_video_over_the_size_limit_is_refused_by_size() {
    let mut rig = Rig::new();
    rig.media_types = &["video/*"];
    rig.sized("clip.mp4", MP4, 20_000_000);
    assert!(
        rig.view("clip.mp4")
            .await
            .refused()
            .starts_with("clip.mp4 is 20.0 MB; files over 7.5 MB can't be attached.")
    );
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
    rig.sized("clip.mp4", MP4, 20_000_000);
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
    assert_eq!(asked, [("image/png".to_owned(), bytes, None)]);
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
    rig.count = Some(Arc::new(move |_, _, _| {
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
    rig.count = Some(Arc::new(|_, _, _| Box::pin(std::future::pending())));
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
