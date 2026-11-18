use tokio::sync::mpsc;
use wisp_media::{MAX_IMAGES_PER_MESSAGE, MAX_SESSION_MEDIA_BASE64, MediaError, MediaType};
use wisp_protocol::log::ImageRef;
use wisp_protocol::{AgentEvent, ContentBlock, ImageData, ImageSource, ImageType, InputId, Mime};

use super::{Agent, emit};
use crate::provider::ImageCaps;
use crate::session::sha256_bytes_hex;

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct PreparedImage {
    pub(super) sha256: String,
    pub(super) media_type: ImageType,
    pub(super) bytes: Vec<u8>,
    pub(super) base64: String,
    pub(super) width: u32,
    pub(super) height: u32,
}

impl PreparedImage {
    pub(super) fn into_block(self) -> ContentBlock {
        ContentBlock::image(self.media_type, self.base64)
    }

    fn reference(&self) -> ImageRef {
        ImageRef {
            sha256: self.sha256.clone(),
            media_type: self.media_type,
            bytes: u64::try_from(self.bytes.len()).unwrap_or(u64::MAX),
            width: self.width,
            height: self.height,
        }
    }
}

fn image_type(media_type: MediaType) -> ImageType {
    match media_type {
        MediaType::Png => ImageType::Png,
        MediaType::Jpeg => ImageType::Jpeg,
    }
}

pub fn no_image_sentence(model: &str) -> String {
    format!(
        "{} can't take images: its limits are unknown.",
        wisp_protocol::shown_model(model)
    )
}

fn normalise_all(images: &[ImageData], caps: ImageCaps) -> Result<Vec<PreparedImage>, String> {
    images
        .iter()
        .enumerate()
        .map(|(i, image)| {
            let prepared = wisp_media::decode_base64(&image.data)
                .and_then(|bytes| wisp_media::normalise(&bytes, caps.tier))
                .map_err(|e: MediaError| format!("Image {}: {e}", i + 1))?;
            Ok(PreparedImage {
                sha256: sha256_bytes_hex(&prepared.bytes),
                media_type: image_type(prepared.media_type),
                base64: wisp_media::encode_base64(&prepared.bytes),
                bytes: prepared.bytes,
                width: prepared.width,
                height: prepared.height,
            })
        })
        .collect()
}

const IMAGE_TOP_LEVEL: &str = "image";

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub(super) struct Media {
    pub(super) images: usize,
    pub(super) files: usize,
    pub(super) bytes: usize,
}

impl std::ops::Add for Media {
    type Output = Self;

    fn add(self, other: Self) -> Self {
        Self {
            images: self.images + other.images,
            files: self.files + other.files,
            bytes: self.bytes.saturating_add(other.bytes),
        }
    }
}

pub(super) fn history_media<'a>(blocks: impl Iterator<Item = &'a ContentBlock>) -> Media {
    blocks.fold(Media::default(), |media, block| match block {
        ContentBlock::Image {
            source: ImageSource::Base64 { data, .. },
        } => {
            media
                + Media {
                    images: 1,
                    files: 0,
                    bytes: data.len(),
                }
        }
        ContentBlock::Media { source } => {
            let image = usize::from(source.media_type().top_level() == IMAGE_TOP_LEVEL);
            let bytes = source.data().len();
            media
                + Media {
                    images: image,
                    files: 1 - image,
                    bytes,
                }
        }
        ContentBlock::ToolResult { content, .. } => media + history_media(content.iter()),
        _ => media,
    })
}

pub(super) fn media_budget_sentence(files: usize) -> String {
    let subject = if files == 1 {
        "This file"
    } else {
        "These files"
    };
    format!(
        "{subject} would take this conversation past {} MB of attachments.",
        MAX_SESSION_MEDIA_BASE64 / 1_000_000
    )
}

fn base64_len(image: &ImageRef) -> usize {
    usize::try_from(image.bytes)
        .unwrap_or(usize::MAX)
        .div_ceil(3)
        .saturating_mul(4)
}

impl Agent {
    pub(super) async fn prepare_images(
        &mut self,
        images: Vec<ImageData>,
        this_turn: bool,
        replacing: Option<&InputId>,
    ) -> Result<Vec<PreparedImage>, String> {
        if images.is_empty() {
            return Ok(Vec::new());
        }
        if images.len() > MAX_IMAGES_PER_MESSAGE {
            return Err(format!(
                "Too many images: a message can carry at most {MAX_IMAGES_PER_MESSAGE}."
            ));
        }
        let (provider, model) = if this_turn {
            (self.provider().clone(), self.session.model.clone())
        } else {
            self.next_target()
        };
        let caps = provider
            .capabilities(&model)
            .and_then(|row| row.images)
            .ok_or_else(|| no_image_sentence(&model))?;
        let prepared = tokio::task::spawn_blocking(move || normalise_all(&images, caps))
            .await
            .map_err(|_| "Preparing the images stopped unexpectedly.".to_owned())??;
        self.check_caps(&model, caps, &prepared, replacing)?;
        Ok(prepared)
    }

    pub(super) fn check_caps(
        &self,
        model: &str,
        caps: ImageCaps,
        prepared: &[PreparedImage],
        replacing: Option<&InputId>,
    ) -> Result<(), String> {
        let (count, bytes) = self.media_in_use(replacing, &[]);
        let pending: &[ImageRef] = self.track.as_ref().map_or(&[], |t| &t.pending_images);
        let total = count + prepared.len();
        if total > caps.per_request {
            return Err(format!(
                "{} takes at most {} images in a request, and this conversation would send {total}.",
                wisp_protocol::shown_model(model),
                caps.per_request
            ));
        }
        let edge = caps.many_image_edge;
        let oversized = prepared.iter().any(|p| p.width.max(p.height) > edge)
            || self
                .inbox
                .waiting_beside(replacing)
                .flat_map(|i| i.refs.iter())
                .chain(pending)
                .any(|r| r.width.max(r.height) > edge);
        if total > caps.many_images && oversized {
            return Err(format!(
                "With more than {} images in a request, each must be at most {} px on its long edge.",
                caps.many_images, caps.many_image_edge
            ));
        }
        let added: usize = prepared.iter().map(|p| p.base64.len()).sum();
        if bytes.saturating_add(added) > MAX_SESSION_MEDIA_BASE64 {
            return Err(format!(
                "These images would take this conversation past {} MB of images.",
                MAX_SESSION_MEDIA_BASE64 / 1_000_000
            ));
        }
        Ok(())
    }

    pub(super) fn media_in_use(
        &self,
        replacing: Option<&InputId>,
        earlier: &[ContentBlock],
    ) -> (usize, usize) {
        let history = history_media(
            self.history
                .iter()
                .skip(self.view_from())
                .flat_map(|m| m.content.iter()),
        );
        let waiting = history_media(
            self.inbox
                .waiting_beside(replacing)
                .flat_map(|i| i.images.iter()),
        );
        let earlier = history_media(earlier.iter());
        let pending: &[ImageRef] = self.track.as_ref().map_or(&[], |t| &t.pending_images);
        let pending_bytes: usize = pending.iter().map(base64_len).sum();
        let count = history.images + waiting.images + earlier.images + pending.len();
        let bytes = history
            .bytes
            .saturating_add(waiting.bytes)
            .saturating_add(earlier.bytes)
            .saturating_add(pending_bytes);
        (count, bytes)
    }

    pub(super) async fn save_images(
        &mut self,
        prepared: &[PreparedImage],
        events: &mpsc::Sender<AgentEvent>,
    ) -> Vec<ImageRef> {
        for image in prepared {
            let failure = match &mut self.log {
                Some((log, _)) => {
                    log.put_blob(&image.sha256, &Mime::from(image.media_type), &image.bytes)
                }
                None => None,
            };
            if let Some(reason) = failure {
                emit(events, AgentEvent::LogDisabled { reason }).await;
            }
        }
        prepared.iter().map(PreparedImage::reference).collect()
    }
}
