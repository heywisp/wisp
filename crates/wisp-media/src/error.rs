#[derive(Clone, Copy, Debug, PartialEq, Eq, thiserror::Error)]
pub enum MediaError {
    #[error("The image is empty.")]
    Empty,
    #[error("The image file is larger than 40 MB.")]
    SourceTooLarge,
    #[error("This isn't a PNG, JPEG, GIF, WebP or BMP image.")]
    Unsupported,
    #[error(
        "The image is larger than 16,384 pixels on a side or needs more than 256 MB to decode."
    )]
    TooBig,
    #[error("The image is damaged and couldn't be read.")]
    Malformed,
    #[error("The image couldn't be converted for sending.")]
    Encode,
    #[error("The image is still larger than 10 MB after converting and shrinking it.")]
    EncodedTooLarge,
    #[error("The image data isn't valid base64.")]
    Base64,
    #[error("Reading the image failed inside the decoder.")]
    Panicked,
}
