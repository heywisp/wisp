mod contain;
mod encoding;
mod error;
mod normalise;
mod riff;
mod size;
mod sniff;

pub use contain::panic_is_contained;
pub use encoding::{MAX_BASE64_INPUT, base64_len, decode_base64, encode_base64};
pub use error::MediaError;
pub use normalise::{
    JPEG_QUALITY, MAX_ANIMATED_CANVAS, MAX_DECODE_ALLOC, MAX_DECODE_SIDE, MAX_IMAGE_BASE64,
    MAX_SOURCE_BYTES, MIN_FALLBACK_EDGE, MediaType, Normalised, dimensions, normalise,
};
pub use size::{ImageSize, MAX_LONG_EDGE, PATCH, Tier, api_size, target, tokens};
pub use sniff::{SourceFormat, sniff};

pub const MAX_IMAGES_PER_MESSAGE: usize = 10;

pub const MAX_SESSION_MEDIA_BASE64: usize = 20_000_000;

pub const MAX_ATTACHMENT_BYTES: usize = MAX_IMAGE_BASE64 / 4 * 3;

#[cfg(feature = "testing")]
pub fn contain_for_testing<R>(work: impl FnOnce() -> R) -> Result<R, MediaError> {
    contain::contain(|| Ok(work()))
}
