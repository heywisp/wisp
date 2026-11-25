use wisp_protocol::log::BlockPosition;
use wisp_protocol::{AttachmentKind, ContentBlock, FileSlice, ImageType, Message, Role};

use super::{
    MAX_REASON_CHARS, end_of, file_note, one_line_reason, provisional_media, withdrawal_note,
};
use crate::test_guard::text;
use crate::test_guard::user;

fn image() -> ContentBlock {
    ContentBlock::image(ImageType::Png, "iVBORw0KGgo=")
}

#[test]
fn a_reason_becomes_one_clean_line_of_at_most_200_characters() {
    assert_eq!(
        one_line_reason("Could not\nprocess   image\t\u{7}"),
        "Could not process image"
    );
    assert_eq!(one_line_reason(" \n "), "the API refused it");
    let long = one_line_reason(&"x".repeat(500));
    assert_eq!(long.chars().count(), MAX_REASON_CHARS);
    assert!(long.ends_with('\u{2026}'));
    assert_eq!(one_line_reason(&"y".repeat(200)), "y".repeat(200));
}

#[test]
fn the_note_names_the_kind_the_label_and_the_reason() {
    assert_eq!(
        withdrawal_note(AttachmentKind::Image, "#2", "Could not process image"),
        "[image #2 couldn't be processed: Could not process image]"
    );
}

#[test]
fn only_media_past_the_accepted_position_is_provisional() {
    let messages = vec![
        user(vec![text("look"), image()]),
        Message {
            role: Role::Assistant,
            content: vec![text("ok")],
        },
        user(vec![text("and this")]),
    ];
    assert!(provisional_media(
        &messages,
        BlockPosition {
            message: 0,
            block: 0
        }
    ));
    assert!(!provisional_media(
        &messages,
        BlockPosition {
            message: 0,
            block: 2
        }
    ));
    assert!(!provisional_media(&messages, end_of(&messages)));
    let mut merged = messages.clone();
    merged[2].content.push(image());
    assert!(provisional_media(&merged, end_of(&messages)));
    let in_result = vec![user(vec![ContentBlock::ToolResult {
        tool_use_id: "toolu_1".into(),
        content: vec![text("shot"), image()],
        is_error: false,
    }])];
    assert!(provisional_media(
        &in_result,
        BlockPosition {
            message: 0,
            block: 0
        }
    ));
}

#[test]
fn images_are_numbered_per_input_even_without_text() {
    let mut blocks = vec![
        text("Sent: now"),
        image(),
        image(),
        text(""),
        image(),
        text(""),
        image(),
        text("look"),
    ];
    let mut slots = Vec::new();
    wisp_protocol::log::media_slots(&mut blocks, &mut slots);
    let numbers: Vec<usize> = slots.iter().map(|s| s.number).collect();
    assert_eq!(numbers, [1, 2, 1, 1]);
}

#[test]
fn a_file_note_hints_at_bash_alone_and_names_the_others_withdrawn_with_it() {
    let pdf = "application/pdf".parse().unwrap();
    assert_eq!(
        file_note(
            "docs/spec.pdf",
            &pdf,
            None,
            "The PDF is password protected.",
            0,
            Some("bash")
        ),
        "[docs/spec.pdf (application/pdf) couldn't be processed: The PDF is password protected. \
Convert or cut it with bash into something you can read, then view or read that.]"
    );
    assert_eq!(
        file_note("a.pdf", &pdf, None, "Bad request", 1, Some("bash")),
        "[a.pdf (application/pdf) was withdrawn with 1 other attachment when the API refused this \
step: Bad request. It may be fine: view it again on its own, and convert it with bash only if \
that is refused too.]"
    );
    let slice = Some(FileSlice::pages(99, 99).unwrap());
    for (shell, advice) in [
        (Some("bash"), "convert or cut it with bash and view that"),
        (None, "omit the slice to read the whole file"),
    ] {
        assert_eq!(
            file_note("docs/manual.pdf", &pdf, slice, "Bad request.", 0, shell),
            format!(
                "[docs/manual.pdf (application/pdf, page 99) couldn't be processed: Bad request. Check the requested range and try again, or {advice}.]"
            )
        );
    }
    assert_eq!(
        file_note("a.pdf", &pdf, slice, "Bad request", 1, None),
        "[a.pdf (application/pdf, page 99) was withdrawn with 1 other attachment when the API refused this step: Bad request. It may be fine: view it again on its own.]"
    );
    assert!(file_note("a.pdf", &pdf, None, "Bad request..", 3, Some("bash")).contains(
        "was withdrawn with 3 other attachments when the API refused this step: Bad request.. It"
    ));
    assert!(
        file_note("a.pdf", &pdf, None, "Bad request", 0, Some("sh"))
            .contains("Convert or cut it with sh into")
    );
    assert!(
        file_note("a.pdf", &pdf, None, "Bad request", 2, Some("sh"))
            .contains("convert it with sh only if")
    );
}

#[test]
fn without_bash_a_file_note_never_names_bash() {
    let pdf = "application/pdf".parse().unwrap();
    assert_eq!(
        file_note(
            "docs/spec.pdf",
            &pdf,
            None,
            "The PDF is password protected.",
            0,
            None
        ),
        "[docs/spec.pdf (application/pdf) couldn't be processed: The PDF is password protected. \
It can't be read in this session.]"
    );
    assert_eq!(
        file_note("a.pdf", &pdf, None, "Bad request", 1, None),
        "[a.pdf (application/pdf) was withdrawn with 1 other attachment when the API refused this \
step: Bad request. It may be fine: view it again on its own.]"
    );
}

#[test]
fn a_file_in_a_tool_result_is_provisional_like_an_image() {
    let file = ContentBlock::media("application/pdf".parse().unwrap(), "JVBERi0=");
    let messages = vec![
        user(vec![text("go")]),
        Message {
            role: Role::Assistant,
            content: vec![text("calling")],
        },
        user(vec![ContentBlock::ToolResult {
            tool_use_id: "a".into(),
            content: vec![text("spec.pdf"), file],
            is_error: false,
        }]),
    ];
    let before = BlockPosition {
        message: 1,
        block: 0,
    };
    assert!(provisional_media(&messages, before));
    assert!(!provisional_media(&messages, end_of(&messages)));
}
