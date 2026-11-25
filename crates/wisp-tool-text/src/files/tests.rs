use super::*;

#[test]
fn carries_matches_exact_types_and_subtype_prefixes() {
    let carried = [
        "image/png",
        "audio/*",
        "application/vnd.oasis.opendocument.*",
    ];
    for media in [
        "image/png",
        "audio/wav",
        "application/vnd.oasis.opendocument.text",
    ] {
        assert!(carries(&carried, media), "{media}");
    }
    for media in [
        "image/pngx",
        "audioish/wav",
        "application/vnd.oasis.opendocumentx.text",
    ] {
        assert!(!carries(&carried, media), "{media}");
    }
}

#[test]
fn megabytes_round_up_to_a_tenth() {
    assert_eq!(megabytes_up(1), "0.1 MB");
    assert_eq!(megabytes_up(100_000), "0.1 MB");
    assert_eq!(megabytes_up(100_001), "0.2 MB");
    assert_eq!(megabytes_up(20_000_000), "20.0 MB");
}

#[test]
fn the_sentences_name_the_shell_tool_only_when_it_is_there() {
    assert_eq!(
        uncarried("a.zip", "application/zip", &[], None),
        "a.zip is application/zip, which this server can't carry (it takes no files), so it can't be read in this session."
    );
    assert!(
        uncarried("a.zip", "application/zip", &["image/png"], Some("bash")).contains("with bash")
    );
    assert!(
        uncarried("a.zip", "application/zip", &["image/png"], Some("sh")).contains("with sh to")
    );
    assert_eq!(
        too_large("big.png", 30_000_000, 20_000_000, "view that", None),
        "big.png is 30.0 MB; files over 20.0 MB can't be attached, so it can't be read in this session."
    );
    assert!(
        too_large("big.png", 30_000_000, 20_000_000, "view that", Some("bash"))
            .ends_with("and view that.")
    );
    assert!(
        too_large("big.png", 30_000_000, 20_000_000, "view that", Some("sh"))
            .contains("a smaller copy with sh,")
    );
}

#[test]
fn parse_pages_seconds_and_sheet_enforce_closed_bounds() {
    assert_eq!(
        parse_pages("5"),
        Ok(FileSlice::Pages(PageSlice { start: 5, end: 5 }))
    );
    assert_eq!(
        parse_pages("14-16"),
        Ok(FileSlice::Pages(PageSlice { start: 14, end: 16 }))
    );
    assert_eq!(
        parse_seconds("0-30"),
        Ok(FileSlice::Seconds(SecondSlice { start: 0, end: 30 }))
    );
    assert_eq!(
        parse_seconds("120-210"),
        Ok(FileSlice::Seconds(SecondSlice {
            start: 120,
            end: 210
        }))
    );
    assert_eq!(parse_sheet("2"), Ok(FileSlice::Sheet(2)));
    for value in [
        "",
        "0",
        "16-14",
        "14-",
        "-16",
        "1,3",
        "4294967296",
        " 5",
        "+5",
        "５",
    ] {
        assert_eq!(
            parse_pages(value),
            Err("`pages` must be a 1-based page or range like `5` or `14-16`"),
            "{value:?}"
        );
    }
    for value in [
        "30",
        "30-30",
        "30-0",
        "0-",
        "-30",
        "0-1.5",
        "00:00-00:30",
        "0-4294967296",
        "0- 30",
    ] {
        assert_eq!(
            parse_seconds(value),
            Err(
                "`seconds` must be a range in whole seconds like `0-30` or `120-210` with end greater than start"
            ),
            "{value:?}"
        );
    }
    for value in ["0", "-1", "4294967296", " 1", "+1"] {
        assert_eq!(
            parse_sheet(value),
            Err("`sheet` must be a 1-based worksheet number (1 or greater)"),
            "{value:?}"
        );
    }
}

#[test]
fn slice_kind_for_and_slice_supported_cover_all_media_families() {
    for (kind, media) in [
        (SliceKind::Pages, "application/pdf"),
        (
            SliceKind::Pages,
            "application/vnd.openxmlformats-officedocument.wordprocessingml.document",
        ),
        (
            SliceKind::Pages,
            "application/vnd.openxmlformats-officedocument.wordprocessingml.template",
        ),
        (
            SliceKind::Pages,
            "application/vnd.openxmlformats-officedocument.presentationml.presentation",
        ),
        (SliceKind::Pages, "application/vnd.oasis.opendocument.text"),
        (
            SliceKind::Pages,
            "application/vnd.oasis.opendocument.presentation",
        ),
        (
            SliceKind::Sheet,
            "application/vnd.openxmlformats-officedocument.spreadsheetml.sheet",
        ),
        (
            SliceKind::Sheet,
            "application/vnd.oasis.opendocument.spreadsheet",
        ),
        (SliceKind::Seconds, "audio/mpeg"),
        (SliceKind::Seconds, "video/mp4"),
    ] {
        assert_eq!(slice_kind_for(media), Some(kind), "{media}");
        for asked in SliceKind::ALL {
            assert_eq!(
                slice_supported(media, *asked, &[media], &[*asked]),
                *asked == kind,
                "{media}"
            );
        }
        assert!(!slice_supported(media, kind, &[], &[kind]), "{media}");
        assert!(!slice_supported(media, kind, &[media], &[]), "{media}");
    }
    assert!(slice_supported(
        "audio/mpeg",
        SliceKind::Seconds,
        &["audio/*"],
        &[SliceKind::Seconds]
    ));
    for media in [
        "audio/",
        "video/",
        "image/png",
        "application/msword",
        "application/vnd.ms-excel",
        "application/vnd.ms-powerpoint",
        "text/plain",
        "application/zip",
    ] {
        assert_eq!(slice_kind_for(media), None, "{media}");
    }
}

#[test]
fn slice_refusal_sentences_vary_cleanly_with_and_without_shell() {
    use SliceKind::{Pages, Seconds, Sheet};

    assert_eq!(
        text_with_slice("notes.txt", Pages, Some("bash")),
        "No file was sent: notes.txt is a text file, so `pages` does not apply. Omit `pages` to read the start, or read a line range with bash (sed -n)."
    );
    assert_eq!(
        text_with_slice("notes.txt", Pages, None),
        "No file was sent: notes.txt is a text file, so `pages` does not apply. Omit `pages` to read the start."
    );
    assert_eq!(
        text_with_slice("notes.txt", Pages, Some("sh")),
        "No file was sent: notes.txt is a text file, so `pages` does not apply. Omit `pages` to read the start, or read a line range with sh (sed -n)."
    );
    for shell in [None, Some("bash")] {
        assert_eq!(
            slice_refusal(
                "budget.xlsx",
                "application/vnd.openxmlformats-officedocument.spreadsheetml.sheet",
                Pages,
                &[Pages, Sheet],
                shell
            )
            .as_deref(),
            Some(
                "No file was sent: budget.xlsx is application/vnd.openxmlformats-officedocument.spreadsheetml.sheet, which takes `sheet`, not `pages`."
            )
        );
        assert_eq!(
            slice_refusal("call.mp3", "audio/mpeg", Seconds, &[Seconds], shell),
            None
        );
    }
    assert_eq!(
        slice_refusal("shot.png", "image/png", Pages, &[], Some("bash")).as_deref(),
        Some(
            "No file was sent: shot.png is image/png, which can't be sliced with `pages`. Cut out only the part you need with bash and view that copy, or omit `pages` to send and store the whole file."
        )
    );
    assert_eq!(
        slice_refusal("shot.png", "image/png", Pages, &[], None).as_deref(),
        Some(
            "No file was sent: shot.png is image/png, which can't be sliced with `pages`. Omit `pages` to send and store the whole file."
        )
    );
    assert_eq!(
        slice_refusal(
            "call.mp3",
            "audio/mpeg",
            Seconds,
            &[Pages, Sheet],
            Some("bash")
        )
        .as_deref(),
        Some(
            "No file was sent: call.mp3 can't be sliced with `seconds` on this model (it takes pages, sheet). Cut out only the part you need with bash and view that copy, or omit `seconds` to send and store the whole file."
        )
    );
    assert_eq!(
        slice_refusal("call.mp3", "audio/mpeg", Seconds, &[Pages, Sheet], None).as_deref(),
        Some(
            "No file was sent: call.mp3 can't be sliced with `seconds` on this model (it takes pages, sheet). Omit `seconds` to read the whole file, or switch to a model that supports `seconds`; either retry sends and stores the whole file."
        )
    );
    assert_eq!(
        slice_refusal("call.mp3", "audio/mpeg", Seconds, &[], None).as_deref(),
        Some(
            "No file was sent: call.mp3 can't be sliced with `seconds` on this model (this model does not slice files). Omit `seconds` to read the whole file, or switch to a model that supports `seconds`; either retry sends and stores the whole file."
        )
    );
}
