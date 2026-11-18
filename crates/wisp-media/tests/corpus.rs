#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    reason = "corpus builders fail the test when an encoder fails"
)]

use std::io::Cursor;
use std::process::Command;

use image::codecs::bmp::BmpEncoder;
use image::codecs::gif::GifEncoder;
use image::codecs::jpeg::JpegEncoder;
use image::codecs::png::{CompressionType, FilterType as PngFilter, PngEncoder};
use image::codecs::webp::WebPEncoder;
use image::{
    ColorType, DynamicImage, ExtendedColorType, Frame, GenericImageView, ImageEncoder, ImageFormat,
    ImageReader, Rgb, RgbImage, Rgba, RgbaImage,
};
use rustix::process::{Resource, Rlimit, setrlimit};
use wisp_media::{
    MAX_ANIMATED_CANVAS, MAX_IMAGE_BASE64, MAX_LONG_EDGE, MAX_SOURCE_BYTES, MIN_FALLBACK_EDGE,
    MediaError, MediaType, Normalised, Tier, base64_len, normalise, panic_is_contained, target,
};

const RED: [u8; 3] = [230, 20, 20];
const BLUE: [u8; 3] = [20, 20, 230];

fn halves(width: u32, height: u32) -> RgbImage {
    RgbImage::from_fn(width, height, |x, _| {
        if x < width / 2 { Rgb(RED) } else { Rgb(BLUE) }
    })
}

fn pattern(width: u32, height: u32) -> RgbaImage {
    RgbaImage::from_fn(width, height, |x, y| {
        Rgba([
            u8::try_from((x * 7) % 256).unwrap(),
            u8::try_from((y * 5) % 256).unwrap(),
            u8::try_from((x + y) % 256).unwrap(),
            u8::try_from(128 + (x % 2) * 127).unwrap(),
        ])
    })
}

fn png_of(raw: &[u8], width: u32, height: u32, colour: ExtendedColorType) -> Vec<u8> {
    let mut out = Vec::new();
    PngEncoder::new_with_quality(&mut out, CompressionType::Fast, PngFilter::NoFilter)
        .write_image(raw, width, height, colour)
        .unwrap();
    out
}

fn png() -> Vec<u8> {
    let image = pattern(64, 48);
    png_of(image.as_raw(), 64, 48, ExtendedColorType::Rgba8)
}

fn jpeg_of(image: &RgbImage) -> Vec<u8> {
    let mut out = Vec::new();
    JpegEncoder::new_with_quality(&mut out, 95)
        .write_image(
            image.as_raw(),
            image.width(),
            image.height(),
            ExtendedColorType::Rgb8,
        )
        .unwrap();
    out
}

fn gif_animated() -> Vec<u8> {
    let mut out = Vec::new();
    {
        let mut encoder = GifEncoder::new(&mut out);
        let red = RgbaImage::from_pixel(24, 16, Rgba([RED[0], RED[1], RED[2], 255]));
        let blue = RgbaImage::from_pixel(24, 16, Rgba([BLUE[0], BLUE[1], BLUE[2], 255]));
        encoder
            .encode_frames([Frame::new(red), Frame::new(blue)])
            .unwrap();
    }
    out
}

fn webp() -> Vec<u8> {
    let image = pattern(40, 30);
    let mut out = Vec::new();
    WebPEncoder::new_lossless(&mut out)
        .write_image(image.as_raw(), 40, 30, ExtendedColorType::Rgba8)
        .unwrap();
    out
}

fn bmp() -> Vec<u8> {
    let image = halves(30, 20);
    let mut out = Vec::new();
    BmpEncoder::new(&mut out)
        .write_image(image.as_raw(), 30, 20, ExtendedColorType::Rgb8)
        .unwrap();
    out
}

fn crc32(bytes: &[u8]) -> u32 {
    let mut crc = 0xffff_ffff_u32;
    for byte in bytes {
        crc ^= u32::from(*byte);
        for _ in 0..8 {
            let mask = (crc & 1).wrapping_neg();
            crc = (crc >> 1) ^ (0xedb8_8320 & mask);
        }
    }
    !crc
}

fn png_chunk(kind: &[u8; 4], data: &[u8]) -> Vec<u8> {
    let mut chunk = u32::try_from(data.len()).unwrap().to_be_bytes().to_vec();
    chunk.extend(kind);
    chunk.extend(data);
    let mut covered = kind.to_vec();
    covered.extend(data);
    chunk.extend(crc32(&covered).to_be_bytes());
    chunk
}

fn insert_after_ihdr(png: &[u8], chunk: &[u8]) -> Vec<u8> {
    let at = 8 + 25;
    let mut out = png[..at].to_vec();
    out.extend(chunk);
    out.extend(&png[at..]);
    out
}

fn with_ihdr_size(png: &[u8], width: u32, height: u32) -> Vec<u8> {
    let mut out = png.to_vec();
    out[16..20].copy_from_slice(&width.to_be_bytes());
    out[20..24].copy_from_slice(&height.to_be_bytes());
    let crc = crc32(&out[12..29]);
    out[29..33].copy_from_slice(&crc.to_be_bytes());
    out
}

fn tiff_exif(orientation: u16) -> Vec<u8> {
    let mut t = b"MM\0\x2a".to_vec();
    t.extend(8u32.to_be_bytes());
    t.extend(2u16.to_be_bytes());
    t.extend([0x01, 0x12, 0x00, 0x03]);
    t.extend(1u32.to_be_bytes());
    t.extend(orientation.to_be_bytes());
    t.extend([0, 0]);
    t.extend([0x88, 0x25, 0x00, 0x04]);
    t.extend(1u32.to_be_bytes());
    t.extend(38u32.to_be_bytes());
    t.extend(0u32.to_be_bytes());
    t.extend(2u16.to_be_bytes());
    t.extend([0x00, 0x01, 0x00, 0x02]);
    t.extend(2u32.to_be_bytes());
    t.extend(*b"N\0\0\0");
    t.extend([0x00, 0x02, 0x00, 0x05]);
    t.extend(3u32.to_be_bytes());
    t.extend(68u32.to_be_bytes());
    t.extend(0u32.to_be_bytes());
    for (numerator, denominator) in [(1u32, 1u32), (2, 1), (3, 1)] {
        t.extend(numerator.to_be_bytes());
        t.extend(denominator.to_be_bytes());
    }
    assert_eq!(t.len(), 92);
    t
}

fn jpeg_with_exif(image: &RgbImage, orientation: u16) -> Vec<u8> {
    let jpeg = jpeg_of(image);
    let mut payload = b"Exif\0\0".to_vec();
    payload.extend(tiff_exif(orientation));
    let mut out = jpeg[..2].to_vec();
    out.extend([0xff, 0xe1]);
    out.extend(u16::try_from(payload.len() + 2).unwrap().to_be_bytes());
    out.extend(payload);
    out.extend(&jpeg[2..]);
    out
}

fn decoded(normalised: &Normalised) -> DynamicImage {
    let format = match normalised.media_type {
        MediaType::Png => ImageFormat::Png,
        MediaType::Jpeg => ImageFormat::Jpeg,
    };
    ImageReader::with_format(Cursor::new(&normalised.bytes), format)
        .decode()
        .unwrap()
}

fn close(actual: Rgb<u8>, expected: [u8; 3]) -> bool {
    actual
        .0
        .iter()
        .zip(expected)
        .all(|(a, e)| a.abs_diff(e) <= 40)
}

fn contains(hay: &[u8], needle: &[u8]) -> bool {
    hay.windows(needle.len()).any(|w| w == needle)
}

fn corpus() -> Vec<(&'static str, Vec<u8>)> {
    vec![
        ("png", png()),
        ("jpeg", jpeg_with_exif(&halves(40, 20), 6)),
        ("gif", gif_animated()),
        ("webp", webp()),
        ("bmp", bmp()),
    ]
}

#[test]
fn a_png_round_trips_losslessly() {
    let source = png();
    let out = normalise(&source, Tier::STANDARD).unwrap();
    assert_eq!(out.media_type, MediaType::Png);
    assert_eq!((out.width, out.height), (64, 48));
    let image = decoded(&out);
    assert_eq!(image.color(), ColorType::Rgba8);
    assert_eq!(image.to_rgba8(), pattern(64, 48));
}

#[test]
fn colour_types_are_reduced_to_eight_bits_and_kept_otherwise() {
    let grey: Vec<u8> = (0..32 * 8)
        .map(|i| u8::try_from(i % 256).unwrap())
        .collect();
    let out = normalise(&png_of(&grey, 32, 8, ExtendedColorType::L8), Tier::HIGH).unwrap();
    assert_eq!(decoded(&out).color(), ColorType::L8);
    assert_eq!(decoded(&out).to_luma8().into_raw(), grey);

    let deep: Vec<u8> = (0..16 * 4 * 6)
        .map(|i| u8::try_from(i % 251).unwrap())
        .collect();
    let out = normalise(&png_of(&deep, 16, 4, ExtendedColorType::Rgb16), Tier::HIGH).unwrap();
    assert_eq!(decoded(&out).color(), ColorType::Rgb8);

    let jpeg = jpeg_of(&halves(16, 16));
    let out = normalise(&jpeg, Tier::HIGH).unwrap();
    assert_eq!(out.media_type, MediaType::Jpeg);
    assert_eq!(decoded(&out).color(), ColorType::Rgb8);
}

#[test]
fn jpeg_exif_orientation_is_applied_and_all_metadata_is_stripped() {
    let source = jpeg_with_exif(&halves(40, 20), 6);
    assert!(contains(&source, b"Exif\0\0"));
    let out = normalise(&source, Tier::STANDARD).unwrap();
    assert_eq!(out.media_type, MediaType::Jpeg);
    assert_eq!((out.width, out.height), (20, 40));
    let image = decoded(&out).to_rgb8();
    assert_eq!(image.dimensions(), (20, 40));
    assert!(
        close(*image.get_pixel(10, 5), RED),
        "{:?}",
        image.get_pixel(10, 5)
    );
    assert!(
        close(*image.get_pixel(10, 35), BLUE),
        "{:?}",
        image.get_pixel(10, 35)
    );
    assert!(!contains(&out.bytes, b"Exif"));
    assert!(!contains(&out.bytes, b"MM\0\x2a"));
    assert!(!contains(&out.bytes, &[0xff, 0xe1]));
}

#[test]
fn png_exif_orientation_is_applied_and_text_chunks_are_stripped() {
    let source = png_of(halves(40, 20).as_raw(), 40, 20, ExtendedColorType::Rgb8);
    let source = insert_after_ihdr(&source, &png_chunk(b"eXIf", &tiff_exif(3)));
    let source = insert_after_ihdr(&source, &png_chunk(b"tEXt", b"Author\0Somebody"));
    let out = normalise(&source, Tier::STANDARD).unwrap();
    assert_eq!((out.width, out.height), (40, 20));
    let image = decoded(&out).to_rgb8();
    assert_eq!(*image.get_pixel(5, 10), Rgb(BLUE));
    assert_eq!(*image.get_pixel(35, 10), Rgb(RED));
    for marker in [&b"eXIf"[..], b"tEXt", b"Somebody", b"MM\0\x2a"] {
        assert!(!contains(&out.bytes, marker), "{marker:?}");
    }
}

#[test]
fn an_animated_gif_becomes_a_png_of_its_first_frame() {
    let out = normalise(&gif_animated(), Tier::STANDARD).unwrap();
    assert_eq!(out.media_type, MediaType::Png);
    assert_eq!((out.width, out.height), (24, 16));
    let image = decoded(&out).to_rgb8();
    assert!(image.pixels().all(|p| close(*p, RED)));
}

#[test]
fn webp_and_bmp_become_png() {
    let out = normalise(&webp(), Tier::STANDARD).unwrap();
    assert_eq!(out.media_type, MediaType::Png);
    assert_eq!(decoded(&out).to_rgba8(), pattern(40, 30));

    let out = normalise(&bmp(), Tier::STANDARD).unwrap();
    assert_eq!(out.media_type, MediaType::Png);
    assert_eq!(decoded(&out).to_rgb8(), halves(30, 20));
}

#[test]
fn large_images_are_downscaled_to_the_target() {
    let screenshot = RgbImage::from_fn(1920, 1080, |x, y| {
        Rgb([
            u8::try_from(x % 256).unwrap(),
            u8::try_from(y % 256).unwrap(),
            0,
        ])
    });
    let source = png_of(screenshot.as_raw(), 1920, 1080, ExtendedColorType::Rgb8);
    let out = normalise(&source, Tier::STANDARD).unwrap();
    assert_eq!((out.width, out.height), (1456, 819));
    assert_eq!(decoded(&out).dimensions(), (1456, 819));
}

#[test]
fn the_2000_px_cap_applies_on_the_high_resolution_tier() {
    let wide = RgbImage::from_pixel(3000, 60, Rgb(RED));
    let source = png_of(wide.as_raw(), 3000, 60, ExtendedColorType::Rgb8);
    let out = normalise(&source, Tier::HIGH).unwrap();
    let expected = target(3000, 60, Tier::HIGH);
    assert_eq!((out.width, out.height), (expected.width, expected.height));
    assert_eq!((out.width, out.height), (MAX_LONG_EDGE, 40));

    let tall = jpeg_with_exif(&RgbImage::from_pixel(2400, 30, Rgb(BLUE)), 8);
    let out = normalise(&tall, Tier::HIGH).unwrap();
    assert_eq!((out.width, out.height), (25, MAX_LONG_EDGE));
}

#[test]
fn empty_oversized_and_unknown_inputs_are_refused() {
    assert_eq!(normalise(&[], Tier::STANDARD), Err(MediaError::Empty));
    let mut huge = png();
    huge.resize(MAX_SOURCE_BYTES + 1, 0);
    assert_eq!(
        normalise(&huge, Tier::STANDARD),
        Err(MediaError::SourceTooLarge)
    );
    for bytes in [
        &b"<!doctype html><img src=x>"[..],
        b"<svg xmlns='http://www.w3.org/2000/svg'/>",
        b"II*\0\x08\0\0\0",
        b"\0\0\x01\0\x01\0",
        b"RIFF\0\0\0\0WAVEfmt ",
        b"%PDF-1.7",
    ] {
        assert_eq!(
            normalise(bytes, Tier::STANDARD),
            Err(MediaError::Unsupported),
            "{bytes:?}"
        );
    }
}

#[test]
fn the_format_follows_the_bytes_not_the_name() {
    let jpeg = jpeg_of(&halves(16, 16));
    assert_eq!(
        normalise(&jpeg, Tier::STANDARD).unwrap().media_type,
        MediaType::Jpeg
    );
    let gif = gif_animated();
    assert_eq!(
        normalise(&gif, Tier::STANDARD).unwrap().media_type,
        MediaType::Png
    );
    let mut png_magic_jpeg_body = b"\x89PNG\r\n\x1a\n".to_vec();
    png_magic_jpeg_body.extend(&jpeg);
    assert_eq!(
        normalise(&png_magic_jpeg_body, Tier::STANDARD),
        Err(MediaError::Malformed)
    );
    let mut jpeg_magic_png_body = b"\xff\xd8\xff".to_vec();
    jpeg_magic_png_body.extend(png());
    assert!(normalise(&jpeg_magic_png_body, Tier::STANDARD).is_err());
}

#[test]
fn every_truncation_of_every_format_fails_cleanly() {
    for (name, bytes) in corpus() {
        for end in 1..bytes.len() {
            let result = normalise(&bytes[..end], Tier::STANDARD);
            assert_ne!(result, Err(MediaError::Panicked), "{name} cut at {end}");
            if let Ok(out) = result {
                assert!(out.width >= 1 && out.height >= 1, "{name} cut at {end}");
            }
            assert!(!panic_is_contained());
        }
    }
}

#[test]
fn corrupted_bytes_never_escape_normalise() {
    let mut state = 0x2545_f491_4f6c_dd1d_u64;
    let mut next = || {
        state ^= state << 13;
        state ^= state >> 7;
        state ^= state << 17;
        state
    };
    for (name, bytes) in corpus() {
        for round in 0..200 {
            let mut damaged = bytes.clone();
            for _ in 0..1 + next() % 8 {
                let at = usize::try_from(next() % u64::try_from(damaged.len()).unwrap()).unwrap();
                damaged[at] = next().to_le_bytes()[0];
            }
            let result = std::panic::catch_unwind(|| normalise(&damaged, Tier::STANDARD));
            assert!(result.is_ok(), "{name} round {round} escaped normalise");
        }
    }
}

#[test]
fn bombs_declared_in_the_header_are_refused_before_decoding() {
    let small = png();
    for (width, height) in [
        (20_000, 20_000),
        (16_385, 1),
        (1, 1_000_000),
        (12_000, 12_000),
    ] {
        assert_eq!(
            normalise(&with_ihdr_size(&small, width, height), Tier::HIGH),
            Err(MediaError::TooBig),
            "png {width}x{height}"
        );
    }

    let jpeg = jpeg_of(&halves(16, 16));
    let sof = jpeg
        .windows(2)
        .position(|w| w == [0xff, 0xc0])
        .expect("baseline jpeg");
    let mut jpeg_bomb = jpeg.clone();
    jpeg_bomb[sof + 5..sof + 7].copy_from_slice(&16_000u16.to_be_bytes());
    jpeg_bomb[sof + 7..sof + 9].copy_from_slice(&16_000u16.to_be_bytes());
    assert_eq!(normalise(&jpeg_bomb, Tier::HIGH), Err(MediaError::TooBig));

    let mut gif_bomb = gif_animated();
    gif_bomb[6..8].copy_from_slice(&16_000u16.to_le_bytes());
    gif_bomb[8..10].copy_from_slice(&16_000u16.to_le_bytes());
    let descriptor = gif_bomb.iter().position(|b| *b == 0x2c).expect("frame");
    gif_bomb[descriptor + 5..descriptor + 7].copy_from_slice(&16_000u16.to_le_bytes());
    gif_bomb[descriptor + 7..descriptor + 9].copy_from_slice(&16_000u16.to_le_bytes());
    assert_eq!(normalise(&gif_bomb, Tier::HIGH), Err(MediaError::TooBig));

    let mut webp_bomb = webp();
    assert_eq!(&webp_bomb[12..16], b"VP8L");
    assert_eq!(webp_bomb[20], 0x2f);
    let packed = u32::from_le_bytes(webp_bomb[21..25].try_into().unwrap());
    let huge = (packed & 0xf000_0000) | 11_999 | (11_999 << 14);
    webp_bomb[21..25].copy_from_slice(&huge.to_le_bytes());
    assert_eq!(normalise(&webp_bomb, Tier::HIGH), Err(MediaError::TooBig));

    let mut bmp_bomb = bmp();
    bmp_bomb[18..22].copy_from_slice(&16_000i32.to_le_bytes());
    bmp_bomb[22..26].copy_from_slice(&16_000i32.to_le_bytes());
    assert_eq!(normalise(&bmp_bomb, Tier::HIGH), Err(MediaError::TooBig));
}

#[test]
fn output_is_deterministic_across_runs_and_threads() {
    let mut inputs = corpus();
    let wide = RgbImage::from_fn(2100, 90, |x, y| {
        Rgb([
            u8::try_from(x % 256).unwrap(),
            u8::try_from((x * y) % 256).unwrap(),
            9,
        ])
    });
    inputs.push((
        "scaled",
        png_of(wide.as_raw(), 2100, 90, ExtendedColorType::Rgb8),
    ));
    let first: Vec<Normalised> = inputs
        .iter()
        .map(|(_, bytes)| normalise(bytes, Tier::HIGH).unwrap())
        .collect();
    let again: Vec<Normalised> = inputs
        .iter()
        .map(|(_, bytes)| normalise(bytes, Tier::HIGH).unwrap())
        .collect();
    assert_eq!(first, again);
    let threads: Vec<_> = (0..4)
        .map(|_| {
            let inputs = inputs.clone();
            std::thread::spawn(move || {
                inputs
                    .iter()
                    .map(|(_, bytes)| normalise(bytes, Tier::HIGH).unwrap())
                    .collect::<Vec<_>>()
            })
        })
        .collect();
    for handle in threads {
        assert_eq!(handle.join().unwrap(), first);
    }
}

const GREEN: [u8; 3] = [20, 210, 20];
const YELLOW: [u8; 3] = [230, 220, 30];
const SECRET: &[u8] = b"WISP-SECRET";

fn quadrants(width: u32, height: u32) -> RgbImage {
    RgbImage::from_fn(width, height, |x, y| {
        match (x < width / 2, y < height / 2) {
            (true, true) => Rgb(RED),
            (false, true) => Rgb(GREEN),
            (true, false) => Rgb(BLUE),
            (false, false) => Rgb(YELLOW),
        }
    })
}

fn unique(width: u32, height: u32) -> RgbImage {
    RgbImage::from_fn(width, height, |x, y| {
        Rgb([
            u8::try_from(x * 40).unwrap(),
            u8::try_from(y * 70).unwrap(),
            u8::try_from(200 - x - y).unwrap(),
        ])
    })
}

fn source_of(orientation: u16, width: u32, height: u32, x: u32, y: u32) -> (u32, u32) {
    match orientation {
        1 => (x, y),
        2 => (width - 1 - x, y),
        3 => (width - 1 - x, height - 1 - y),
        4 => (x, height - 1 - y),
        5 => (y, x),
        6 => (y, height - 1 - x),
        7 => (width - 1 - y, height - 1 - x),
        8 => (width - 1 - y, x),
        _ => unreachable!(),
    }
}

fn upright_size(orientation: u16, width: u32, height: u32) -> (u32, u32) {
    if orientation >= 5 {
        (height, width)
    } else {
        (width, height)
    }
}

fn png_with_exif(image: &RgbImage, orientation: u16) -> Vec<u8> {
    let png = png_of(
        image.as_raw(),
        image.width(),
        image.height(),
        ExtendedColorType::Rgb8,
    );
    insert_after_ihdr(&png, &png_chunk(b"eXIf", &tiff_exif(orientation)))
}

fn riff_chunk(kind: &[u8; 4], data: &[u8]) -> Vec<u8> {
    let mut out = kind.to_vec();
    out.extend(u32::try_from(data.len()).unwrap().to_le_bytes());
    out.extend(data);
    if data.len() % 2 == 1 {
        out.push(0);
    }
    out
}

fn riff(body: &[u8]) -> Vec<u8> {
    let mut out = b"RIFF".to_vec();
    out.extend(u32::try_from(body.len() + 4).unwrap().to_le_bytes());
    out.extend(b"WEBP");
    out.extend(body);
    out
}

fn u24(value: u32) -> [u8; 3] {
    let [a, b, c, _] = value.to_le_bytes();
    [a, b, c]
}

fn vp8x(flags: u8, width: u32, height: u32) -> Vec<u8> {
    let mut data = vec![flags, 0, 0, 0];
    data.extend(u24(width - 1));
    data.extend(u24(height - 1));
    riff_chunk(b"VP8X", &data)
}

fn vp8l_chunk(image: &RgbaImage) -> Vec<u8> {
    let mut out = Vec::new();
    WebPEncoder::new_lossless(&mut out)
        .write_image(
            image.as_raw(),
            image.width(),
            image.height(),
            ExtendedColorType::Rgba8,
        )
        .unwrap();
    assert_eq!(&out[12..16], b"VP8L");
    out[12..].to_vec()
}

fn opaque(image: &RgbImage) -> RgbaImage {
    DynamicImage::ImageRgb8(image.clone()).to_rgba8()
}

const EXIF_FLAG: u8 = 0x08;
const XMP_FLAG: u8 = 0x04;
const ALPHA_FLAG: u8 = 0x10;
const ANIMATION_FLAG: u8 = 0x02;
const ICC_FLAG: u8 = 0x20;

fn webp_with_metadata(image: &RgbImage, orientation: u16) -> Vec<u8> {
    let mut xmp = b"<x:xmpmeta>".to_vec();
    xmp.extend(SECRET);
    xmp.extend(b"</x:xmpmeta>");
    let mut body = vp8x(
        EXIF_FLAG | XMP_FLAG | ALPHA_FLAG,
        image.width(),
        image.height(),
    );
    body.extend(vp8l_chunk(&opaque(image)));
    body.extend(riff_chunk(b"EXIF", &tiff_exif(orientation)));
    body.extend(riff_chunk(b"XMP ", &xmp));
    riff(&body)
}

fn animated_webp() -> Vec<u8> {
    let red = RgbaImage::from_pixel(8, 6, Rgba([RED[0], RED[1], RED[2], 255]));
    let blue = RgbaImage::from_pixel(8, 6, Rgba([BLUE[0], BLUE[1], BLUE[2], 255]));
    let mut body = vp8x(ANIMATION_FLAG | ALPHA_FLAG, 8, 6);
    body.extend(riff_chunk(b"ANIM", &[0, 0, 0, 0, 0, 0]));
    for frame in [red, blue] {
        let mut data = Vec::new();
        data.extend(u24(0));
        data.extend(u24(0));
        data.extend(u24(7));
        data.extend(u24(5));
        data.extend(u24(100));
        data.push(0b10);
        data.extend(vp8l_chunk(&frame));
        body.extend(riff_chunk(b"ANMF", &data));
    }
    riff(&body)
}

fn webp_exif_bomb() -> Vec<u8> {
    let mut body = vp8x(EXIF_FLAG, 2, 2);
    body.extend(vp8l_chunk(&RgbaImage::from_pixel(
        2,
        2,
        Rgba([9, 9, 9, 255]),
    )));
    body.extend(b"EXIF");
    body.extend(0xffff_fff0_u32.to_le_bytes());
    riff(&body)
}

fn nested_bomb(kind: &[u8; 4]) -> Vec<u8> {
    let mut body = vp8x(ANIMATION_FLAG, 2, 2);
    body.extend(riff_chunk(b"ANIM", &[0; 6]));
    let mut frame = vec![0; 16];
    frame.extend(kind);
    frame.extend(0xffff_fff0_u32.to_le_bytes());
    body.extend(riff_chunk(b"ANMF", &frame));
    riff(&body)
}

#[test]
fn chunks_nested_in_an_animation_frame_are_refused_before_any_allocation() {
    assert_eq!(nested_bomb(b"EXIF").len(), 76);
    for kind in [b"EXIF", b"ICCP", b"XMP "] {
        let result = normalise(&nested_bomb(kind), Tier::HIGH);
        assert!(
            matches!(result, Err(MediaError::Malformed | MediaError::TooBig)),
            "{kind:?}: {result:?}"
        );
    }
}

fn adler32(bytes: &[u8]) -> u32 {
    let (mut a, mut b) = (1u32, 0u32);
    for byte in bytes {
        a = (a + u32::from(*byte)) % 65_521;
        b = (b + a) % 65_521;
    }
    (b << 16) | a
}

fn zlib_stored(data: &[u8]) -> Vec<u8> {
    let length = u16::try_from(data.len()).unwrap();
    let mut out = vec![0x78, 0x01, 0x01];
    out.extend(length.to_le_bytes());
    out.extend((!length).to_le_bytes());
    out.extend(data);
    out.extend(adler32(data).to_be_bytes());
    out
}

fn jpeg_segment(marker: u8, payload: &[u8]) -> Vec<u8> {
    let mut out = vec![0xff, marker];
    out.extend(u16::try_from(payload.len() + 2).unwrap().to_be_bytes());
    out.extend(payload);
    out
}

fn with_secret(prefix: &[u8]) -> Vec<u8> {
    let mut out = prefix.to_vec();
    out.extend(SECRET);
    out
}

#[test]
fn all_eight_png_exif_orientations_map_every_pixel() {
    let source = unique(5, 3);
    for orientation in 1..=8 {
        let out = normalise(&png_with_exif(&source, orientation), Tier::STANDARD).unwrap();
        let (w, h) = upright_size(orientation, 5, 3);
        assert_eq!((out.width, out.height), (w, h), "orientation {orientation}");
        let image = decoded(&out).to_rgb8();
        for y in 0..h {
            for x in 0..w {
                let (sx, sy) = source_of(orientation, 5, 3, x, y);
                assert_eq!(
                    image.get_pixel(x, y),
                    source.get_pixel(sx, sy),
                    "orientation {orientation} at {x},{y}"
                );
            }
        }
    }
}

fn check_quadrants(image: &RgbImage, orientation: u16, label: &str) {
    let (w, h) = upright_size(orientation, 64, 32);
    assert_eq!(image.dimensions(), (w, h), "{label} {orientation}");
    let source = quadrants(64, 32);
    for (x, y) in [
        (w / 4, h / 4),
        (3 * w / 4, h / 4),
        (w / 4, 3 * h / 4),
        (3 * w / 4, 3 * h / 4),
    ] {
        let (sx, sy) = source_of(orientation, 64, 32, x, y);
        let expected = source.get_pixel(sx, sy).0;
        assert!(
            close(*image.get_pixel(x, y), expected),
            "{label} orientation {orientation} at {x},{y}: {:?} vs {expected:?}",
            image.get_pixel(x, y)
        );
    }
}

#[test]
fn all_eight_jpeg_exif_orientations_are_applied() {
    for orientation in 1..=8 {
        let out = normalise(
            &jpeg_with_exif(&quadrants(64, 32), orientation),
            Tier::STANDARD,
        )
        .unwrap();
        assert_eq!(out.media_type, MediaType::Jpeg);
        check_quadrants(&decoded(&out).to_rgb8(), orientation, "jpeg");
    }
}

#[test]
fn all_eight_webp_exif_orientations_are_applied_and_metadata_is_stripped() {
    for orientation in 1..=8 {
        let source = webp_with_metadata(&quadrants(64, 32), orientation);
        assert!(contains(&source, SECRET));
        let out = normalise(&source, Tier::STANDARD).unwrap();
        assert_eq!(out.media_type, MediaType::Png);
        check_quadrants(&decoded(&out).to_rgb8(), orientation, "webp");
        for marker in [SECRET, b"xmpmeta", b"MM\0\x2a", b"EXIF", b"Exif", b"XMP "] {
            assert!(!contains(&out.bytes, marker), "{marker:?}");
        }
    }
}

#[test]
fn an_animated_webp_becomes_a_png_of_its_first_frame() {
    let out = normalise(&animated_webp(), Tier::STANDARD).unwrap();
    assert_eq!(out.media_type, MediaType::Png);
    assert_eq!((out.width, out.height), (8, 6));
    assert!(decoded(&out).to_rgb8().pixels().all(|p| *p == Rgb(RED)));
}

#[test]
fn png_ancillary_chunks_never_reach_the_output() {
    let mut source = png_of(halves(12, 8).as_raw(), 12, 8, ExtendedColorType::Rgb8);
    let mut icc = b"wisp\0\0".to_vec();
    icc.extend(zlib_stored(&with_secret(b"icc profile ")));
    let mut itxt = b"Comment\0\0\0\0\0".to_vec();
    itxt.extend(with_secret(b"international "));
    for chunk in [
        png_chunk(b"iCCP", &icc),
        png_chunk(b"tEXt", &with_secret(b"Author\0")),
        png_chunk(b"iTXt", &itxt),
        png_chunk(b"tIME", &[0x07, 0xea, 10, 23, 12, 0, 0]),
    ] {
        source = insert_after_ihdr(&source, &chunk);
    }
    let out = normalise(&source, Tier::STANDARD).unwrap();
    assert_eq!(decoded(&out).to_rgb8(), halves(12, 8));
    for marker in [SECRET, b"iCCP", b"tEXt", b"iTXt", b"tIME", b"zTXt", b"eXIf"] {
        assert!(!contains(&out.bytes, marker), "{marker:?}");
    }
}

#[test]
fn gif_comments_never_reach_the_output() {
    let mut source = gif_animated();
    assert_eq!(source.pop(), Some(0x3b));
    source.extend([0x21, 0xfe]);
    let comment = with_secret(b"comment ");
    source.push(u8::try_from(comment.len()).unwrap());
    source.extend(&comment);
    source.extend([0x00, 0x3b]);
    let out = normalise(&source, Tier::STANDARD).unwrap();
    assert!(!contains(&out.bytes, SECRET));
}

#[test]
fn jpeg_comments_icc_profiles_and_xmp_never_reach_the_output() {
    let jpeg = jpeg_of(&halves(16, 16));
    let mut icc = b"ICC_PROFILE\0\x01\x01".to_vec();
    icc.extend(with_secret(b"profile "));
    let xmp = with_secret(b"http://ns.adobe.com/xap/1.0/\0<x:xmpmeta>");
    let mut source = jpeg[..2].to_vec();
    source.extend(jpeg_segment(0xfe, &with_secret(b"comment ")));
    source.extend(jpeg_segment(0xe2, &icc));
    source.extend(jpeg_segment(0xe1, &xmp));
    source.extend(&jpeg[2..]);
    let out = normalise(&source, Tier::STANDARD).unwrap();
    assert_eq!(out.media_type, MediaType::Jpeg);
    for marker in [
        SECRET,
        b"ICC_PROFILE",
        b"xap/1.0",
        &[0xff, 0xfe][..],
        &[0xff, 0xe2],
        &[0xff, 0xe1],
    ] {
        assert!(!contains(&out.bytes, marker), "{marker:?}");
    }
}

#[test]
fn jpeg_output_is_encoded_at_quality_90() {
    let source = jpeg_of(&quadrants(64, 32));
    let out = normalise(&source, Tier::STANDARD).unwrap();
    let original = ImageReader::with_format(Cursor::new(&source), ImageFormat::Jpeg)
        .decode()
        .unwrap()
        .to_rgb8();
    let mut expected = Vec::new();
    JpegEncoder::new_with_quality(&mut expected, 90)
        .write_image(original.as_raw(), 64, 32, ExtendedColorType::Rgb8)
        .unwrap();
    assert_eq!(out.bytes, expected);
}

#[test]
fn a_webp_chunk_declaring_more_than_the_file_is_refused_before_any_allocation() {
    let bomb = webp_exif_bomb();
    assert!(bomb.len() < 128, "{}", bomb.len());
    assert_eq!(normalise(&bomb, Tier::HIGH), Err(MediaError::Malformed));
    let mut riff_lie = webp();
    riff_lie[4..8].copy_from_slice(&0xffff_fff0_u32.to_le_bytes());
    assert_eq!(normalise(&riff_lie, Tier::HIGH), Err(MediaError::Malformed));
    let mut icc = vp8x(ICC_FLAG, 2, 2);
    icc.extend(b"ICCP");
    icc.extend(0x7fff_fff0_u32.to_le_bytes());
    icc.extend(vp8l_chunk(&RgbaImage::from_pixel(
        2,
        2,
        Rgba([1, 1, 1, 255]),
    )));
    assert_eq!(
        normalise(&riff(&icc), Tier::HIGH),
        Err(MediaError::Malformed)
    );
}

fn hostile() -> Vec<(&'static str, Vec<u8>)> {
    let png_source = png();
    let mut png_huge_chunk = png_source[..33].to_vec();
    png_huge_chunk.extend(0x7fff_fff0_u32.to_be_bytes());
    png_huge_chunk.extend(b"eXIf");
    png_huge_chunk.extend([0; 64]);
    let mut png_huge_icc = png_source[..33].to_vec();
    png_huge_icc.extend(0x7fff_fff0_u32.to_be_bytes());
    png_huge_icc.extend(b"iCCP");
    png_huge_icc.extend(b"wisp\0\0\x78\x01");
    let mut jpeg_cut = jpeg_of(&halves(16, 16))[..2].to_vec();
    jpeg_cut.extend([0xff, 0xe1, 0xff, 0xff]);
    jpeg_cut.extend(b"Exif\0\0MM");
    let mut gif_big_frame = gif_animated();
    let descriptor = gif_big_frame.iter().position(|b| *b == 0x2c).unwrap();
    gif_big_frame[descriptor + 5..descriptor + 9].copy_from_slice(&[0xff, 0xff, 0xff, 0xff]);
    let mut bmp_palette = bmp();
    bmp_palette[46..50].copy_from_slice(&u32::MAX.to_le_bytes());
    let mut bmp_offset = bmp();
    bmp_offset[10..14].copy_from_slice(&u32::MAX.to_le_bytes());
    vec![
        ("webp exif", webp_exif_bomb()),
        ("webp nested exif", nested_bomb(b"EXIF")),
        ("webp nested icc", nested_bomb(b"ICCP")),
        ("webp nested xmp", nested_bomb(b"XMP ")),
        ("png exif", png_huge_chunk),
        ("png icc", png_huge_icc),
        ("jpeg app1", jpeg_cut),
        ("gif frame", gif_big_frame),
        ("bmp palette", bmp_palette),
        ("bmp offset", bmp_offset),
    ]
}

const CHILD: &str = "WISP_MEDIA_ADDRESS_SPACE_CHILD";

const ADDRESS_SPACE: u64 = 1 << 30;

#[test]
#[ignore = "run in a child process by declared_lengths_never_allocate_under_a_one_gib_address_space"]
fn hostile_inputs_under_a_one_gib_address_space() {
    if std::env::var_os(CHILD).is_none() {
        return;
    }
    let limit = Rlimit {
        current: Some(ADDRESS_SPACE),
        maximum: Some(ADDRESS_SPACE),
    };
    setrlimit(Resource::As, limit).unwrap();
    for (name, bytes) in hostile() {
        let result = normalise(&bytes, Tier::HIGH);
        assert_ne!(result, Err(MediaError::Panicked), "{name}");
        if !name.starts_with("bmp") {
            assert!(result.is_err(), "{name}: {result:?}");
        }
        println!("{name}: {:?}", result.map(|out| (out.width, out.height)));
    }
}

fn run_child(name: &str) -> String {
    let output = Command::new(std::env::current_exe().unwrap())
        .args([
            "--ignored",
            "--exact",
            name,
            "--test-threads=1",
            "--nocapture",
        ])
        .env(CHILD, "1")
        .output()
        .unwrap();
    let stdout = String::from_utf8_lossy(&output.stdout).into_owned();
    assert!(
        output.status.success(),
        "{:?}\n{stdout}\n{}",
        output.status,
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(stdout.contains("1 passed"), "{stdout}");
    stdout
}

#[test]
fn declared_lengths_never_allocate_under_a_one_gib_address_space() {
    let stdout = run_child("hostile_inputs_under_a_one_gib_address_space");
    assert!(stdout.contains("webp nested exif: Err("), "{stdout}");
}

fn animated_frames(width: u32, height: u32, frames: &[(u32, u32, [u8; 3])]) -> Vec<u8> {
    let mut body = vp8x(ANIMATION_FLAG | ALPHA_FLAG, width, height);
    body.extend(riff_chunk(b"ANIM", &[0; 6]));
    for (frame_width, frame_height, colour) in frames {
        let mut frame = Vec::new();
        frame.extend(u24(0));
        frame.extend(u24(0));
        frame.extend(u24(frame_width - 1));
        frame.extend(u24(frame_height - 1));
        frame.extend(u24(100));
        frame.push(0b10);
        let pixels = RgbaImage::from_pixel(
            *frame_width,
            *frame_height,
            Rgba([colour[0], colour[1], colour[2], 255]),
        );
        frame.extend(vp8l_chunk(&pixels));
        body.extend(riff_chunk(b"ANMF", &frame));
    }
    riff(&body)
}

fn animated_canvas(width: u32, height: u32) -> Vec<u8> {
    animated_frames(width, height, &[(1, 1, RED)])
}

#[test]
fn an_animated_canvas_over_a_third_of_the_decode_cap_is_refused() {
    assert!(u64::from(4620u32 * 4618) * 4 > MAX_ANIMATED_CANVAS);
    assert_eq!(
        normalise(&animated_canvas(4620, 4618), Tier::HIGH),
        Err(MediaError::TooBig)
    );
    assert_eq!(
        normalise(&animated_canvas(8000, 8000), Tier::HIGH),
        Err(MediaError::TooBig)
    );
}

#[test]
fn an_animated_canvas_just_under_the_bound_decodes() {
    assert!(u64::from(4619u32 * 4618) * 4 <= MAX_ANIMATED_CANVAS);
    let out = normalise(&animated_canvas(4619, 4618), Tier::HIGH).unwrap();
    let expected = target(4619, 4618, Tier::HIGH);
    assert_eq!((out.width, out.height), (expected.width, expected.height));
}

const ANIMATED_ADDRESS_SPACE: u64 = 384 << 20;

#[test]
#[ignore = "run in a child process by animated_webp_peaks_stay_near_the_decode_cap"]
fn animated_webp_under_a_tight_address_space() {
    if std::env::var_os(CHILD).is_none() {
        return;
    }
    let limit = Rlimit {
        current: Some(ANIMATED_ADDRESS_SPACE),
        maximum: Some(ANIMATED_ADDRESS_SPACE),
    };
    let full_size = animated_frames(4619, 4618, &[(4619, 4618, RED), (4619, 4618, BLUE)]);
    let oversized = animated_canvas(8000, 8000);
    setrlimit(Resource::As, limit).unwrap();
    let under = normalise(&full_size, Tier::HIGH).map(|out| (out.width, out.height));
    let expected = target(4619, 4618, Tier::HIGH);
    assert_eq!(under, Ok((expected.width, expected.height)));
    println!("under: ok");
    assert_eq!(normalise(&oversized, Tier::HIGH), Err(MediaError::TooBig));
}

#[test]
fn animated_webp_peaks_stay_near_the_decode_cap() {
    let stdout = run_child("animated_webp_under_a_tight_address_space");
    assert!(stdout.contains("under: ok"), "{stdout}");
}

fn noise(count: usize, seed: u64) -> Vec<u8> {
    let mut state = seed;
    (0..count)
        .map(|_| {
            state ^= state << 13;
            state ^= state >> 7;
            state ^= state << 17;
            state.to_le_bytes()[0]
        })
        .collect()
}

fn gaussian_photo(side: u32, sigma_twenty: bool) -> RgbImage {
    let raw = noise(
        usize::try_from(side * side * 3 * 4).unwrap(),
        0x5eed_1234_5678_9abc,
    );
    let mut pixels = raw.as_chunks::<4>().0.iter().map(|four| {
        let sum: i32 = four.iter().map(|b| i32::from(*b)).sum();
        let centred = sum - 510;
        let spread = if sigma_twenty {
            centred * 20 / 147
        } else {
            centred
        };
        u8::try_from((128 + spread).clamp(0, 255)).unwrap()
    });
    RgbImage::from_fn(side, side, |_, _| {
        Rgb([
            pixels.next().unwrap(),
            pixels.next().unwrap(),
            pixels.next().unwrap(),
        ])
    })
}

#[test]
fn a_noisy_photo_on_the_high_tier_falls_back_to_jpeg() {
    let photo = gaussian_photo(1932, true);
    let mut source = Vec::new();
    WebPEncoder::new_lossless(&mut source)
        .write_image(&opaque(&photo), 1932, 1932, ExtendedColorType::Rgba8)
        .unwrap();
    let out = normalise(&source, Tier::HIGH).unwrap();
    assert_eq!(out.media_type, MediaType::Jpeg);
    assert_eq!((out.width, out.height), (1932, 1932));
    assert!(base64_len(out.bytes.len()) <= MAX_IMAGE_BASE64);
}

#[test]
fn a_png_between_the_raw_and_base64_caps_falls_back_to_jpeg() {
    let raw = noise(1700 * 1700 * 3, 0x0123_4567_89ab_cdef);
    let source = png_of(&raw, 1700, 1700, ExtendedColorType::Rgb8);
    let out = normalise(&source, Tier::HIGH).unwrap();
    assert_eq!(out.media_type, MediaType::Jpeg);
    assert_eq!((out.width, out.height), (1700, 1700));
}

#[test]
fn opaque_alpha_falls_back_to_jpeg_and_real_alpha_shrinks_as_png() {
    let mut rgba = noise(1990 * 1400 * 4, 0x9e37_79b9_7f4a_7c15);
    for alpha in rgba.iter_mut().skip(3).step_by(4) {
        *alpha = 255;
    }
    let source = png_of(&rgba, 1990, 1400, ExtendedColorType::Rgba8);
    let out = normalise(&source, Tier::HIGH).unwrap();
    assert_eq!(out.media_type, MediaType::Jpeg);
    assert_eq!((out.width, out.height), (1990, 1400));

    let translucent = noise(1990 * 1400 * 4, 0x9e37_79b9_7f4a_7c15);
    let source = png_of(&translucent, 1990, 1400, ExtendedColorType::Rgba8);
    let out = normalise(&source, Tier::HIGH).unwrap();
    assert_eq!(out.media_type, MediaType::Png);
    assert!(base64_len(out.bytes.len()) <= MAX_IMAGE_BASE64);
    assert_eq!((out.width, out.height), (1492, 1050));
    assert!(out.width.max(out.height) >= MIN_FALLBACK_EDGE);
    assert_eq!(decoded(&out).color(), ColorType::Rgba8);
}
