use super::*;

#[test]
fn orientations_that_swap_axes_are_the_four_transposing_ones() {
    let swapping: Vec<Orientation> = [
        Orientation::NoTransforms,
        Orientation::Rotate90,
        Orientation::Rotate180,
        Orientation::Rotate270,
        Orientation::FlipHorizontal,
        Orientation::FlipVertical,
        Orientation::Rotate90FlipH,
        Orientation::Rotate270FlipH,
    ]
    .into_iter()
    .filter(|o| swaps_axes(*o))
    .collect();
    assert_eq!(
        swapping,
        [
            Orientation::Rotate90,
            Orientation::Rotate270,
            Orientation::Rotate90FlipH,
            Orientation::Rotate270FlipH
        ]
    );
}

#[test]
fn media_types_name_their_wire_strings_and_extensions() {
    assert_eq!(MediaType::Png.as_str(), "image/png");
    assert_eq!(MediaType::Jpeg.as_str(), "image/jpeg");
    assert_eq!(MediaType::Png.extension(), "png");
    assert_eq!(MediaType::Jpeg.extension(), "jpg");
}

#[test]
fn the_pre_shrink_keeps_the_exact_target() {
    let image = DynamicImage::new_rgb8(4000, 100);
    let out = scaled(image, 1000, 25);
    assert_eq!((out.width(), out.height()), (1000, 25));
}

#[test]
fn the_cap_is_measured_in_base64_not_raw_bytes() {
    assert!(fits_cap(7_500_000));
    assert!(!fits_cap(7_500_001));
    assert!(!fits_cap(MAX_IMAGE_BASE64));
}

#[test]
fn fallback_steps_shrink_by_a_quarter_down_to_the_floor() {
    assert_eq!(next_step(2000), Some(1500));
    assert_eq!(next_step(1001), Some(750));
    assert_eq!(next_step(342), Some(256));
    assert_eq!(next_step(341), None);
    assert_eq!(next_step(0), None);

    let mut edge = 2000;
    let mut steps = vec![];
    while let Some(next) = next_step(edge) {
        steps.push(next);
        edge = next;
    }
    assert_eq!(steps, [1500, 1125, 843, 632, 474, 355, 266]);
}

#[test]
fn only_translucent_pixels_count_as_real_alpha() {
    let opaque = DynamicImage::ImageRgba8(image::RgbaImage::from_pixel(
        4,
        4,
        image::Rgba([1, 2, 3, 255]),
    ));
    assert!(!has_real_alpha(&opaque));
    let mut translucent = opaque.to_rgba8();
    translucent.put_pixel(3, 3, image::Rgba([1, 2, 3, 254]));
    assert!(has_real_alpha(&DynamicImage::ImageRgba8(translucent)));
    assert!(!has_real_alpha(&DynamicImage::new_rgb8(4, 4)));
}

fn nested_bomb(kind: &[u8; 4]) -> Vec<u8> {
    let mut body = b"VP8X".to_vec();
    body.extend(10u32.to_le_bytes());
    body.extend([0x02, 0, 0, 0, 1, 0, 0, 1, 0, 0]);
    body.extend(b"ANIM");
    body.extend(6u32.to_le_bytes());
    body.extend([0; 6]);
    body.extend(b"ANMF");
    body.extend(24u32.to_le_bytes());
    body.extend([0; 16]);
    body.extend(kind);
    body.extend(0xffff_fff0_u32.to_le_bytes());
    let mut file = b"RIFF".to_vec();
    file.extend(u32::try_from(body.len() + 4).unwrap().to_le_bytes());
    file.extend(b"WEBP");
    file.extend(body);
    file
}

#[test]
fn the_webp_decoder_alone_bounds_a_nested_exif_chunk() {
    let bomb = nested_bomb(b"EXIF");
    assert_eq!(bomb.len(), 76);
    assert!(matches!(decode_webp(&bomb), Err(MediaError::TooBig)));
    for kind in [b"ICCP", b"XMP "] {
        assert!(decode_webp(&nested_bomb(kind)).is_err(), "{kind:?}");
    }
}

#[test]
fn animated_canvases_are_bounded_to_a_third_of_the_decode_cap() {
    assert_eq!(MAX_ANIMATED_CANVAS, 85_333_333);
    assert!(canvas_bytes(4619, 4618) <= MAX_ANIMATED_CANVAS);
    assert!(canvas_bytes(4620, 4618) > MAX_ANIMATED_CANVAS);
    assert_eq!(canvas_bytes(u32::MAX, u32::MAX), u64::MAX);
}

#[test]
fn dimensions_are_read_without_decoding_the_pixels() {
    for (format, image) in [
        (image::ImageFormat::Png, DynamicImage::new_rgb8(300, 120)),
        (image::ImageFormat::Jpeg, DynamicImage::new_rgb8(300, 120)),
    ] {
        let mut bytes = Vec::new();
        image
            .write_to(&mut Cursor::new(&mut bytes), format)
            .unwrap();
        assert_eq!(dimensions(&bytes), Ok((300, 120)), "{format:?}");
    }
    assert_eq!(dimensions(b"not an image"), Err(MediaError::Unsupported));
    assert!(dimensions(b"\x89PNG\r\n\x1a\n").is_err());
}
