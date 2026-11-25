use std::sync::LazyLock;

use serde::{Serialize, Serializer};
use wisp_protocol::{FileSlice, MediaSource};

use super::super::{NotSent, not_sent_note};

pub static MEDIA_TYPES: LazyLock<Vec<&str>> =
    LazyLock::new(|| BLOCKS.iter().map(|(media_type, _)| *media_type).collect());

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum Carried {
    Image,
    Document,
}

pub(super) const BLOCKS: [(&str, Carried); 5] = [
    ("application/pdf", Carried::Document),
    ("image/gif", Carried::Image),
    ("image/jpeg", Carried::Image),
    ("image/png", Carried::Image),
    ("image/webp", Carried::Image),
];

pub(super) fn carried(media_type: &str) -> Option<Carried> {
    BLOCKS
        .iter()
        .find(|(listed, _)| *listed == media_type)
        .map(|(_, carried)| *carried)
}

#[derive(Serialize)]
struct Base64<'a> {
    #[serde(rename = "type")]
    kind: &'static str,
    media_type: &'a str,
    data: &'a str,
}

#[derive(Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
enum Block<'a> {
    Image { source: Base64<'a> },
    Document { source: Base64<'a> },
    Text { text: String },
}

pub(super) struct WireMedia<'a>(pub(super) &'a MediaSource, pub(super) Option<FileSlice>);

impl Serialize for WireMedia<'_> {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        let media_type = self.0.media_type().as_str();
        if self.1.is_some() {
            return Block::Text {
                text: not_sent_note(media_type, self.1, NotSent::Slice),
            }
            .serialize(serializer);
        }
        let source = Base64 {
            kind: "base64",
            media_type,
            data: self.0.data(),
        };
        match carried(media_type) {
            Some(Carried::Image) => Block::Image { source }.serialize(serializer),
            Some(Carried::Document) => Block::Document { source }.serialize(serializer),
            None => Block::Text {
                text: not_sent_note(media_type, None, NotSent::Server),
            }
            .serialize(serializer),
        }
    }
}
