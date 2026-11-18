use image::ImageFormat;

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum SourceFormat {
    Png,
    Jpeg,
    Gif,
    Webp,
    Bmp,
}

impl SourceFormat {
    pub(crate) const fn image_format(self) -> ImageFormat {
        match self {
            Self::Png => ImageFormat::Png,
            Self::Jpeg => ImageFormat::Jpeg,
            Self::Gif => ImageFormat::Gif,
            Self::Webp => ImageFormat::WebP,
            Self::Bmp => ImageFormat::Bmp,
        }
    }
}

const PNG: &[u8] = b"\x89PNG\r\n\x1a\n";
const JPEG: &[u8] = b"\xff\xd8\xff";
const GIF87: &[u8] = b"GIF87a";
const GIF89: &[u8] = b"GIF89a";
const RIFF: &[u8] = b"RIFF";
const WEBP: &[u8] = b"WEBP";
const BMP: &[u8] = b"BM";

pub fn sniff(bytes: &[u8]) -> Option<SourceFormat> {
    if bytes.starts_with(PNG) {
        Some(SourceFormat::Png)
    } else if bytes.starts_with(JPEG) {
        Some(SourceFormat::Jpeg)
    } else if bytes.starts_with(GIF87) || bytes.starts_with(GIF89) {
        Some(SourceFormat::Gif)
    } else if bytes.starts_with(RIFF) && bytes.get(8..12) == Some(WEBP) {
        Some(SourceFormat::Webp)
    } else if bytes.starts_with(BMP) && bytes.len() >= 26 {
        Some(SourceFormat::Bmp)
    } else {
        None
    }
}

#[cfg(test)]
mod tests;
