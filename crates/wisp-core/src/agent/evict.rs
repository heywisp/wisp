use std::borrow::Cow;
use std::collections::BTreeMap;
use std::io;
use std::ops::Range;
use std::sync::Arc;

use wisp_protocol::log::{MediaSlot, media_slots};
use wisp_protocol::{BlobRef, ContentBlock, ImageSource, MediaSource, Message, Mime};

use super::Agent;
use crate::context::block_tokens;
use crate::session::{SessionStore, base64_ref, blob_name, read_blob};

#[derive(Debug, Clone)]
pub(super) struct Kept {
    blob: BlobRef,
    tokens: u64,
    held: u64,
}

pub(super) type Evicted = BTreeMap<(usize, usize), Kept>;

fn data(block: &mut ContentBlock) -> Option<(&mut Arc<str>, Mime)> {
    match block {
        ContentBlock::Image {
            source: ImageSource::Base64 { media_type, data },
        } => Some((data, Mime::from(*media_type))),
        ContentBlock::Media {
            source: MediaSource::Base64 { media_type, data },
            ..
        } => Some((data, media_type.clone())),
        _ => None,
    }
}

fn slots(message: &mut Message) -> Vec<MediaSlot<'_>> {
    let mut slots = Vec::new();
    media_slots(&mut message.content, &mut slots);
    slots
}

fn evicted(store: &dyn SessionStore, block: &mut ContentBlock) -> Option<Kept> {
    let tokens = block_tokens(block);
    let (held, media_type) = data(block)?;
    let blob = base64_ref(&media_type, held).filter(|_| !held.is_empty())?;
    store.get_blob(&blob.sha256, &blob.media_type).ok()?;
    *held = Arc::from("");
    Some(Kept {
        blob,
        tokens,
        held: block_tokens(block),
    })
}

fn reloaded(store: &dyn SessionStore, blob: &BlobRef) -> Result<Arc<str>, String> {
    read_blob(store, blob).map(Arc::from).map_err(|e| {
        let what = if blob.media_type.top_level() == "image" {
            "image"
        } else {
            "attachment"
        };
        let name = blob_name(&blob.sha256, &blob.media_type);
        if e.kind() == io::ErrorKind::NotFound {
            format!("the {what} file {name} is missing from the attachments folder")
        } else {
            format!("the {what} file {name} in the attachments folder can't be read ({e})")
        }
    })
}

fn put_back(history: &mut [Message], (message, slot): (usize, usize), restored: Arc<str>) {
    let Some(message) = history.get_mut(message) else {
        return;
    };
    if let Some((held, _)) = slots(message)
        .into_iter()
        .nth(slot)
        .and_then(|slot| data(slot.block))
    {
        *held = restored;
    }
}

impl Agent {
    pub(super) fn evict_media(&mut self) {
        let Some((log, _)) = self.log.as_ref().filter(|(log, _)| log.is_on()) else {
            return;
        };
        let upto = self.view_from().min(self.history.len());
        for (index, message) in self.history.iter_mut().enumerate().take(upto) {
            for (slot, media) in slots(message).into_iter().enumerate() {
                if self.evicted.contains_key(&(index, slot)) {
                    continue;
                }
                if let Some(kept) = evicted(log.store(), media.block) {
                    self.evicted.insert((index, slot), kept);
                }
            }
        }
    }

    pub(super) fn restore_media(&mut self, from: usize) -> Result<(), String> {
        let spots: Vec<(usize, usize)> =
            self.evicted.range((from, 0)..).map(|(at, _)| *at).collect();
        for at in spots {
            if let Some(kept) = self.evicted.get(&at) {
                let Some((log, _)) = &self.log else {
                    let name = blob_name(&kept.blob.sha256, &kept.blob.media_type);
                    return Err(format!("there is no session log to read {name} from"));
                };
                let restored = reloaded(log.store(), &kept.blob)?;
                put_back(&mut self.history, at, restored);
            }
            self.evicted.remove(&at);
        }
        Ok(())
    }

    pub(super) fn restored_history(&self) -> Cow<'_, [Message]> {
        let Some((log, _)) = self.log.as_ref().filter(|_| !self.evicted.is_empty()) else {
            return Cow::Borrowed(&self.history);
        };
        let mut history = self.history.clone();
        for (at, kept) in &self.evicted {
            if let Ok(restored) = reloaded(log.store(), &kept.blob) {
                put_back(&mut history, *at, restored);
            }
        }
        Cow::Owned(history)
    }

    pub(super) fn evicted_tokens(&self, messages: Range<usize>, counted: u64) -> u64 {
        self.evicted
            .range((messages.start, 0)..(messages.end, 0))
            .fold(counted, |sum, (_, kept)| {
                sum.saturating_add(kept.tokens).saturating_sub(kept.held)
            })
    }

    #[cfg(test)]
    pub(crate) fn evicted_count(&self) -> usize {
        self.evicted.len()
    }

    #[cfg(test)]
    pub(super) fn nothing_evicted_in_view(&self) -> bool {
        self.evicted
            .keys()
            .all(|(message, _)| *message < self.view_from())
    }
}
