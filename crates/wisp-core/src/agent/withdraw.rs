use std::collections::BTreeMap;

use tokio::sync::mpsc;
use wisp_plugin_api::types::SliceKind;
use wisp_protocol::log::{BlockPosition, LogEvent, MediaSlot, WithdrawnBlock, media_slots};
use wisp_protocol::{
    AgentEvent, AttachmentKind, ContentBlock, FileSlice, HistoryChange, ImageSource, Message, Mime,
    Role, WithdrawnAttachment,
};

use super::epoch::MarkReason;
use super::images::{self, Media, history_media};
use super::{Agent, Pending};
use crate::provider::Rendering;
use crate::session::base64_sha256;
use crate::tool::SHELL;

pub(crate) const MAX_REASON_CHARS: usize = 200;

pub(super) struct Withdrawal {
    step: u32,
    from: usize,
    history: Vec<Message>,
    user_media: Vec<(usize, ContentBlock)>,
    after_media: Vec<(usize, ContentBlock)>,
    items: Vec<(WithdrawnAttachment, String)>,
    pending: Vec<usize>,
}

fn non_text_blocks(blocks: &[ContentBlock]) -> Vec<(usize, ContentBlock)> {
    blocks
        .iter()
        .enumerate()
        .filter(|(_, block)| !matches!(block, ContentBlock::Text { .. }))
        .map(|(index, block)| (index, block.clone()))
        .collect()
}

pub(super) fn end_of(messages: &[Message]) -> BlockPosition {
    match messages.last() {
        Some(last) => BlockPosition {
            message: messages.len() - 1,
            block: last.content.len(),
        },
        None => BlockPosition {
            message: 0,
            block: 0,
        },
    }
}

fn start_in(index: usize, message: &Message, from: BlockPosition) -> usize {
    if index == from.message {
        from.block.min(message.content.len())
    } else {
        0
    }
}

fn holds_media(blocks: &[ContentBlock]) -> bool {
    history_media(blocks.iter()) != Media::default()
}

pub(super) fn provisional_media(messages: &[Message], from: BlockPosition) -> bool {
    messages
        .iter()
        .enumerate()
        .skip(from.message)
        .filter(|(_, m)| m.role == Role::User)
        .any(|(i, m)| holds_media(&m.content[start_in(i, m, from)..]))
}

pub(super) fn first_media(messages: &[Message]) -> Option<usize> {
    messages.iter().position(|m| holds_media(&m.content))
}

fn region<'a>(messages: &'a mut [Message], from: BlockPosition, out: &mut Vec<MediaSlot<'a>>) {
    for (i, message) in messages.iter_mut().enumerate().skip(from.message) {
        if message.role != Role::User {
            continue;
        }
        let start = start_in(i, message, from);
        media_slots(&mut message.content[start..], out);
    }
}

pub(crate) fn one_line_reason(reason: &str) -> String {
    let visible: String = reason
        .chars()
        .map(|c| if c.is_control() { ' ' } else { c })
        .collect();
    let flat = visible.split_whitespace().collect::<Vec<_>>().join(" ");
    let flat = if flat.is_empty() {
        "the API refused it".to_owned()
    } else {
        flat
    };
    if flat.chars().count() <= MAX_REASON_CHARS {
        return flat;
    }
    let mut cut: String = flat.chars().take(MAX_REASON_CHARS - 1).collect();
    cut.push('\u{2026}');
    cut
}

pub(crate) fn withdrawal_note(kind: AttachmentKind, label: &str, reason: &str) -> String {
    format!("[{} {label} couldn't be processed: {reason}]", kind.noun())
}

pub(crate) fn file_note(
    label: &str,
    media_type: &Mime,
    slice: Option<FileSlice>,
    reason: &str,
    others: usize,
    shell: Option<&str>,
) -> String {
    let reason = reason.strip_suffix('.').unwrap_or(reason);
    let noun = if others == 1 {
        "attachment"
    } else {
        "attachments"
    };
    let file = match slice {
        Some(slice) => format!("{label} ({media_type}, {slice})"),
        None => format!("{label} ({media_type})"),
    };
    if others == 0 && slice.is_some() {
        let advice = match shell {
            Some(shell) => format!("convert or cut it with {shell} and view that"),
            None => "omit the slice to read the whole file".to_owned(),
        };
        return format!(
            "[{file} couldn't be processed: {reason}. Check the requested range and try again, or {advice}.]"
        );
    }
    match (others, shell) {
        (0, None) => format!(
            "[{file} couldn't be processed: {reason}. It can't be read in this \
session.]"
        ),
        (0, Some(shell)) => format!(
            "[{file} couldn't be processed: {reason}. Convert or cut it with \
{shell} into something you can read, then view or read that.]"
        ),
        (_, None) => format!(
            "[{file} was withdrawn with {others} other {noun} when the API \
refused this step: {reason}. It may be fine: view it again on its own.]"
        ),
        (_, Some(shell)) => format!(
            "[{file} was withdrawn with {others} other {noun} when the API refused \
this step: {reason}. It may be fine: view it again on its own, and convert it with {shell} only \
if that is refused too.]"
        ),
    }
}

struct Found {
    ordinal: usize,
    call_id: Option<String>,
    kind: AttachmentKind,
    label: String,
    media_type: Option<Mime>,
    slice: Option<FileSlice>,
    sha256: String,
}

fn found(
    ordinal: usize,
    slot: &MediaSlot<'_>,
    targets: &BTreeMap<String, String>,
) -> Option<Found> {
    let call_id = slot.call_id.map(str::to_owned);
    let number = format!("#{}", slot.number);
    let (kind, label, media_type, slice, data) = match &*slot.block {
        ContentBlock::Image {
            source: ImageSource::Base64 { data, .. },
        } => (AttachmentKind::Image, number, None, None, &**data),
        ContentBlock::Media { source, slice } => {
            let label = slot
                .call_id
                .and_then(|id| targets.get(id))
                .cloned()
                .unwrap_or(number);
            let media_type = Some(source.media_type().clone());
            (
                AttachmentKind::File,
                label,
                media_type,
                *slice,
                source.data(),
            )
        }
        _ => return None,
    };
    Some(Found {
        ordinal,
        call_id,
        kind,
        label,
        media_type,
        slice,
        sha256: base64_sha256(data),
    })
}

fn pending_slots<'a>(
    blocks: &'a mut [ContentBlock],
    start: usize,
    out: &mut Vec<(bool, MediaSlot<'a>)>,
) {
    let (carried, open) = blocks.split_at_mut(start);
    let mut slots = Vec::new();
    media_slots(carried, &mut slots);
    out.extend(slots.into_iter().map(|slot| (false, slot)));
    let mut slots = Vec::new();
    media_slots(open, &mut slots);
    out.extend(slots.into_iter().map(|slot| (true, slot)));
}

fn pending_start(at: BlockPosition, len: usize, accepted: BlockPosition) -> usize {
    match accepted.message.cmp(&at.message) {
        std::cmp::Ordering::Less => 0,
        std::cmp::Ordering::Equal => accepted.block.saturating_sub(at.block).min(len),
        std::cmp::Ordering::Greater => len,
    }
}

fn pending_positions(
    history: &[Message],
    pending: &Pending,
    fresh: bool,
) -> (BlockPosition, BlockPosition) {
    let user_len = pending.user.as_ref().map_or(0, Vec::len);
    let user = match history.last() {
        Some(last) if last.role == Role::User && !fresh => BlockPosition {
            message: history.len() - 1,
            block: last.content.len(),
        },
        _ => BlockPosition {
            message: history.len(),
            block: 0,
        },
    };
    let after = if pending.has_system() {
        let system_message = if pending.user.is_some() {
            user.message + 1
        } else {
            history.len()
        };
        BlockPosition {
            message: system_message + 1,
            block: 0,
        }
    } else {
        BlockPosition {
            message: user.message,
            block: user.block + user_len,
        }
    };
    (user, after)
}

impl Agent {
    pub(super) fn kept_notes(&self) -> bool {
        match self.provider().capabilities(&self.session.model) {
            Some(row) => row.images.is_none(),
            None => self.search.notes.contains(&self.notes_pair()),
        }
    }

    pub(super) fn sends_notes(&self) -> bool {
        self.search.notes_trial || self.kept_notes()
    }

    pub(super) fn learns_notes(&self) -> bool {
        !self.sends_notes() && self.provider().capabilities(&self.session.model).is_none()
    }

    pub(super) fn carried_media(&self) -> &'static [&'static str] {
        if self.sends_notes() {
            &[]
        } else {
            self.provider().media_types()
        }
    }

    pub(super) fn carried_slices(&self) -> &'static [SliceKind] {
        if self.sends_notes() {
            return &[];
        }
        self.provider()
            .capabilities(&self.session.model)
            .map_or(&[], |caps| caps.slices)
    }

    pub(super) fn sent_sound(&self, messages: &[Message], rendering: &Rendering) -> bool {
        images::sent_sound(
            messages,
            rendering,
            self.carried_media(),
            self.carried_slices(),
        )
    }

    fn notes_pair(&self) -> (String, String) {
        (self.endpoint_name().to_owned(), self.session.model.clone())
    }

    pub(super) async fn keep_notes(
        &mut self,
        messages: &[Message],
        rendering: &Rendering,
        events: &mpsc::Sender<AgentEvent>,
    ) {
        let pair = self.notes_pair();
        let Some(first) = first_media(messages).filter(|_| rendering.attachments_as_notes) else {
            return;
        };
        if !self.search.notes.insert(pair.clone()) {
            return;
        }
        let (endpoint_name, model) = pair;
        let media = history_media(messages.iter().flat_map(|m| m.content.iter()));
        let record = LogEvent::AttachmentsAsNotes {
            endpoint_name,
            model: model.clone(),
        };
        self.log(events, record).await;
        let change = HistoryChange::AttachmentsAsNotes {
            model,
            images: media.images,
            files: media.files,
        };
        self.adapted(change, events).await;
        self.mark_changed_from(first, MarkReason::AttachmentsAsNotes);
        self.log_mark(events).await;
    }

    fn call_targets(&self) -> BTreeMap<String, String> {
        let held = crate::session::held();
        self.calls()
            .map(|(id, name, input)| (id.to_owned(), self.shown_target(id, name, &input, &held)))
            .collect()
    }

    pub(super) fn withdraw(&mut self, pending: &mut Pending, reason: &str, step: u32) -> bool {
        let from = self.accepted.min(end_of(&self.history));
        let kept_from = from.message.min(self.history.len());
        let mut saved = Withdrawal {
            step,
            from: kept_from,
            history: self.history[kept_from..].to_vec(),
            user_media: non_text_blocks(pending.user.as_deref().unwrap_or_default()),
            after_media: non_text_blocks(&pending.after),
            items: Vec::new(),
            pending: Vec::new(),
        };
        let reason = one_line_reason(reason);
        let targets = self.call_targets();
        let (user_at, after_at) = pending_positions(&self.history, pending, self.at_head());
        let accepted = self.accepted;
        let unwithdrawn: Vec<usize> = {
            let track = self.track();
            (0..track.pending_images.len())
                .filter(|i| !track.withdrawn_images.contains(i))
                .collect()
        };
        let mut held = Vec::new();
        region(&mut self.history, from, &mut held);
        let mut present = Vec::new();
        if let Some(user) = pending.user.as_mut() {
            let start = pending_start(user_at, user.len(), accepted);
            pending_slots(user, start, &mut present);
        }
        let start = pending_start(after_at, pending.after.len(), accepted);
        pending_slots(&mut pending.after, start, &mut present);
        if present.len() != unwithdrawn.len() {
            return false;
        }
        let first = held.len();
        let open = present
            .into_iter()
            .zip(unwithdrawn)
            .filter(|((fresh, _), _)| *fresh)
            .map(|((_, slot), index)| (first + index, Some(index), slot));
        let slots = held
            .into_iter()
            .enumerate()
            .map(|(ordinal, slot)| (ordinal, None, slot))
            .chain(open);
        let found: Vec<(MediaSlot<'_>, Option<usize>, Found)> = slots
            .filter_map(|(ordinal, index, slot)| {
                let found = found(ordinal, &slot, &targets)?;
                Some((slot, index, found))
            })
            .collect();
        let others = found.len().saturating_sub(1);
        let shell = self.toolbox.provider_of(SHELL);
        for (slot, index, found) in found {
            let note = match &found.media_type {
                Some(media_type) => file_note(
                    &found.label,
                    media_type,
                    found.slice,
                    &reason,
                    others,
                    shell,
                ),
                None => withdrawal_note(found.kind, &found.label, &reason),
            };
            let item = WithdrawnAttachment {
                call_id: found.call_id,
                ordinal: found.ordinal,
                kind: found.kind,
                label: found.label,
                reason: reason.clone(),
                note: note.clone(),
            };
            *slot.block = ContentBlock::Text { text: note };
            saved.items.push((item, found.sha256));
            saved.pending.extend(index);
        }
        if saved.items.is_empty() {
            return false;
        }
        self.withdrawal = Some(saved);
        self.sync_history();
        true
    }

    pub(super) fn restore(&mut self, pending: &mut Pending) -> bool {
        let Some(saved) = self.withdrawal.take() else {
            return false;
        };
        self.history.truncate(saved.from);
        self.history.extend(saved.history);
        if let Some(user) = pending.user.as_mut() {
            for (index, block) in saved.user_media {
                if let Some(slot) = user.get_mut(index) {
                    *slot = block;
                }
            }
        }
        for (index, block) in saved.after_media {
            if let Some(slot) = pending.after.get_mut(index) {
                *slot = block;
            }
        }
        self.sync_history();
        true
    }

    pub(super) async fn accept(&mut self, messages: &[Message], events: &mpsc::Sender<AgentEvent>) {
        self.accepted = self.to_history(end_of(messages));
        self.track().step_accepted = true;
        let Some(saved) = self.withdrawal.take() else {
            return;
        };
        let turn = {
            let track = self.track();
            track.withdrawn_images.extend(saved.pending);
            track.index
        };
        let (items, blocks): (Vec<_>, Vec<_>) = saved
            .items
            .into_iter()
            .map(|(item, sha256)| {
                let block = WithdrawnBlock {
                    ordinal: item.ordinal,
                    sha256,
                    note: item.note.clone(),
                };
                (item, block)
            })
            .unzip();
        let record = LogEvent::AttachmentsWithdrawn {
            turn,
            step: saved.step,
            items: blocks,
        };
        self.log(events, record).await;
        let event = AgentEvent::AttachmentsWithdrawn {
            step: saved.step,
            items,
        };
        self.emit(events, event).await;
    }

    pub(super) fn settle_accepted(&mut self) {
        self.accepted = self.accepted.min(end_of(&self.history));
    }

    #[cfg(test)]
    pub(crate) fn resumed(
        mut self,
        rebuilt: wisp_protocol::log::Rebuilt,
        records: &[wisp_protocol::log::LogRecord],
    ) -> Self {
        self.accepted = match rebuilt.provisional {
            Some(message) => BlockPosition { message, block: 0 },
            None => end_of(&rebuilt.messages),
        };
        self.history = rebuilt.messages;
        self.evicted.clear();
        self.search.paused = rebuilt.paused;
        self.search.closed = rebuilt.closed;
        self.search.cuts = rebuilt.cuts;
        self.search.refused = rebuilt.search_refused;
        self.sync_history();
        self.restore_requests(rebuilt.requests, records);
        self
    }
}

#[cfg(test)]
mod tests;
