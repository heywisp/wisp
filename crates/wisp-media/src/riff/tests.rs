use super::*;

fn chunk(kind: &[u8; 4], size: u32, data: &[u8]) -> Vec<u8> {
    let mut out = kind.to_vec();
    out.extend(size.to_le_bytes());
    out.extend(data);
    out
}

fn riff(declared: u32, body: &[u8]) -> Vec<u8> {
    let mut out = b"RIFF".to_vec();
    out.extend(declared.to_le_bytes());
    out.extend(b"WEBP");
    out.extend(body);
    out
}

fn sized(body: &[u8]) -> Vec<u8> {
    riff(u32::try_from(body.len() + 4).unwrap(), body)
}

#[test]
fn well_formed_chunks_pass_including_odd_sizes_and_their_padding() {
    let mut body = chunk(b"VP8L", 3, &[1, 2, 3, 0]);
    body.extend(chunk(b"EXIF", 2, &[9, 9]));
    assert_eq!(check_chunks(&sized(&body)), Ok(()));
    assert_eq!(check_chunks(&sized(&[])), Ok(()));
}

#[test]
fn a_chunk_declaring_more_than_the_input_holds_is_malformed() {
    let mut body = chunk(b"VP8L", 2, &[1, 2]);
    body.extend(chunk(b"EXIF", 0xffff_fff0, &[0; 4]));
    assert_eq!(check_chunks(&sized(&body)), Err(MediaError::Malformed));
    let short = chunk(b"VP8L", 9, &[1, 2]);
    assert_eq!(check_chunks(&sized(&short)), Err(MediaError::Malformed));
    let header_cut = b"VP8L\x02".to_vec();
    assert_eq!(
        check_chunks(&sized(&header_cut)),
        Err(MediaError::Malformed)
    );
}

#[test]
fn a_riff_size_past_the_end_or_a_short_header_is_malformed() {
    let body = chunk(b"VP8L", 2, &[1, 2]);
    assert_eq!(
        check_chunks(&riff(0xffff_fff0, &body)),
        Err(MediaError::Malformed)
    );
    assert_eq!(
        check_chunks(&riff(u32::MAX, &body)),
        Err(MediaError::Malformed)
    );
    assert_eq!(check_chunks(b"RIFF\0\0"), Err(MediaError::Malformed));
}

#[test]
fn trailing_bytes_after_the_riff_are_ignored() {
    let mut file = sized(&chunk(b"VP8L", 2, &[1, 2]));
    file.extend([0xff; 16]);
    assert_eq!(check_chunks(&file), Ok(()));
}

fn animated_with_nested(kind: &[u8; 4], declared: u32) -> Vec<u8> {
    let mut vp8x_data = vec![0x02, 0, 0, 0];
    vp8x_data.extend([1, 0, 0, 1, 0, 0]);
    let mut body = chunk(b"VP8X", 10, &vp8x_data);
    body.extend(chunk(b"ANIM", 6, &[0; 6]));
    let mut frame = vec![0; 16];
    frame.extend(kind);
    frame.extend(declared.to_le_bytes());
    body.extend(chunk(b"ANMF", 24, &frame));
    sized(&body)
}

#[test]
fn the_76_byte_nested_exif_bomb_and_its_icc_and_xmp_twins_are_malformed() {
    let bomb = animated_with_nested(b"EXIF", 0xffff_fff0);
    assert_eq!(bomb.len(), 76);
    for kind in [b"EXIF", b"ICCP", b"XMP "] {
        let file = animated_with_nested(kind, 0xffff_fff0);
        assert_eq!(check_chunks(&file), Err(MediaError::Malformed), "{kind:?}");
        let just_over = animated_with_nested(kind, 1);
        assert_eq!(
            check_chunks(&just_over),
            Err(MediaError::Malformed),
            "{kind:?}"
        );
    }
}

#[test]
fn frames_whose_subchunks_fit_pass_and_short_frames_are_malformed() {
    let fits = animated_with_nested(b"EXIF", 0);
    assert_eq!(check_chunks(&fits), Ok(()));
    let mut frame = vec![0; 16];
    frame.extend(chunk(b"VP8L", 3, &[1, 2, 3, 0]));
    frame.extend(chunk(b"ALPH", 2, &[1, 2]));
    let size = u32::try_from(frame.len()).unwrap();
    assert_eq!(check_chunks(&sized(&chunk(b"ANMF", size, &frame))), Ok(()));
    assert_eq!(
        check_chunks(&sized(&chunk(b"ANMF", 8, &[0; 8]))),
        Err(MediaError::Malformed)
    );
    let mut cut = vec![0; 16];
    cut.extend(b"VP8L\x02");
    assert_eq!(
        check_chunks(&sized(&chunk(b"ANMF", 21, &[cut, vec![0]].concat()))),
        Err(MediaError::Malformed)
    );
}
