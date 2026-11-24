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
