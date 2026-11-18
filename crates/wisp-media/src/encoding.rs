use base64::Engine;
use base64::engine::general_purpose::STANDARD;

use crate::MediaError;
use crate::normalise::MAX_SOURCE_BYTES;

pub const MAX_BASE64_INPUT: usize = MAX_SOURCE_BYTES.div_ceil(3) * 4;

pub const fn base64_len(bytes: usize) -> usize {
    bytes.div_ceil(3).saturating_mul(4)
}

pub fn encode_base64(bytes: &[u8]) -> String {
    STANDARD.encode(bytes)
}

pub fn decode_base64(text: &str) -> Result<Vec<u8>, MediaError> {
    if text.len() > MAX_BASE64_INPUT {
        return Err(MediaError::SourceTooLarge);
    }
    STANDARD.decode(text).map_err(|_| MediaError::Base64)
}

#[cfg(test)]
mod tests;
