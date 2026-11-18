use std::fmt;
use std::str::FromStr;
use std::sync::Arc;

use serde::{Deserialize, Deserializer, Serialize};

use crate::{ImageType, ParseError};

pub const MAX_MIME_LEN: usize = 127;

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize)]
#[serde(transparent)]
pub struct Mime(String);

fn name_char(c: char) -> bool {
    c.is_ascii_lowercase() || c.is_ascii_digit() || "!#$&-^_.+".contains(c)
}

fn valid_name(name: &str) -> bool {
    name.chars()
        .next()
        .is_some_and(|c| c.is_ascii_lowercase() || c.is_ascii_digit())
        && name.chars().all(name_char)
}

impl Mime {
    pub fn as_str(&self) -> &str {
        &self.0
    }

    pub fn top_level(&self) -> &str {
        self.0.split_once('/').map_or("", |(top, _)| top)
    }

    pub fn subtype(&self) -> &str {
        self.0.split_once('/').map_or("", |(_, sub)| sub)
    }
}

impl FromStr for Mime {
    type Err = ParseError;

    fn from_str(text: &str) -> Result<Self, Self::Err> {
        let refuse = |why: &str| ParseError(format!("{why}: {:?}", text));
        if text.len() > MAX_MIME_LEN {
            return Err(refuse(&format!(
                "a media type is at most {MAX_MIME_LEN} bytes"
            )));
        }
        let Some((top, sub)) = text.split_once('/') else {
            return Err(refuse("a media type is type/subtype"));
        };
        if !valid_name(top) || !valid_name(sub) {
            return Err(refuse(
                "a media type is lowercase ASCII type/subtype, with no parameters",
            ));
        }
        Ok(Self(text.to_owned()))
    }
}

impl<'de> Deserialize<'de> for Mime {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let text = String::deserialize(deserializer)?;
        text.parse().map_err(serde::de::Error::custom)
    }
}

impl fmt::Display for Mime {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl From<ImageType> for Mime {
    fn from(media_type: ImageType) -> Self {
        Self(media_type.as_str().to_owned())
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum MediaSource {
    Base64 { media_type: Mime, data: Arc<str> },
}

impl MediaSource {
    pub fn media_type(&self) -> &Mime {
        match self {
            Self::Base64 { media_type, .. } => media_type,
        }
    }

    pub fn data(&self) -> &str {
        match self {
            Self::Base64 { data, .. } => data,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BlobRef {
    pub media_type: Mime,
    pub sha256: String,
    pub bytes: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AttachmentRef {
    pub media_type: Mime,
    pub sha256: String,
    pub bytes: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub saved: Option<String>,
}

impl AttachmentRef {
    pub fn blob(&self) -> BlobRef {
        BlobRef {
            media_type: self.media_type.clone(),
            sha256: self.sha256.clone(),
            bytes: self.bytes,
        }
    }
}
