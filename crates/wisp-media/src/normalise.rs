use std::io::Cursor;

use image::codecs::jpeg::JpegEncoder;
use image::codecs::png::{CompressionType, FilterType as PngFilter, PngEncoder};
use image::error::{ImageError, LimitErrorKind};
use image::imageops::FilterType;
use image::metadata::Orientation;
use image::{
    DynamicImage, ExtendedColorType, ImageDecoder, ImageEncoder, ImageReader, Limits, RgbImage,
    RgbaImage,
};
use image_webp::{DecodingError as WebpError, WebPDecoder};

use crate::contain::contain;
use crate::encoding::base64_len;
use crate::riff::check_chunks;
use crate::size::capped;
use crate::{MediaError, SourceFormat, Tier, sniff, target};

pub const MAX_SOURCE_BYTES: usize = 40_000_000;

pub const MAX_DECODE_SIDE: u32 = 16384;

pub const MAX_DECODE_ALLOC: u64 = 256_000_000;

pub const MAX_ANIMATED_CANVAS: u64 = MAX_DECODE_ALLOC / 3;

pub const MAX_IMAGE_BASE64: usize = 10_000_000;

pub const JPEG_QUALITY: u8 = 90;

const PRE_SHRINK: u32 = 2;

pub const MIN_FALLBACK_EDGE: u32 = 256;

const STEP_NUMERATOR: u32 = 3;

const STEP_DENOMINATOR: u32 = 4;

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum MediaType {
    Png,
    Jpeg,
}

impl MediaType {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Png => "image/png",
            Self::Jpeg => "image/jpeg",
        }
    }

    pub const fn extension(self) -> &'static str {
        match self {
            Self::Png => "png",
            Self::Jpeg => "jpg",
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Normalised {
    pub bytes: Vec<u8>,
    pub media_type: MediaType,
    pub width: u32,
    pub height: u32,
}

pub fn normalise(bytes: &[u8], tier: Tier) -> Result<Normalised, MediaError> {
    if bytes.is_empty() {
        return Err(MediaError::Empty);
    }
    if bytes.len() > MAX_SOURCE_BYTES {
        return Err(MediaError::SourceTooLarge);
    }
    let format = sniff(bytes).ok_or(MediaError::Unsupported)?;
    contain(|| convert(bytes, format, tier))
}

pub fn dimensions(bytes: &[u8]) -> Result<(u32, u32), MediaError> {
    let format = sniff(bytes).ok_or(MediaError::Unsupported)?;
    contain(|| {
        if format == SourceFormat::Webp {
            let decoder = WebPDecoder::new(Cursor::new(bytes)).map_err(webp_error)?;
            return Ok(decoder.dimensions());
        }
        ImageReader::with_format(Cursor::new(bytes), format.image_format())
            .into_dimensions()
            .map_err(decode_error)
    })
}

fn limits() -> Limits {
    let mut limits = Limits::default();
    limits.max_image_width = Some(MAX_DECODE_SIDE);
    limits.max_image_height = Some(MAX_DECODE_SIDE);
    limits.max_alloc = Some(MAX_DECODE_ALLOC);
    limits
}

fn decode_error(error: ImageError) -> MediaError {
    match error {
        ImageError::Limits(limit) => match limit.kind() {
            LimitErrorKind::DimensionError | LimitErrorKind::InsufficientMemory => {
                MediaError::TooBig
            }
            _ => MediaError::Malformed,
        },
        ImageError::Unsupported(_) => MediaError::Unsupported,
        _ => MediaError::Malformed,
    }
}

fn decode(bytes: &[u8], format: SourceFormat) -> Result<(DynamicImage, Orientation), MediaError> {
    if format == SourceFormat::Webp {
        return decode_webp(bytes);
    }
    let mut reader = ImageReader::with_format(Cursor::new(bytes), format.image_format());
    reader.limits(limits());
    let mut decoder = reader.into_decoder().map_err(decode_error)?;
    if decoder.total_bytes() > MAX_DECODE_ALLOC {
        return Err(MediaError::TooBig);
    }
    let orientation = decoder.orientation().map_err(decode_error)?;
    let image = DynamicImage::from_decoder(decoder).map_err(decode_error)?;
    Ok((image, orientation))
}

fn webp_error(error: WebpError) -> MediaError {
    match error {
        WebpError::MemoryLimitExceeded | WebpError::ImageTooLarge => MediaError::TooBig,
        _ => MediaError::Malformed,
    }
}

pub(crate) const fn canvas_bytes(width: u32, height: u32) -> u64 {
    (width as u64 * height as u64).saturating_mul(4)
}

pub(crate) fn decode_webp(bytes: &[u8]) -> Result<(DynamicImage, Orientation), MediaError> {
    let mut decoder = WebPDecoder::new(Cursor::new(bytes)).map_err(webp_error)?;
    decoder.set_memory_limit(MAX_SOURCE_BYTES);
    let (width, height) = decoder.dimensions();
    if width == 0 || height == 0 {
        return Err(MediaError::Malformed);
    }
    if width > MAX_DECODE_SIDE || height > MAX_DECODE_SIDE {
        return Err(MediaError::TooBig);
    }
    let size = decoder.output_buffer_size().ok_or(MediaError::TooBig)?;
    if u64::try_from(size).map_err(|_| MediaError::TooBig)? > MAX_DECODE_ALLOC {
        return Err(MediaError::TooBig);
    }
    if decoder.is_animated() && canvas_bytes(width, height) > MAX_ANIMATED_CANVAS {
        return Err(MediaError::TooBig);
    }
    let orientation = decoder
        .exif_metadata()
        .map_err(webp_error)?
        .and_then(|exif| Orientation::from_exif_chunk(&exif))
        .unwrap_or(Orientation::NoTransforms);
    let mut pixels = vec![0; size];
    decoder.read_image(&mut pixels).map_err(webp_error)?;
    let image = if decoder.has_alpha() {
        RgbaImage::from_raw(width, height, pixels).map(DynamicImage::ImageRgba8)
    } else {
        RgbImage::from_raw(width, height, pixels).map(DynamicImage::ImageRgb8)
    };
    Ok((image.ok_or(MediaError::Malformed)?, orientation))
}

const fn swaps_axes(orientation: Orientation) -> bool {
    matches!(
        orientation,
        Orientation::Rotate90
            | Orientation::Rotate270
            | Orientation::Rotate90FlipH
            | Orientation::Rotate270FlipH
    )
}

fn scaled(image: DynamicImage, width: u32, height: u32) -> DynamicImage {
    if (width, height) == (image.width(), image.height()) {
        return image;
    }
    let coarse_width = image.width().min(width.saturating_mul(PRE_SHRINK));
    let coarse_height = image.height().min(height.saturating_mul(PRE_SHRINK));
    let image = if (coarse_width, coarse_height) == (image.width(), image.height()) {
        image
    } else {
        let coarse = image.thumbnail_exact(coarse_width, coarse_height);
        drop(image);
        coarse
    };
    image.resize_exact(width, height, FilterType::Lanczos3)
}

fn upright(bytes: &[u8], format: SourceFormat, tier: Tier) -> Result<DynamicImage, MediaError> {
    if format == SourceFormat::Webp {
        check_chunks(bytes)?;
    }
    let (image, orientation) = decode(bytes, format)?;
    let swapped = swaps_axes(orientation);
    let (upright_width, upright_height) = if swapped {
        (image.height(), image.width())
    } else {
        (image.width(), image.height())
    };
    let size = target(upright_width, upright_height, tier);
    if size.width == 0 || size.height == 0 {
        return Err(MediaError::Malformed);
    }
    let (stored_width, stored_height) = if swapped {
        (size.height, size.width)
    } else {
        (size.width, size.height)
    };
    let mut image = scaled(image, stored_width, stored_height);
    image.apply_orientation(orientation);
    Ok(image)
}

pub(crate) const fn fits_cap(encoded: usize) -> bool {
    base64_len(encoded) <= MAX_IMAGE_BASE64
}

pub(crate) const fn next_step(long_edge: u32) -> Option<u32> {
    let next = long_edge / STEP_DENOMINATOR * STEP_NUMERATOR
        + long_edge % STEP_DENOMINATOR * STEP_NUMERATOR / STEP_DENOMINATOR;
    if next < MIN_FALLBACK_EDGE {
        None
    } else {
        Some(next)
    }
}

fn has_real_alpha(image: &DynamicImage) -> bool {
    image.color().has_alpha() && image.to_rgba8().pixels().any(|p| p.0[3] != u8::MAX)
}

fn encode(image: &DynamicImage, media_type: MediaType) -> Result<Vec<u8>, MediaError> {
    match media_type {
        MediaType::Png => encode_png(image),
        MediaType::Jpeg => encode_jpeg(image),
    }
}

fn finished(image: &DynamicImage, bytes: Vec<u8>, media_type: MediaType) -> Normalised {
    Normalised {
        bytes,
        media_type,
        width: image.width(),
        height: image.height(),
    }
}

fn convert(bytes: &[u8], format: SourceFormat, tier: Tier) -> Result<Normalised, MediaError> {
    let image = upright(bytes, format, tier)?;
    let preferred = match format {
        SourceFormat::Jpeg => MediaType::Jpeg,
        SourceFormat::Png | SourceFormat::Gif | SourceFormat::Webp | SourceFormat::Bmp => {
            MediaType::Png
        }
    };
    let encoded = encode(&image, preferred)?;
    if fits_cap(encoded.len()) {
        return Ok(finished(&image, encoded, preferred));
    }
    drop(encoded);
    let fallback = if preferred == MediaType::Png && !has_real_alpha(&image) {
        MediaType::Jpeg
    } else {
        preferred
    };
    if fallback != preferred {
        let encoded = encode(&image, fallback)?;
        if fits_cap(encoded.len()) {
            return Ok(finished(&image, encoded, fallback));
        }
    }
    let mut long_edge = image.width().max(image.height());
    loop {
        long_edge = next_step(long_edge).ok_or(MediaError::EncodedTooLarge)?;
        let size = capped(image.width(), image.height(), long_edge);
        let smaller = image.resize_exact(size.width, size.height, FilterType::Lanczos3);
        let encoded = encode(&smaller, fallback)?;
        if fits_cap(encoded.len()) {
            return Ok(finished(&smaller, encoded, fallback));
        }
    }
}

fn encode_png(image: &DynamicImage) -> Result<Vec<u8>, MediaError> {
    let (raw, colour) = if image.color().has_alpha() {
        if image.color().has_color() {
            (image.to_rgba8().into_raw(), ExtendedColorType::Rgba8)
        } else {
            (image.to_luma_alpha8().into_raw(), ExtendedColorType::La8)
        }
    } else if image.color().has_color() {
        (image.to_rgb8().into_raw(), ExtendedColorType::Rgb8)
    } else {
        (image.to_luma8().into_raw(), ExtendedColorType::L8)
    };
    let mut out = Vec::new();
    PngEncoder::new_with_quality(&mut out, CompressionType::Default, PngFilter::Adaptive)
        .write_image(&raw, image.width(), image.height(), colour)
        .map_err(|_| MediaError::Encode)?;
    Ok(out)
}

fn encode_jpeg(image: &DynamicImage) -> Result<Vec<u8>, MediaError> {
    let (raw, colour) = if image.color().has_color() {
        (image.to_rgb8().into_raw(), ExtendedColorType::Rgb8)
    } else {
        (image.to_luma8().into_raw(), ExtendedColorType::L8)
    };
    let mut out = Vec::new();
    JpegEncoder::new_with_quality(&mut out, JPEG_QUALITY)
        .write_image(&raw, image.width(), image.height(), colour)
        .map_err(|_| MediaError::Encode)?;
    Ok(out)
}

#[cfg(test)]
mod tests;
