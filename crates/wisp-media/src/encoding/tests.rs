use super::*;

#[test]
fn base64_is_standard_and_padded_and_round_trips() {
    assert_eq!(encode_base64(b""), "");
    assert_eq!(encode_base64(b"f"), "Zg==");
    assert_eq!(encode_base64(b"fo"), "Zm8=");
    assert_eq!(encode_base64(&[0xfb, 0xff]), "+/8=");
    let bytes: Vec<u8> = (0..=255).collect();
    assert_eq!(decode_base64(&encode_base64(&bytes)), Ok(bytes));
}

#[test]
fn the_length_formula_matches_the_encoder() {
    for n in 0..64 {
        assert_eq!(base64_len(n), encode_base64(&vec![7; n]).len(), "{n}");
    }
    assert_eq!(base64_len(usize::MAX), usize::MAX);
}

#[test]
fn bad_base64_is_refused() {
    for text in ["Zg", "Z===", "-_8=", "Zm8=\n", "@@@@"] {
        assert_eq!(decode_base64(text), Err(MediaError::Base64), "{text}");
    }
    let oversized = "A".repeat(MAX_BASE64_INPUT + 4);
    assert_eq!(decode_base64(&oversized), Err(MediaError::SourceTooLarge));
}
