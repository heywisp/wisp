use jiff::Timestamp;
use tokio::sync::mpsc;
use wisp_protocol::log::LogEvent;
use wisp_protocol::{AgentEvent, CacheTtl, Effort};

use super::Agent;
use crate::context::{Anchor, blocks_tokens, ttl_span};
use crate::provider::TailMarker;

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct Entry {
    pub(super) turn: u64,
    pub(super) seq: Option<u64>,
    pub(super) view_len: usize,
    pub(super) prompt_tokens: u64,
    pub(super) used: u64,
    pub(super) output_tokens: u64,
    pub(super) started_at: Timestamp,
    pub(super) ttl: CacheTtl,
    pub(super) model: String,
    pub(super) endpoint: usize,
    pub(super) prefix: Option<String>,
    pub(super) refreshed_at: Option<Timestamp>,
}

impl Entry {
    pub(super) fn used_at(&self) -> Timestamp {
        self.refreshed_at
            .map_or(self.started_at, |at| at.max(self.started_at))
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum MarkReason {
    EndpointSwitch,
    ModelSwitch,
    EffortChange,
    PerMessageEffortRejected,
    BindingControlsChange,
    NativeSearchRefused,
    ToolsChange,
    SystemChange,
    TailClosed,
    ThinkingCut,
    RenderingForm,
    SearchHistoryUnreadable,
    SearchHistoryRendered,
    AttachmentsAsNotes,
}

impl MarkReason {
    pub(super) fn as_str(self) -> &'static str {
        match self {
            Self::EndpointSwitch => "endpoint_switch",
            Self::ModelSwitch => "model_switch",
            Self::EffortChange => "effort_change",
            Self::PerMessageEffortRejected => "per_message_effort_rejected",
            Self::BindingControlsChange => "binding_controls_change",
            Self::NativeSearchRefused => "native_search_refused",
            Self::ToolsChange => "tools_change",
            Self::SystemChange => "system_change",
            Self::TailClosed => "tail_closed",
            Self::ThinkingCut => "thinking_cut",
            Self::RenderingForm => "rendering_form",
            Self::SearchHistoryUnreadable => "search_history_unreadable",
            Self::SearchHistoryRendered => "search_history_rendered",
            Self::AttachmentsAsNotes => "attachments_as_notes",
        }
    }

    pub(super) fn changes_bytes(self) -> bool {
        wisp_protocol::log::BYTE_REASONS.contains(&self.as_str())
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct Logged {
    pub(super) view_len: usize,
    pub(super) started_at: Timestamp,
    pub(super) ttl: CacheTtl,
    pub(super) prefix: Option<String>,
    pub(super) betas: Vec<String>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(super) struct Epoch {
    entries: Vec<Entry>,
    mark: usize,
    sending: Option<(Option<String>, Vec<String>, PrefixKey)>,
    sent: Option<PrefixKey>,
    unlogged_mark: Option<MarkReason>,
    unlogged_entry: Option<Logged>,
    bytes_changed: bool,
    expected: Option<&'static str>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct EpochAt {
    anchor: Option<Entry>,
    sent: Option<PrefixKey>,
}

impl EpochAt {
    pub(super) fn resumed(anchor: Option<Entry>) -> Self {
        Self { anchor, sent: None }
    }
}

impl Epoch {
    pub(super) fn at(&self) -> EpochAt {
        EpochAt {
            anchor: self.after_mark().last().cloned(),
            sent: self.sent.clone(),
        }
    }

    pub(super) fn restore_at(&mut self, at: EpochAt) {
        *self = Self {
            entries: at.anchor.into_iter().collect(),
            sent: at.sent,
            ..Self::default()
        };
    }

    pub(super) fn bytes_changed(&self) -> bool {
        self.bytes_changed
    }

    pub(super) fn sending(&mut self, prefix: Option<String>, betas: Vec<String>, key: PrefixKey) {
        self.sending = Some((prefix, betas, key));
    }

    pub(super) fn take_unsent_betas(&mut self) -> Option<Vec<String>> {
        self.sending.take().map(|(_, betas, _)| betas)
    }

    pub(super) fn sent(&self) -> Option<&PrefixKey> {
        self.sent.as_ref()
    }

    pub(super) fn entry_of(&self, turn: u64, view_len: usize) -> Option<&Entry> {
        self.entries
            .iter()
            .rev()
            .find(|e| e.turn == turn && e.view_len == view_len)
    }

    pub(super) fn refreshed(&mut self, turn: u64, view_len: usize, at: Timestamp) {
        if let Some(entry) = self
            .entries
            .iter_mut()
            .rev()
            .find(|e| e.turn == turn && e.view_len == view_len)
        {
            entry.refreshed_at = Some(at);
        }
    }

    pub(super) fn sent_same_view(&self, key: &PrefixKey) -> bool {
        self.sent.as_ref().is_some_and(|sent| sent.same_view(key))
    }

    fn mark_if_changed(&mut self, key: &PrefixKey) {
        if let Some(reason) = self.sent.as_ref().and_then(|sent| sent.change_to(key)) {
            self.rebuilt(reason);
        }
    }

    fn committed(&mut self, mut entry: Entry) {
        let mut betas = Vec::new();
        if let Some((prefix, sent_betas, key)) = self.sending.take() {
            entry.prefix = prefix;
            betas = sent_betas;
            self.sent = Some(key);
        }
        self.bytes_changed = false;
        self.unlogged_entry = Some(Logged {
            view_len: entry.view_len,
            started_at: entry.started_at,
            ttl: entry.ttl,
            prefix: entry.prefix.clone(),
            betas,
        });
        self.entries.push(entry);
    }

    pub(super) fn take_logged_entry(&mut self) -> Option<Logged> {
        self.unlogged_entry.take()
    }

    pub(super) fn logged_at(&mut self, seq: Option<u64>) {
        if let Some(entry) = self.entries.last_mut()
            && entry.seq.is_none()
        {
            entry.seq = seq;
        }
    }

    pub(super) fn take_logged_mark(&mut self) -> Option<MarkReason> {
        self.unlogged_mark.take()
    }

    #[cfg(test)]
    pub(super) fn restored(entries: Vec<Entry>, mark: usize) -> Self {
        Self {
            mark: mark.min(entries.len()),
            entries,
            ..Self::default()
        }
    }

    pub(super) fn entries(&self) -> &[Entry] {
        &self.entries
    }

    pub(super) fn mark(&self) -> usize {
        self.mark
    }

    pub(super) fn earlier(
        &self,
        fits: Fits<'_>,
        estimate: impl Fn(&Entry) -> u64,
    ) -> Option<usize> {
        let before = fits.before.min(self.entries.len());
        self.entries.get(..before)?.iter().rposition(|e| {
            let tokens = match e.prompt_tokens {
                0 => estimate(e),
                known => known,
            };
            counted(tokens, &e.model, fits.model).saturating_add(fits.room) <= fits.limit
        })
    }

    #[cfg(test)]
    pub(super) fn push(&mut self, entry: Entry) {
        self.entries.push(entry);
    }

    #[cfg(test)]
    pub(super) fn marked_at(&self) -> (usize, usize) {
        (self.mark, self.entries.len())
    }

    pub(super) fn rebuilt(&mut self, reason: MarkReason) {
        if self.mark != self.entries.len() {
            self.mark = self.entries.len();
            self.unlogged_mark = Some(reason);
            self.bytes_changed |= reason.changes_bytes();
            self.expected = Some(reason.as_str());
        }
    }

    pub(super) fn expect(&mut self, reason: &'static str) {
        self.expected = Some(reason);
    }

    pub(super) fn take_expected(&mut self) -> Option<&'static str> {
        self.expected.take()
    }

    pub(super) fn expected(&self) -> Option<&'static str> {
        self.expected
    }

    pub(super) fn clear(&mut self) {
        *self = Self::default();
    }

    pub(super) fn changed_from(&mut self, message: usize, reason: MarkReason) {
        if self.after_mark().iter().any(|e| e.view_len > message) {
            self.rebuilt(reason);
        }
    }

    fn after_mark(&self) -> &[Entry] {
        self.entries.get(self.mark..).unwrap_or_default()
    }

    pub(super) fn anchor(&self, endpoint: usize, model: &str) -> Option<&Entry> {
        self.after_mark()
            .last()
            .filter(|e| e.endpoint == endpoint && e.model == model)
    }

    pub(super) fn tail_marker(
        &self,
        endpoint: usize,
        model: &str,
        now: Timestamp,
    ) -> Option<TailMarker> {
        let entry = self
            .anchor(endpoint, model)
            .filter(|e| now.duration_since(e.used_at()) < ttl_span(e.ttl))?;
        Some(TailMarker {
            view_len: entry.view_len,
            ttl: entry.ttl,
            prefix: entry.prefix.clone()?,
            last_blocks: None,
        })
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) struct Fits<'a> {
    pub(super) limit: u64,
    pub(super) room: u64,
    pub(super) model: &'a str,
    pub(super) before: usize,
}

pub(super) const OTHER_MODEL_PERCENT: u64 = 135;

pub(super) fn counted(tokens: u64, sent_on: &str, model: &str) -> u64 {
    if sent_on == model {
        return tokens;
    }
    tokens.saturating_mul(OTHER_MODEL_PERCENT).div_ceil(100)
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct PrefixKey {
    endpoint: usize,
    model: String,
    effort: Effort,
    system: String,
    tools: String,
    server_tools: String,
    per_message_effort: bool,
    binding_controls: bool,
}

impl PrefixKey {
    pub(super) fn same_bytes(&self, other: &Self) -> bool {
        self.system == other.system
            && self.tools == other.tools
            && self.server_tools == other.server_tools
    }

    pub(super) fn same_view(&self, other: &Self) -> bool {
        self.endpoint == other.endpoint
            && self.model == other.model
            && self.system == other.system
            && self.tools == other.tools
            && self.server_tools == other.server_tools
    }

    fn change_to(&self, next: &Self) -> Option<MarkReason> {
        let changes = [
            (self.endpoint != next.endpoint, MarkReason::EndpointSwitch),
            (self.model != next.model, MarkReason::ModelSwitch),
            (
                self.per_message_effort != next.per_message_effort,
                MarkReason::PerMessageEffortRejected,
            ),
            (
                self.binding_controls != next.binding_controls,
                MarkReason::BindingControlsChange,
            ),
            (self.effort != next.effort, MarkReason::EffortChange),
            (self.tools != next.tools, MarkReason::ToolsChange),
            (
                self.server_tools != next.server_tools,
                MarkReason::NativeSearchRefused,
            ),
            (self.system != next.system, MarkReason::SystemChange),
        ];
        changes
            .into_iter()
            .find_map(|(changed, reason)| changed.then_some(reason))
    }
}

fn digest(value: &impl serde::Serialize) -> String {
    crate::session::sha256_json_hex(value).unwrap_or_else(|| crate::session::sha256_hex(""))
}

impl Agent {
    pub(super) fn prefix_key(&self) -> PrefixKey {
        let (session, features) = self.request_parts();
        PrefixKey {
            endpoint: self.endpoint,
            system: digest(&(&session.system, &session.instructions)),
            tools: match super::tool_changes::deferred_for(&self.deferred, features) {
                [] => digest(&self.sent),
                deferred => digest(&crate::tool::array_json(&self.sent, deferred, &[])),
            },
            server_tools: digest(&self.step_server_tools()),
            model: session.model,
            effort: session.effort,
            per_message_effort: features.per_message_effort.is_some(),
            binding_controls: features.binding_controls.is_some(),
        }
    }

    pub(super) fn check_prefix(&mut self) -> PrefixKey {
        let key = self.prefix_key();
        self.epoch.mark_if_changed(&key);
        key
    }

    pub(super) async fn log_mark(&mut self, events: &mpsc::Sender<AgentEvent>) {
        if let Some(reason) = self.epoch.take_logged_mark() {
            let turn = self.running_turn().unwrap_or(self.turns.saturating_add(1));
            let record = LogEvent::RebuildMarked {
                turn,
                reason: reason.as_str().to_owned(),
            };
            self.log(events, record).await;
        }
    }

    #[cfg(test)]
    pub(super) fn restore_requests(
        &mut self,
        list: wisp_protocol::log::RequestList,
        records: &[wisp_protocol::log::LogRecord],
    ) {
        let mut entries = Vec::new();
        let mut mark = 0;
        for (index, logged) in list.entries.iter().enumerate() {
            let Some(entry) = self.logged_entry(records, logged) else {
                continue;
            };
            if index < list.mark {
                mark += 1;
            }
            entries.push(entry);
        }
        self.epoch = Epoch::restored(entries, mark);
    }

    pub(super) fn note_committed(&mut self, prefix: Option<String>) {
        let Some(last) = &self.last else {
            return;
        };
        let (Some(started_at), Some(ttl)) = (last.started_at, last.ttl) else {
            return;
        };
        let entry = Entry {
            turn: last.turn,
            seq: None,
            view_len: last.messages_len,
            prompt_tokens: last.prompt_tokens,
            used: last.used,
            output_tokens: last.output_tokens,
            started_at,
            ttl,
            model: last.asked.clone(),
            endpoint: last.endpoint,
            prefix,
            refreshed_at: None,
        };
        self.epoch.committed(entry);
    }

    pub(super) fn compaction_anchor(&self) -> (Option<Anchor>, u64) {
        let Some(entry) = self.epoch.anchor(self.endpoint, &self.session.model) else {
            return (None, 0);
        };
        let since: Vec<_> = self
            .history
            .iter()
            .skip(self.history_len(entry.view_len).saturating_add(1))
            .flat_map(|m| m.content.iter().cloned())
            .collect();
        let anchor = Anchor {
            used: entry.used,
            output_tokens: entry.output_tokens,
            started_at: Some(entry.used_at()),
            ttl: Some(entry.ttl),
            same_pair: true,
        };
        (Some(anchor), blocks_tokens(&since))
    }

    pub(super) fn tail_marker(&self) -> Option<TailMarker> {
        self.epoch
            .tail_marker(self.endpoint, &self.session.model, self.clock.now())
    }
}

#[cfg(test)]
mod tests;
