use std::collections::{BTreeMap, BTreeSet};

use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::settings::{LoggedChange, Withdrawal};
use crate::{
    AttachmentRef, BackgroundReason, BlobRef, BusyWait, CacheEffect, CacheTtl, CandidateInfo,
    CitedSource, CompactionEngine, CompactionFailure, CompactionTrigger, ContentBlock, Covers,
    DeliveryMode, DetachReason, Diagnostic, Effort, EffortMarker, ErrorDetail, ErrorKind, Fact,
    HookChange, HookOutcome, ImageType, InputId, InputTransformation, InstructionsSource,
    MediaSource, Message, MessageTime, NeedsInputKind, Placement, ProcessEnd, Question,
    QuestionEnd, RejectKind, RenderingForm, ReportedEnd, Resend, Role, SearchAction, SearchEnd,
    SourceInfo, StopReason, SwitchCompaction, TermSize, ToolDisplay, ToolOutcome, ToolOutputView,
    TurnOrigin, TurnOutcome, TurnStats, Typed, UnfinishedReason, Usage, WithdrawnBy,
    WorkspaceChange,
};
use crate::{Dropped, MissCause, RewindPoint, RewindReason, SourceChange};

pub const LOG_SCHEMA_VERSION: u16 = 42;

pub const PROVISIONAL_SINCE: u16 = 15;

const UNVERSIONED: u16 = 0;

mod rewind;

pub use rewind::{BYTE_REASONS, REWIND_NEEDS_RECORDS, RebuiltPoint, rewind_mask};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LoggedInput {
    pub id: InputId,
    pub text: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub images: Vec<ImageRef>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub note: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Slot {
    Image,
    Text,
    Note,
}

pub fn input_slots(images: usize, noted: bool) -> impl Iterator<Item = Slot> {
    std::iter::repeat_n(Slot::Image, images)
        .chain([Slot::Text])
        .chain(noted.then_some(Slot::Note))
}

pub fn input_blocks(
    note: Option<&str>,
    images: Vec<ContentBlock>,
    text: &str,
) -> Vec<ContentBlock> {
    let text_block = |text: &str| ContentBlock::Text {
        text: text.to_owned(),
    };
    let mut images = images.into_iter();
    input_slots(images.len(), note.is_some())
        .filter_map(|slot| match slot {
            Slot::Image => images.next(),
            Slot::Text => Some(text_block(text)),
            Slot::Note => note.map(text_block),
        })
        .collect()
}

fn is_false(b: &bool) -> bool {
    !*b
}

fn is_true(b: &bool) -> bool {
    *b
}

fn is_zero_usize(n: &usize) -> bool {
    *n == 0
}

fn is_zero_u64(n: &u64) -> bool {
    *n == 0
}

fn accepted_by_default() -> bool {
    true
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WithdrawnBlock {
    pub ordinal: usize,
    pub sha256: String,
    pub note: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ImageRef {
    pub sha256: String,
    pub media_type: ImageType,
    pub bytes: u64,
    pub width: u32,
    pub height: u32,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RebuildError {
    pub kind: DamageKind,
    pub turn: u64,
    pub sha256: String,
    pub reason: String,
    pub rewind_at: Option<u64>,
}

impl std::fmt::Display for RebuildError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        if self.sha256.is_empty() {
            return write!(f, "turn {}: {}", self.turn, self.reason);
        }
        write!(
            f,
            "turn {}: attachment {} can't be restored: {}",
            self.turn, self.sha256, self.reason
        )
    }
}

impl std::error::Error for RebuildError {}

pub fn no_images(image: &ImageRef) -> Result<String, String> {
    Err(format!("no image store to read {} from", image.sha256))
}

pub const NO_HASH: &str = "there is no hash function to check a withdrawn block with";

pub trait BlobSource {
    fn image(&mut self, image: &ImageRef) -> Result<String, String>;

    fn media(&mut self, blob: &BlobRef) -> Result<String, String> {
        Err(format!("no attachment store to read {} from", blob.sha256))
    }

    fn sha256(&mut self, _base64: &str) -> Result<String, String> {
        Err(NO_HASH.to_owned())
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct LoggedMessage {
    pub role: Role,
    pub content: Vec<LoggedBlock>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum StoredSource {
    Blob(BlobRef),
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum LoggedBlock {
    Media {
        source: StoredSource,
    },
    ToolResult {
        tool_use_id: String,
        content: Vec<LoggedBlock>,
        #[serde(default)]
        is_error: bool,
    },
    #[serde(untagged)]
    Inline(ContentBlock),
}

impl LoggedBlock {
    pub fn as_inline(&self) -> Option<&ContentBlock> {
        match self {
            Self::Inline(block) => Some(block),
            _ => None,
        }
    }
}

fn stored_blocks(
    blocks: &[ContentBlock],
    store: &mut impl FnMut(&MediaSource) -> Option<BlobRef>,
) -> Vec<LoggedBlock> {
    blocks
        .iter()
        .map(|block| match block {
            ContentBlock::Media { source } => match store(source) {
                Some(blob) => LoggedBlock::Media {
                    source: StoredSource::Blob(blob),
                },
                None => LoggedBlock::Inline(block.clone()),
            },
            ContentBlock::ToolResult {
                tool_use_id,
                content,
                is_error,
            } => LoggedBlock::ToolResult {
                tool_use_id: tool_use_id.clone(),
                content: stored_blocks(content, store),
                is_error: *is_error,
            },
            other => LoggedBlock::Inline(other.clone()),
        })
        .collect()
}

fn resolved_blocks(
    blocks: &[LoggedBlock],
    blobs: &mut impl BlobSource,
) -> Result<Vec<ContentBlock>, (String, String)> {
    blocks
        .iter()
        .map(|block| match block {
            LoggedBlock::Media {
                source: StoredSource::Blob(blob),
            } => blobs
                .media(blob)
                .map(|data| ContentBlock::media(blob.media_type.clone(), data))
                .map_err(|reason| (blob.sha256.clone(), reason)),
            LoggedBlock::ToolResult {
                tool_use_id,
                content,
                is_error,
            } => Ok(ContentBlock::ToolResult {
                tool_use_id: tool_use_id.clone(),
                content: resolved_blocks(content, blobs)?,
                is_error: *is_error,
            }),
            LoggedBlock::Inline(block) => Ok(block.clone()),
        })
        .collect()
}

impl LoggedMessage {
    pub fn stored(
        message: &Message,
        mut store: impl FnMut(&MediaSource) -> Option<BlobRef>,
    ) -> Self {
        Self {
            role: message.role,
            content: stored_blocks(&message.content, &mut store),
        }
    }

    pub fn resolve(&self, blobs: &mut impl BlobSource) -> Result<Message, (String, String)> {
        Ok(Message {
            role: self.role,
            content: resolved_blocks(&self.content, blobs)?,
        })
    }

    pub fn inline(&self) -> Option<Message> {
        self.resolve(&mut ImagesOnly(no_images)).ok()
    }

    pub fn effort_marker(&self) -> Option<EffortMarker> {
        self.inline_turn_system()
            .and_then(|message| message.effort_marker())
    }

    pub fn is_turn_system(&self) -> bool {
        self.inline_turn_system().is_some()
    }

    fn inline_turn_system(&self) -> Option<Message> {
        if self.role != Role::System {
            return None;
        }
        let message = self.inline()?;
        message.is_turn_system().then_some(message)
    }
}

impl From<Message> for LoggedMessage {
    fn from(message: Message) -> Self {
        Self::stored(&message, |_| None)
    }
}

pub struct ImagesOnly<F>(pub F);

impl<F: FnMut(&ImageRef) -> Result<String, String>> BlobSource for ImagesOnly<F> {
    fn image(&mut self, image: &ImageRef) -> Result<String, String> {
        (self.0)(image)
    }
}

pub struct MediaSlot<'a> {
    pub block: &'a mut ContentBlock,
    pub call_id: Option<&'a str>,
    pub number: usize,
}

pub fn media_slots<'a>(blocks: &'a mut [ContentBlock], out: &mut Vec<MediaSlot<'a>>) {
    slots_in(blocks, None, out);
}

fn slots_in<'a>(
    blocks: &'a mut [ContentBlock],
    call_id: Option<&'a str>,
    out: &mut Vec<MediaSlot<'a>>,
) {
    let mut number = 0;
    for block in blocks {
        match block {
            ContentBlock::Text { .. } => number = 0,
            ContentBlock::Image { .. } | ContentBlock::Media { .. } => {
                number += 1;
                out.push(MediaSlot {
                    block,
                    call_id,
                    number,
                });
            }
            ContentBlock::ToolResult {
                tool_use_id,
                content,
                ..
            } => {
                let id: &'a String = tool_use_id;
                slots_in(content, Some(id.as_str()), out);
            }
            _ => {}
        }
    }
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct ToolsCopy {
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub tool_definitions: Vec<Value>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub deferred_definitions: Vec<Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tool_context: Option<Box<ToolContext>>,
    #[serde(default, skip_serializing_if = "is_zero_usize")]
    pub history_len: usize,
}

impl ToolsCopy {
    pub fn new(tool_definitions: Vec<Value>, tool_context: Option<ToolContext>) -> Self {
        Self {
            tool_definitions,
            deferred_definitions: Vec::new(),
            tool_context: tool_context.map(Box::new),
            history_len: 0,
        }
    }

    #[must_use]
    pub fn with_deferred(mut self, deferred_definitions: Vec<Value>) -> Self {
        self.deferred_definitions = deferred_definitions;
        self
    }
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct PrefixCopy {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub system_prompt: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub system_prompt_sha256: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub system_base_sha256: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub tools: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tools_sha256: Option<String>,
    #[serde(flatten)]
    pub whole: ToolsCopy,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tools_on: Option<Vec<String>>,
}

pub const LEAD_ENVIRONMENT: &str = "workspace/environment";

pub const LEAD_SENT: &str = "time/sent";

pub const LEAD_FILL: &str = "context/fill";

pub const LEAD_PROCESSES: &str = "shell/processes";

pub const LEAD_TOOLS: &str = "core/tools";

pub const LEAD_DATA: &str = "core/data";

pub const SECTIONS_SINCE: u16 = 35;

pub const CONTINUED: [(&str, &str); 2] = [
    ("core/environment", LEAD_ENVIRONMENT),
    ("core/processes", LEAD_PROCESSES),
];

pub fn continued_name(name: &str) -> &str {
    CONTINUED
        .iter()
        .find(|(old, _)| *old == name)
        .map_or(name, |(_, new)| new)
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LeadEntry {
    pub by: String,
    pub text: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub snapshot: Option<String>,
    #[serde(default, skip_serializing_if = "is_false")]
    pub replay: bool,
    #[serde(default, skip_serializing_if = "is_false")]
    pub tail: bool,
}

impl LeadEntry {
    pub fn new(by: &str, text: impl Into<String>) -> Self {
        Self {
            by: by.to_owned(),
            text: text.into(),
            snapshot: None,
            replay: false,
            tail: false,
        }
    }

    #[must_use]
    pub fn with_snapshot(mut self, snapshot: impl Into<String>) -> Self {
        self.snapshot = Some(snapshot.into());
        self
    }

    #[must_use]
    pub fn replayed(mut self) -> Self {
        self.replay = true;
        self
    }

    fn read_back(mut self) -> Self {
        self.by = continued_name(&self.by).to_owned();
        if self.by == LEAD_PROCESSES {
            self.replay = true;
        }
        self
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SectionSnapshot {
    pub name: String,
    pub snapshot: String,
}

pub fn lead_text<'a>(entries: &'a [LeadEntry], by: &str) -> Option<&'a str> {
    entries.iter().find(|e| e.by == by).map(|e| e.text.as_str())
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(from = "LeadFields")]
pub struct TurnLead {
    #[serde(rename = "lead", default, skip_serializing_if = "Vec::is_empty")]
    pub entries: Vec<LeadEntry>,
}

impl TurnLead {
    pub fn new(entries: Vec<LeadEntry>) -> Self {
        Self { entries }
    }

    pub fn from_fields(
        context: Option<String>,
        stamp: Option<String>,
        context_note: Option<String>,
        processes: Option<String>,
    ) -> Self {
        let entries = [
            (LEAD_ENVIRONMENT, context),
            (LEAD_SENT, stamp),
            (LEAD_FILL, context_note),
            (LEAD_PROCESSES, processes),
        ]
        .into_iter()
        .filter_map(|(by, text)| text.map(|text| LeadEntry::new(by, text).read_back()))
        .collect();
        Self::new(entries)
    }

    pub fn text(&self, by: &str) -> Option<&str> {
        lead_text(&self.entries, by)
    }

    pub fn has(&self, by: &str) -> bool {
        self.text(by).is_some()
    }

    pub fn replays(&self) -> bool {
        self.entries.iter().any(|e| e.replay)
    }

    pub fn len(&self) -> usize {
        self.entries.len()
    }

    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    pub fn texts(&self) -> impl Iterator<Item = &str> {
        self.entries
            .iter()
            .filter(|e| !e.tail)
            .map(|e| e.text.as_str())
    }

    pub fn tail(&self) -> impl Iterator<Item = &str> {
        self.entries
            .iter()
            .filter(|e| e.tail)
            .map(|e| e.text.as_str())
    }

    pub fn leading(&self) -> usize {
        self.texts().count()
    }
}

#[derive(Deserialize)]
struct LeadFields {
    #[serde(default)]
    lead: Vec<LeadEntry>,
    #[serde(default)]
    context: Option<String>,
    #[serde(default)]
    stamp: Option<String>,
    #[serde(default)]
    context_note: Option<String>,
    #[serde(default)]
    processes: Option<String>,
}

impl From<LeadFields> for TurnLead {
    fn from(fields: LeadFields) -> Self {
        let lead = if fields.lead.is_empty() {
            Self::from_fields(
                fields.context,
                fields.stamp,
                fields.context_note,
                fields.processes,
            )
        } else {
            Self::new(fields.lead)
        };
        Self::new(lead.entries.into_iter().map(LeadEntry::read_back).collect())
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ToolContext {
    pub platform: String,
    pub capabilities: Vec<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FeatureState {
    Proven,
    FallenBack,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ThreadSend {
    Create,
    Continue,
    Replay,
    Dropped,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct LogRecord {
    pub schema_version: u16,
    pub seq: u64,
    pub at: String,
    #[serde(flatten)]
    pub event: LogEvent,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CredentialSource {
    Environment,
    EnvFile,
    Keyring,
    CredentialFile,
    Mode {
        billing: crate::Billing,
        source: Box<CredentialSource>,
    },
    #[serde(untagged)]
    Other(String),
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum LogEvent {
    SessionStarted {
        session_id: String,
        wisp_version: String,
        model: String,
        effort: Effort,
        cache_ttl: CacheTtl,
        system_prompt: Option<String>,
        system_prompt_sha256: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        system_base_sha256: Option<String>,
        endpoint: String,
        credential_source: CredentialSource,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        endpoint_name: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        message_time: Option<MessageTime>,
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        tools: Vec<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        tools_sha256: Option<String>,
        #[serde(flatten)]
        whole: ToolsCopy,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        cwd: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        output_dir: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        instructions: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        instructions_sha256: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        instructions_source: Option<InstructionsSource>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        instructions_extra: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        preset: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        context_note_enabled: Option<bool>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        debug_cache: Option<bool>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        refusal_fallback: Option<bool>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        parent: Option<Box<crate::agents::ParentLink>>,
        #[serde(default, skip_serializing_if = "std::ops::Not::not")]
        headless: bool,
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        plugins: Vec<crate::PluginRecord>,
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        sections: Vec<SectionSnapshot>,
    },
    TurnStarted {
        turn: u64,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        origin: Option<TurnOrigin>,
        #[serde(flatten)]
        lead: TurnLead,
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        ends: Vec<ReportedEnd>,
        #[serde(default, skip_serializing_if = "String::is_empty")]
        text: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        model: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        effort: Option<Effort>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        effort_marker: Option<EffortMarker>,
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        tool_message: Vec<ContentBlock>,
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        images: Vec<ImageRef>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        endpoint_name: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        input_id: Option<InputId>,
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        inputs: Vec<LoggedInput>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        cache_ttl: Option<CacheTtl>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        debug_cache: Option<bool>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        refusal_fallback: Option<bool>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        binding_controls: Option<bool>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        retry_of: Option<u64>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        kept_tools_sha256: Option<String>,
    },
    InputAccepted {
        turn: Option<u64>,
        id: InputId,
        mode: DeliveryMode,
        text: String,
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        images: Vec<ImageRef>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        replaces: Option<InputId>,
        #[serde(default, skip_serializing_if = "is_false")]
        held: bool,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        from: Option<crate::agents::MailFrom>,
    },
    InputHeld {
        turn: Option<u64>,
        ids: Vec<InputId>,
        by: WithdrawnBy,
    },
    InputUnheld {
        turn: Option<u64>,
        ids: Vec<InputId>,
    },
    ReturnedTaken {
        ids: Vec<InputId>,
    },
    InputCommitted {
        turn: u64,
        step: u32,
        inputs: Vec<LoggedInput>,
    },
    InputWithdrawn {
        turn: Option<u64>,
        ids: Vec<InputId>,
        by: WithdrawnBy,
    },
    InputDelivered {
        turn: u64,
        step: u32,
        ids: Vec<InputId>,
    },
    Retrying {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        turn: Option<u64>,
        attempt: u32,
        delay_ms: u64,
        reason: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        step: Option<u32>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        kind: Option<ErrorKind>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        detail: Option<ErrorDetail>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        usage: Option<Usage>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        partial_output: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        model: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        cost_usd: Option<f64>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        list_usd: Option<f64>,
        #[serde(default, skip_serializing_if = "is_false")]
        lower_bound: bool,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        resend: Option<Resend>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        busy: Option<BusyWait>,
    },
    StepEnded {
        turn: u64,
        step: u32,
        usage: Option<Usage>,
        cost_usd: Option<f64>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        list_usd: Option<f64>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        model: Option<String>,
        lower_bound: bool,
        prompt_tokens: Option<u64>,
        response_id: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        window: Option<u64>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        compact_at: Option<u64>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        used: Option<u64>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        first_pass: Option<u64>,
        #[serde(default = "accepted_by_default", skip_serializing_if = "is_true")]
        accepted: bool,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        view_len: Option<usize>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        started_at: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        ttl: Option<CacheTtl>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        prefix: Option<String>,
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        betas: Vec<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        queued_ms: Option<u64>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        thread: Option<ThreadSend>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        thread_error: Option<String>,
    },
    RebuildMarked {
        turn: u64,
        reason: String,
    },
    AttachmentsWithdrawn {
        turn: u64,
        step: u32,
        items: Vec<WithdrawnBlock>,
    },
    Fallback {
        turn: u64,
        from: String,
        to: String,
    },
    CacheMiss {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        turn: Option<u64>,
        previous_prompt_tokens: u64,
        cache_read_input_tokens: u64,
        cache_creation_input_tokens: u64,
        diagnostics: Option<Value>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        cause: Option<MissCause>,
    },
    CacheMissRepeated {
        #[serde(default, skip_serializing_if = "crate::Billing::is_metered")]
        billing: crate::Billing,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        turn: Option<u64>,
        cause: MissCause,
        count: u32,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        usd: Option<f64>,
    },
    CacheRefresh {
        turn: u64,
        outcome: crate::RefreshOutcome,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        detail: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        started_at: Option<String>,
        count: u32,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        usage: Option<Usage>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        cost_usd: Option<f64>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        list_usd: Option<f64>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        model: Option<String>,
        #[serde(default, skip_serializing_if = "is_false")]
        lower_bound: bool,
    },
    ModelAsked {
        call_id: String,
        plugin: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        turn: Option<u64>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        detail: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        usage: Option<Usage>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        cost_usd: Option<f64>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        list_usd: Option<f64>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        plan_usd: Option<f64>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        model: Option<String>,
        #[serde(default, skip_serializing_if = "is_false")]
        lower_bound: bool,
    },
    SessionNamed {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        name: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        detail: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        usage: Option<Usage>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        cost_usd: Option<f64>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        list_usd: Option<f64>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        model: Option<String>,
        #[serde(default, skip_serializing_if = "is_false")]
        lower_bound: bool,
    },
    VoiceCall {
        call: crate::voice::CallId,
        outcome: crate::voice::CallOutcome,
        seconds: u64,
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        lines: Vec<String>,
        #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
        unknown: BTreeMap<String, u32>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        endpoint_name: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        usage: Option<Usage>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        cost_usd: Option<f64>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        list_usd: Option<f64>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        model: Option<String>,
        #[serde(default, skip_serializing_if = "is_false")]
        lower_bound: bool,
    },
    UnknownProviderEvent {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        turn: Option<u64>,
        provider: String,
        event: Value,
    },
    InputTransformations {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        turn: Option<u64>,
        provider: String,
        transformations: Vec<InputTransformation>,
    },
    Rejected {
        turn: Option<u64>,
        reason: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        id: Option<InputId>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        kind: Option<RejectKind>,
    },
    WakeHeld {
        turn: u64,
        sources: Vec<crate::agents::WakeSource>,
        reason: crate::agents::HoldReason,
    },
    AgentRefused {
        turn: Option<u64>,
        by: crate::agents::AgentPath,
        action: crate::agents::AgentAction,
        reason: String,
    },
    AgentSpawned {
        turn: Option<u64>,
        agent: crate::agents::AgentPath,
        session_id: String,
        depth: u32,
        model: String,
        effort: Effort,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        cwd: Option<String>,
        config: crate::agents::SpawnChoices,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        endpoint: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        provider: Option<String>,
    },
    AgentEnded {
        turn: Option<u64>,
        agent: crate::agents::AgentPath,
        spent_usd: f64,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        plan_usd: Option<f64>,
        #[serde(default, skip_serializing_if = "crate::Billing::is_metered")]
        billing: crate::Billing,
    },
    AgentRenamed {
        turn: Option<u64>,
        agent: crate::agents::AgentPath,
        to: crate::agents::AgentPath,
    },
    AgentReported {
        turn: u64,
        agent: crate::agents::AgentPath,
        outcome: crate::agents::ReportOutcome,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        report: Option<Value>,
        duration_ms: u64,
    },
    MessageCommitted {
        turn: u64,
        message: LoggedMessage,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        model: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        stop_reason: Option<StopReason>,
    },
    TurnEnded {
        turn: u64,
        outcome: TurnOutcome,
        stats: TurnStats,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        partial_output: Option<String>,
    },
    ToolCallRequested {
        turn: u64,
        step: u32,
        block: usize,
        call_id: String,
        tool: String,
        input: Value,
        display: ToolDisplay,
    },
    ToolCallStarted {
        turn: u64,
        call_id: String,
        cwd: String,
        #[serde(default, skip_serializing_if = "std::ops::Not::not")]
        movable: bool,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        file: Option<String>,
    },
    ToolCallFinished {
        turn: u64,
        call_id: String,
        outcome: ToolOutcome,
        output: ToolOutputView,
        facts: Vec<Fact>,
        state: Option<WorkspaceChange>,
        duration_ms: u64,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        result: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        process: Option<u32>,
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        attachments: Vec<AttachmentRef>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        notes: Option<Vec<String>>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        typed: Option<Typed>,
    },
    QuestionAsked {
        turn: u64,
        call_id: String,
        questions: Vec<Question>,
    },
    QuestionClosed {
        turn: u64,
        call_id: String,
        how: QuestionEnd,
    },
    SessionEnded {
        turns: u64,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        interrupted: Option<u64>,
    },
    ImageLost {
        turn: u64,
        sha256: String,
        note: String,
    },
    ResumeFellBack {
        kept_through: Option<u64>,
        dropped_through: Option<u64>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        covered_through: Option<u64>,
        last_turn: Option<u64>,
        turns_seen: u64,
        processes_seen: u32,
        spent_usd: f64,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        plan_usd: Option<f64>,
        #[serde(default, skip_serializing_if = "crate::Billing::is_metered")]
        billing: crate::Billing,
        damage: Vec<Damage>,
    },
    SessionResumed {
        wisp_version: String,
        endpoint: String,
        endpoint_name: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        credential_source: Option<CredentialSource>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        system_prompt_sha256: Option<String>,
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        tools: Vec<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        tools_sha256: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        system_prompt: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        system_base_sha256: Option<String>,
        #[serde(flatten)]
        whole: ToolsCopy,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        tools_on: Option<Vec<String>>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        instructions_sha256: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        instructions_source: Option<InstructionsSource>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        instructions: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        instructions_extra: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        cwd: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        gap_ms: Option<u64>,
        #[serde(default, skip_serializing_if = "is_zero_u64")]
        truncated_bytes: u64,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        sections: Option<Vec<SectionSnapshot>>,
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        returned: Vec<InputId>,
    },
    SettingsChanged {
        model: String,
        effort: Effort,
        cache: CacheEffect,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        endpoint_name: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        provider: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        endpoint: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        credential_source: Option<CredentialSource>,
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        account_bound: Vec<String>,
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        tools: Vec<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        tools_sha256: Option<String>,
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        changes: Vec<LoggedChange>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        withdrawn: Option<Withdrawal>,
    },
    PerMessageEffort {
        model: String,
        state: FeatureState,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        endpoint_name: Option<String>,
    },
    BindingControls {
        model: String,
        state: FeatureState,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        endpoint_name: Option<String>,
    },
    NativeWebSearch {
        endpoint_name: String,
        state: FeatureState,
    },
    NativeCompaction {
        model: String,
        state: FeatureState,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        endpoint_name: Option<String>,
    },
    InlineTools {
        model: String,
        state: FeatureState,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        endpoint_name: Option<String>,
    },
    DeferredTools {
        model: String,
        state: FeatureState,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        endpoint_name: Option<String>,
    },
    PluginsChanged {
        turn: u64,
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        added: Vec<crate::PluginRecord>,
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        updated: Vec<crate::PluginRecord>,
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        removed: Vec<crate::PluginRecord>,
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        kept: Vec<crate::PluginRecord>,
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        notes: Vec<crate::plugin::PluginNote>,
    },
    ToolsChanged {
        turn: u64,
        via: crate::tool::ToolChangeVia,
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        added: Vec<String>,
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        removed: Vec<String>,
        #[serde(flatten)]
        copy: Box<PrefixCopy>,
    },
    ContextCompacted {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        plan_usd: Option<f64>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        turn: Option<u64>,
        epoch: u64,
        trigger: CompactionTrigger,
        engine: CompactionEngine,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        focus: Option<String>,
        before: u64,
        after: u64,
        #[serde(default, skip_serializing_if = "is_false")]
        partial: bool,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        covers: Option<Covers>,
        from: usize,
        head: Message,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        summary: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        block: Option<Value>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        usage: Option<Usage>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        cost_usd: Option<f64>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        list_usd: Option<f64>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        model: Option<String>,
        #[serde(default, skip_serializing_if = "is_false")]
        lower_bound: bool,
        duration_ms: u64,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        switch: Option<SwitchCompaction>,
        #[serde(default, skip_serializing_if = "Placement::is_head")]
        placement: Placement,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        cut: Option<usize>,
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        additions: Vec<ContentBlock>,
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        sections: Vec<SectionSnapshot>,
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        instructions_by: Vec<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        strategy: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        instruction: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        requested_by: Option<String>,
    },
    CompactionPrepared {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        plan_usd: Option<f64>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        turn: Option<u64>,
        epoch: u64,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        covers: Option<Covers>,
        block: Value,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        usage: Option<Usage>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        cost_usd: Option<f64>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        list_usd: Option<f64>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        model: Option<String>,
        #[serde(default, skip_serializing_if = "is_false")]
        lower_bound: bool,
        duration_ms: u64,
    },
    CompactionFailed {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        plan_usd: Option<f64>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        turn: Option<u64>,
        trigger: CompactionTrigger,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        engine: Option<CompactionEngine>,
        kind: CompactionFailure,
        message: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        usage: Option<Usage>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        cost_usd: Option<f64>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        list_usd: Option<f64>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        model: Option<String>,
        #[serde(default, skip_serializing_if = "is_false")]
        lower_bound: bool,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        switch: Option<SwitchCompaction>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        strategy: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        instruction: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        requested_by: Option<String>,
    },
    TailClosed {
        through: usize,
        calls: Vec<String>,
        reason: UnfinishedReason,
    },
    RenderingForm {
        form: RenderingForm,
    },
    ThinkingCut {
        from: BlockPosition,
        through: usize,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        tools_sha256: Option<String>,
    },
    SearchHistoryRendered {
        endpoint_name: String,
    },
    SearchHistoryUnreadable {
        endpoint_name: String,
    },
    SystemMarkerDropped {
        endpoint_name: String,
    },
    AttachmentsAsNotes {
        endpoint_name: String,
        model: String,
    },
    WebSearchNotRun {
        turn: u64,
        endpoint_name: String,
        model: String,
    },
    SearchStarted {
        turn: u64,
        step: u32,
        attempt: u32,
        call_id: String,
        query: String,
        #[serde(default, skip_serializing_if = "SearchAction::is_search")]
        action: SearchAction,
    },
    SearchEnded {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        turn: Option<u64>,
        step: u32,
        attempt: u32,
        call_id: String,
        outcome: SearchEnd,
    },
    Citations {
        turn: u64,
        step: u32,
        attempt: u32,
        index: usize,
        sources: Vec<CitedSource>,
    },
    InstructionsLoaded {
        turn: Option<u64>,
        chosen: Option<SourceInfo>,
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        imports: Vec<SourceInfo>,
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        candidates: Vec<CandidateInfo>,
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        diagnostics: Vec<Diagnostic>,
    },
    InstructionsChanged {
        turn: Option<u64>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        epoch: Option<u64>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        retry_of: Option<u64>,
        #[serde(default, skip_serializing_if = "is_false")]
        ahead: bool,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        from: Option<Box<SourceInfo>>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        to: Option<Box<SourceInfo>>,
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        changed: Vec<SourceChange>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        chosen: Option<Box<SourceInfo>>,
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        imports: Vec<SourceInfo>,
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        candidates: Vec<CandidateInfo>,
        sent: bool,
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        diagnostics: Vec<Diagnostic>,
        #[serde(default, skip_serializing_if = "String::is_empty")]
        text: String,
    },
    ProcessStarted {
        turn: u64,
        id: u32,
        call_id: String,
        reason: BackgroundReason,
        tty: bool,
        stdin: bool,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        size: Option<TermSize>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        file: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        pid: Option<u32>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        sid: Option<u32>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        start_ticks: Option<u64>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        boot_id: Option<String>,
        #[serde(default, skip_serializing_if = "is_false")]
        keeps_running: bool,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        log: Option<String>,
    },
    ProcessEnded {
        id: u32,
        end: ProcessEnd,
        bytes: u64,
        lines: u64,
        duration_ms: u64,
    },
    ProcessResized {
        id: u32,
        size: TermSize,
    },
    ProcessAttached {
        id: u32,
        size: TermSize,
    },
    ProcessDetached {
        id: u32,
        size: TermSize,
        reason: DetachReason,
        typed: bool,
        #[serde(default, skip_serializing_if = "is_false")]
        answered: bool,
    },
    ProcessHeard {
        turn: u64,
        ids: Vec<u32>,
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        typed: Vec<u32>,
    },
    ProcessNeedsInput {
        id: u32,
        kind: NeedsInputKind,
    },
    ProcessNeedsInputCleared {
        id: u32,
    },
    EndpointSaved {
        turn: Option<u64>,
        name: String,
        store: crate::endpoint::StoreKind,
        check: crate::endpoint::CheckResult,
    },
    EndpointNotSaved {
        turn: Option<u64>,
        name: String,
        why: crate::endpoint::NotSavedReason,
    },
    EndpointEdited {
        turn: Option<u64>,
        name: String,
        changed: Vec<crate::endpoint::ConnectionField>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        store: Option<crate::endpoint::StoreKind>,
        check: crate::endpoint::CheckResult,
    },
    EndpointRemoved {
        turn: Option<u64>,
        name: String,
        kept_item: bool,
        default_cleared: bool,
    },
    EndpointsChanged {
        turn: Option<u64>,
        endpoints: Vec<crate::settings::EndpointSummary>,
    },
    WindowLowered {
        endpoint: String,
        model: String,
        tokens: u64,
    },
    TodosUpdated {
        turn: u64,
        call_id: String,
        items: Vec<crate::TodoItem>,
    },
    Rewound {
        to: u64,
        point: RewindPoint,
        reason: RewindReason,
        messages: usize,
        blocks: usize,
        dropped: Dropped,
        cache: CacheEffect,
    },
    HookRan {
        hook: String,
        point: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        turn: Option<u64>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        step: Option<u32>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        call_id: Option<String>,
        outcome: HookOutcome,
        ms: u64,
    },
    HookApplied {
        hook: String,
        point: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        turn: Option<u64>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        step: Option<u32>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        call_id: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        input_id: Option<InputId>,
        change: HookChange,
    },
    #[serde(other, skip_serializing)]
    Unknown,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DamageKind {
    Unreadable,
    OutOfOrder,
    InvalidRecord,
    NoStart,
    Image,
    WithdrawnBlock,
    MalformedHistory,
    #[serde(other)]
    Unknown,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Damage {
    pub kind: DamageKind,
    pub seq: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub turn: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sha256: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub struct BlockPosition {
    pub message: usize,
    pub block: usize,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Closed {
    pub calls: BTreeSet<String>,
    pub tails: BTreeSet<usize>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Cut {
    pub from: BlockPosition,
    pub through: usize,
    pub tools: bool,
}

#[derive(Debug, Clone, PartialEq)]
pub struct LoggedHead {
    pub epoch: u64,
    pub from: usize,
    pub message: Message,
    pub placement: Placement,
    pub cut: usize,
    pub additions: Vec<ContentBlock>,
    pub join: Option<usize>,
}

impl LoggedHead {
    pub fn view(&self, history: &[Message]) -> Vec<Message> {
        match self.placement {
            Placement::Head => compacted_view(&self.message, self.from, history),
            Placement::Leading => {
                let at = KeptAt {
                    cut: self.cut,
                    from: self.from,
                    join: self.join,
                };
                kept_view(&self.message, at, &self.additions, history)
            }
        }
    }
}

impl Rebuilt {
    pub fn view(&self) -> Vec<Message> {
        match &self.head {
            Some(head) => head.view(&self.messages),
            None => self.messages.clone(),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct KeptAt {
    pub cut: usize,
    pub from: usize,
    pub join: Option<usize>,
}

pub fn kept_join(history: &[Message], cut: usize, from: usize) -> Option<usize> {
    if from <= cut {
        return None;
    }
    history
        .get(from - 1)
        .filter(|m| m.role == Role::User)
        .map(|m| m.content.len())
}

pub fn additions_at(history: &[Message], at: KeptAt) -> Option<(usize, usize)> {
    if let Some(join) = at.join
        && at.from > 0
    {
        return Some((at.from - 1, join));
    }
    history
        .iter()
        .enumerate()
        .skip(at.from)
        .find(|(_, m)| m.role != Role::System)
        .map(|(index, _)| (index, 0))
}

pub fn kept_view(
    lead: &Message,
    at: KeptAt,
    additions: &[ContentBlock],
    history: &[Message],
) -> Vec<Message> {
    let mut view = vec![lead.clone()];
    let place = (!additions.is_empty())
        .then(|| additions_at(history, at))
        .flatten();
    let mut placed = additions.is_empty();
    for (index, message) in history.iter().enumerate().skip(at.cut) {
        if let Some((target, start)) = place
            && index == target
        {
            placed = true;
            if message.role == Role::User {
                let start = start.min(message.content.len());
                let mut content = message.content[..start].to_vec();
                content.extend(additions.iter().cloned());
                content.extend(message.content[start..].iter().cloned());
                view.push(Message {
                    role: Role::User,
                    content,
                });
                continue;
            }
            view.push(Message {
                role: Role::User,
                content: additions.to_vec(),
            });
        }
        view.push(message.clone());
    }
    if !placed {
        view.push(Message {
            role: Role::User,
            content: additions.to_vec(),
        });
    }
    view
}

pub fn joins_head(history: &[Message], from: usize) -> bool {
    history.get(from).is_none_or(|m| m.role == Role::User)
}

pub fn compacted_view(head: &Message, from: usize, history: &[Message]) -> Vec<Message> {
    let mut view = vec![head.clone()];
    for message in history.iter().skip(from) {
        if message.role == Role::User {
            append_user(&mut view, message.content.clone());
        } else {
            view.push(message.clone());
        }
    }
    view
}

#[derive(Debug, Clone, Default, PartialEq)]
pub struct Rebuilt {
    pub head: Option<LoggedHead>,
    pub messages: Vec<Message>,
    pub paused: BTreeSet<usize>,
    pub closed: Closed,
    pub endpoints: Vec<Option<String>>,
    pub turns: Vec<u64>,
    pub sources: Vec<usize>,
    pub cuts: Vec<Cut>,
    pub provisional: Option<usize>,
    pub search_refused: BTreeSet<String>,
    pub requests: RequestList,
    pub points: Vec<RebuiltPoint>,
    pub restored: Option<Option<LoggedRequest>>,
    pub tool_notes: BTreeMap<usize, String>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct RequestList {
    pub entries: Vec<LoggedRequest>,
    pub mark: usize,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LoggedRequest {
    pub reply: Option<usize>,
    pub turn: u64,
    pub step: u32,
    pub view_len: Option<usize>,
    pub prompt_tokens: u64,
    pub used: u64,
    pub first_pass: Option<u64>,
    pub output_tokens: u64,
    pub cache_read: Option<u64>,
    pub started_at: String,
    pub ttl: CacheTtl,
    pub model: String,
    pub endpoint_name: Option<String>,
    pub prefix: Option<String>,
    pub response_id: Option<String>,
    pub served: Option<String>,
    pub effort: Option<Effort>,
    pub accepted: bool,
}

impl LoggedRequest {
    pub fn fills(
        prompt_tokens: u64,
        used: Option<u64>,
        first_pass: Option<u64>,
    ) -> (u64, Option<u64>) {
        (used.unwrap_or(prompt_tokens), first_pass.or(used))
    }
}

impl RequestList {
    fn baseline(&self, bytes_changed: bool) -> Option<LoggedRequest> {
        self.entries
            .iter()
            .rev()
            .find(|r| r.accepted)
            .filter(|_| !bytes_changed)
            .cloned()
    }

    fn clear(&mut self) {
        *self = Self::default();
    }

    fn marked(&mut self) {
        self.mark = self.entries.len();
    }
}

struct Part<'a> {
    images: &'a [ImageRef],
    text: &'a str,
    note: Option<&'a str>,
}

struct PendingTurn<'a> {
    turn: u64,
    lead: &'a TurnLead,
    parts: Vec<Part<'a>>,
    after: Vec<Part<'a>>,
}

struct Deferred<'a> {
    ordinal: usize,
    item: &'a WithdrawnBlock,
    turn: u64,
}

#[derive(Default)]
struct Withdrawals<'a> {
    items: Vec<Deferred<'a>>,
    seen: usize,
}

impl<'a> Withdrawals<'a> {
    fn new(items: Vec<Deferred<'a>>) -> Self {
        Self { items, seen: 0 }
    }

    fn next(&mut self, turn: u64, image: &ImageRef) -> Result<Option<&'a str>, RebuildError> {
        let ordinal = self.seen;
        self.seen += 1;
        let Some(at) = self.items.iter().position(|d| d.ordinal == ordinal) else {
            return Ok(None);
        };
        let deferred = self.items.remove(at);
        if deferred.item.sha256 != image.sha256 {
            return Err(mismatch(
                turn,
                deferred.item,
                format!(
                    "the withdrawal names image {}, but the message holds {} there",
                    deferred.item.sha256, image.sha256
                ),
            ));
        }
        Ok(Some(deferred.item.note.as_str()))
    }

    fn rest(self) -> Result<Vec<Deferred<'a>>, RebuildError> {
        let seen = self.seen;
        self.items
            .into_iter()
            .map(|d| match d.ordinal.checked_sub(seen) {
                Some(ordinal) => Ok(Deferred { ordinal, ..d }),
                None => Err(mismatch(
                    d.turn,
                    d.item,
                    format!(
                        "attachment {} of the message is withdrawn more than once",
                        d.ordinal
                    ),
                )),
            })
            .collect()
    }

    fn finish(self) -> Result<(), RebuildError> {
        match self.items.first() {
            Some(left) => Err(mismatch(
                left.turn,
                left.item,
                format!(
                    "the withdrawal names attachment {} of the message, which holds no such attachment",
                    left.ordinal
                ),
            )),
            None => Ok(()),
        }
    }
}

fn mismatch(turn: u64, item: &WithdrawnBlock, reason: String) -> RebuildError {
    RebuildError {
        kind: DamageKind::WithdrawnBlock,
        turn,
        sha256: item.sha256.clone(),
        reason,
        rewind_at: None,
    }
}

type LostImages<'a> = BTreeMap<(u64, &'a str), &'a str>;

fn noted(message: &LoggedMessage, turn: u64, lost: &LostImages<'_>) -> LoggedMessage {
    LoggedMessage {
        role: message.role,
        content: noted_blocks(&message.content, turn, lost),
    }
}

fn noted_blocks(blocks: &[LoggedBlock], turn: u64, lost: &LostImages<'_>) -> Vec<LoggedBlock> {
    blocks
        .iter()
        .map(|block| match block {
            LoggedBlock::Media {
                source: StoredSource::Blob(blob),
            } => match lost.get(&(turn, blob.sha256.as_str())) {
                Some(note) => LoggedBlock::Inline(ContentBlock::Text {
                    text: (*note).to_owned(),
                }),
                None => block.clone(),
            },
            LoggedBlock::ToolResult {
                tool_use_id,
                content,
                is_error,
            } => LoggedBlock::ToolResult {
                tool_use_id: tool_use_id.clone(),
                content: noted_blocks(content, turn, lost),
                is_error: *is_error,
            },
            LoggedBlock::Inline(_) => block.clone(),
        })
        .collect()
}

fn part_blocks(
    turn: u64,
    parts: &[Part<'_>],
    blobs: &mut impl BlobSource,
    withdrawals: &mut Withdrawals<'_>,
    lost: &LostImages<'_>,
) -> Result<Vec<ContentBlock>, RebuildError> {
    let mut blocks = Vec::new();
    for part in parts {
        let mut images = Vec::new();
        for r in part.images {
            if let Some(note) = withdrawals.next(turn, r)? {
                images.push(ContentBlock::Text {
                    text: note.to_owned(),
                });
                continue;
            }
            if let Some(note) = lost.get(&(turn, r.sha256.as_str())) {
                images.push(ContentBlock::Text {
                    text: (*note).to_owned(),
                });
                continue;
            }
            let data = blobs.image(r).map_err(|reason| RebuildError {
                kind: DamageKind::Image,
                turn,
                sha256: r.sha256.clone(),
                reason,
                rewind_at: None,
            })?;
            images.push(ContentBlock::image(r.media_type, data));
        }
        blocks.extend(input_blocks(part.note, images, part.text));
    }
    Ok(blocks)
}

fn parts(inputs: &[LoggedInput]) -> Vec<Part<'_>> {
    inputs
        .iter()
        .map(|i| Part {
            images: &i.images,
            text: &i.text,
            note: i.note.as_deref(),
        })
        .collect()
}

fn append_user(history: &mut Vec<Message>, blocks: Vec<ContentBlock>) {
    match history.last_mut() {
        Some(last) if last.role == Role::User => last.content.extend(blocks),
        _ => history.push(Message {
            role: Role::User,
            content: blocks,
        }),
    }
}

fn append_after(history: &mut Vec<Message>, blocks: Vec<ContentBlock>, boundary: Option<usize>) {
    if boundary == Some(history.len()) {
        history.push(Message {
            role: Role::User,
            content: blocks,
        });
    } else {
        append_user(history, blocks);
    }
}

#[derive(Default)]
struct Origins {
    endpoints: Vec<Option<String>>,
    turns: Vec<u64>,
    sources: Vec<usize>,
}

struct Origin<'a> {
    endpoint: Option<&'a str>,
    turn: u64,
    source: usize,
}

impl Origins {
    fn tag(&mut self, history: &[Message], origin: &Origin<'_>) {
        while self.endpoints.len() < history.len() {
            self.endpoints.push(origin.endpoint.map(str::to_owned));
            self.turns.push(origin.turn);
            self.sources.push(origin.source);
        }
    }
}

pub fn rebuild_history<'a>(
    events: impl IntoIterator<Item = &'a LogEvent>,
    image: impl FnMut(&ImageRef) -> Result<String, String>,
) -> Result<Rebuilt, RebuildError> {
    rebuild_history_with(events, ImagesOnly(image))
}

pub fn rebuild_history_with<'a>(
    events: impl IntoIterator<Item = &'a LogEvent>,
    blobs: impl BlobSource,
) -> Result<Rebuilt, RebuildError> {
    rebuild_versioned(
        events.into_iter().map(|e| (UNVERSIONED, false, None, e)),
        blobs,
        Seed::default(),
    )
    .map(|(rebuilt, _)| rebuilt)
}

pub fn rebuild_log_with<'a>(
    records: impl IntoIterator<Item = &'a LogRecord>,
    blobs: impl BlobSource,
) -> Result<Rebuilt, RebuildError> {
    let records: Vec<&LogRecord> = records.into_iter().collect();
    let keyed: Vec<(u64, &LogEvent)> = records.iter().map(|r| (r.seq, &r.event)).collect();
    let masked = rewind_mask(&keyed)?;
    rebuild_versioned(
        records
            .iter()
            .zip(masked)
            .map(|(r, masked)| (r.schema_version, masked, Some(r.seq), &r.event)),
        blobs,
        Seed::default(),
    )
    .map(|(rebuilt, _)| rebuilt)
}

pub fn rebuild_log_from<'a>(
    records: impl IntoIterator<Item = &'a LogRecord>,
    start: usize,
    mut blobs: impl BlobSource,
) -> Result<Option<Rebuilt>, RebuildError> {
    let records: Vec<&LogRecord> = records.into_iter().collect();
    let keyed: Vec<(u64, &LogEvent)> = records.iter().map(|r| (r.seq, &r.event)).collect();
    let masked = rewind_mask(&keyed)?;
    let events = || {
        records
            .iter()
            .zip(masked.iter().copied())
            .map(|(r, masked)| (r.schema_version, masked, Some(r.seq), &r.event))
    };
    let probe = Seed {
        start,
        placeholders: 0,
        probe: true,
    };
    let (_, at_head) = rebuild_versioned(events(), &mut blobs, probe)?;
    let Some((from, built)) = at_head else {
        return Ok(None);
    };
    let Some(placeholders) = from.checked_sub(built) else {
        return Ok(None);
    };
    let seed = Seed {
        start,
        placeholders,
        probe: false,
    };
    rebuild_versioned(events(), blobs, seed).map(|(rebuilt, _)| Some(rebuilt))
}

#[derive(Default, Clone, Copy)]
struct Seed {
    start: usize,
    placeholders: usize,
    probe: bool,
}

impl<B: BlobSource> BlobSource for &mut B {
    fn image(&mut self, image: &ImageRef) -> Result<String, String> {
        (**self).image(image)
    }

    fn media(&mut self, blob: &BlobRef) -> Result<String, String> {
        (**self).media(blob)
    }

    fn sha256(&mut self, base64: &str) -> Result<String, String> {
        (**self).sha256(base64)
    }
}

fn global(event: &LogEvent) -> bool {
    matches!(
        event,
        LogEvent::TailClosed { .. }
            | LogEvent::ThinkingCut { .. }
            | LogEvent::NativeWebSearch {
                state: FeatureState::FallenBack,
                ..
            }
    )
}

pub fn logged_head(event: &LogEvent, history: &[Message]) -> Option<LoggedHead> {
    let LogEvent::ContextCompacted {
        epoch,
        from,
        head,
        placement,
        cut,
        additions,
        ..
    } = event
    else {
        return None;
    };
    let cut = cut.unwrap_or(*from);
    Some(LoggedHead {
        epoch: *epoch,
        from: *from,
        message: head.clone(),
        placement: *placement,
        cut,
        additions: additions.clone(),
        join: (*placement == Placement::Leading)
            .then(|| kept_join(history, cut, *from))
            .flatten(),
    })
}

fn withdraw_from_tail<'a>(
    turn: u64,
    items: &'a [WithdrawnBlock],
    tail: Option<&mut Message>,
    blobs: &mut impl BlobSource,
    deferred: Option<&mut Vec<Deferred<'a>>>,
) -> Result<(), RebuildError> {
    let mut slots = Vec::new();
    if let Some(message) = tail {
        media_slots(&mut message.content, &mut slots);
    }
    let held = slots.len();
    let mut deferred = deferred;
    for item in items {
        let Some(slot) = slots.get_mut(item.ordinal) else {
            if let Some(deferred) = deferred.as_deref_mut() {
                deferred.push(Deferred {
                    ordinal: item.ordinal - held,
                    item,
                    turn,
                });
            }
            continue;
        };
        let data = match &*slot.block {
            ContentBlock::Image {
                source: crate::ImageSource::Base64 { data, .. },
            } => data,
            ContentBlock::Media { source } => source.data(),
            _ => {
                return Err(mismatch(
                    turn,
                    item,
                    format!("attachment {} is not an attachment", item.ordinal),
                ));
            }
        };
        let sha256 = blobs
            .sha256(data)
            .map_err(|reason| mismatch(turn, item, reason))?;
        if sha256 != item.sha256 {
            return Err(mismatch(
                turn,
                item,
                format!(
                    "the withdrawal names attachment {}, but the message holds {sha256} there",
                    item.sha256
                ),
            ));
        }
        *slot.block = ContentBlock::Text {
            text: item.note.clone(),
        };
    }
    Ok(())
}

#[derive(Default)]
struct Seen<'a> {
    model: Option<&'a str>,
    endpoint: Option<&'a str>,
    effort: Option<Effort>,
    epoch: u64,
    bytes_changed: bool,
}

fn rebuild_versioned<'a>(
    events: impl Iterator<Item = (u16, bool, Option<u64>, &'a LogEvent)>,
    mut blobs: impl BlobSource,
    seed: Seed,
) -> Result<(Rebuilt, Option<(usize, usize)>), RebuildError> {
    let mut history: Vec<Message> = Vec::new();
    let mut tool_notes: BTreeMap<usize, String> = BTreeMap::new();
    let mut paused = BTreeSet::new();
    let mut closed = Closed::default();
    let mut origins = Origins::default();
    let mut cuts: Vec<Cut> = Vec::new();
    let mut search_refused = BTreeSet::new();
    let mut requests = RequestList::default();
    let mut reply: Option<usize> = None;
    let mut asked: Option<&'a str> = None;
    let mut asked_effort: Option<Effort> = None;
    let mut served: Option<&'a str> = None;
    let mut current: Option<&'a str> = None;
    let mut pending: Option<PendingTurn<'a>> = None;
    let mut marker: Option<EffortMarker> = None;
    let mut tool_message: Vec<ContentBlock> = Vec::new();
    let mut open_tail: Option<u16> = None;
    let mut deferred: Vec<Deferred<'a>> = Vec::new();
    let mut boundary: Option<usize> = None;
    let mut head: Option<LoggedHead> = None;
    let mut head_source: Option<usize> = None;
    let mut points = rewind::Points::default();
    let mut seen = Seen::default();
    let mut at_head: Option<(usize, usize)> = None;
    let events: Vec<(u16, bool, Option<u64>, &'a LogEvent)> = events.collect();
    let lost: LostImages<'a> = events
        .iter()
        .filter_map(|(_, _, _, event)| match event {
            LogEvent::ImageLost { turn, sha256, note } => {
                Some(((*turn, sha256.as_str()), note.as_str()))
            }
            _ => None,
        })
        .collect();
    for (source, (version, masked, seq, event)) in events.into_iter().enumerate() {
        if source < seed.start && !global(event) {
            continue;
        }
        if source == seed.start && seed.start > 0 {
            history.extend((0..seed.placeholders).map(|_| Message {
                role: Role::Assistant,
                content: Vec::new(),
            }));
            origins.tag(
                &history,
                &Origin {
                    endpoint: None,
                    turn: 0,
                    source: 0,
                },
            );
        }
        if masked {
            match event {
                LogEvent::TurnStarted {
                    turn,
                    model,
                    endpoint_name,
                    effort,
                    ..
                } => {
                    seen.model = model.as_deref();
                    seen.endpoint = endpoint_name.as_deref();
                    seen.effort = *effort;
                    let baseline = requests.baseline(seen.bytes_changed);
                    points.masked.entry(*turn).or_insert(baseline);
                }
                LogEvent::StepEnded { accepted, .. } => {
                    if *accepted {
                        open_tail = None;
                        let logged = masked_request(event, &seen);
                        if let Some(logged) = logged {
                            points.refresh(&logged, seen.epoch);
                        }
                    }
                }
                LogEvent::AttachmentsWithdrawn { turn, items, .. } => {
                    let tail = history.last_mut().filter(|_| open_tail.is_some());
                    withdraw_from_tail(*turn, items, tail, &mut blobs, None)?;
                }
                LogEvent::ThinkingCut {
                    from,
                    through,
                    tools_sha256,
                } if tools_sha256.is_some() || from.message == 0 && from.block == 0 => {
                    if from.message < history.len() {
                        cuts.push(Cut {
                            from: *from,
                            through: (*through).min(history.len().saturating_sub(1)),
                            tools: tools_sha256.is_some(),
                        });
                    }
                }
                LogEvent::NativeWebSearch {
                    endpoint_name,
                    state: FeatureState::FallenBack,
                } => {
                    search_refused.insert(endpoint_name.clone());
                }
                LogEvent::ContextCompacted { epoch, .. } => seen.epoch = *epoch,
                _ => {}
            }
            continue;
        }
        match event {
            LogEvent::TurnStarted {
                turn,
                text,
                lead,
                effort_marker,
                tool_message: tools,
                images,
                inputs,
                endpoint_name,
                retry_of,
                model,
                effort,
                ..
            } => {
                asked = model.as_deref();
                asked_effort = *effort;
                marker = *effort_marker;
                tool_message.clone_from(tools);
                current = endpoint_name.as_deref();
                seen.model = asked;
                seen.endpoint = current;
                seen.effort = asked_effort;
                deferred.clear();
                let baseline = requests.baseline(seen.bytes_changed);
                let refreshed = baseline
                    .iter()
                    .map(|b| (b.started_at.clone(), b.ttl))
                    .collect();
                points.start(
                    RebuiltPoint {
                        turn: *turn,
                        source,
                        messages: history.len(),
                        blocks: rewind::last_len(&history),
                        opening: None,
                        cuts: cuts.len(),
                        head: head_source,
                        join: head.as_ref().and_then(|h| h.join),
                        epoch: head.as_ref().map_or(0, |h| h.epoch),
                        anchor: requests
                            .entries
                            .get(requests.mark..)
                            .and_then(|e| e.last())
                            .cloned(),
                        baseline,
                        refreshed,
                    },
                    &history,
                );
                let continues = retry_of.is_some()
                    && text.is_empty()
                    && images.is_empty()
                    && inputs.is_empty()
                    && !lead.replays();
                if continues {
                    pending = None;
                    continue;
                }
                let mut parts = match (inputs.is_empty(), lead.replays()) {
                    (false, _) => parts(inputs),
                    (true, true) if text.is_empty() && images.is_empty() => Vec::new(),
                    (true, _) => vec![Part {
                        images,
                        text,
                        note: None,
                    }],
                };
                parts.extend(lead.tail().map(|text| Part {
                    images: &[],
                    text,
                    note: None,
                }));
                pending = Some(PendingTurn {
                    turn: *turn,
                    lead,
                    parts,
                    after: Vec::new(),
                });
            }
            LogEvent::PerMessageEffort {
                state: FeatureState::FallenBack,
                ..
            } => {
                if let Some(m) = marker.as_mut() {
                    m.rendered = false;
                }
            }
            LogEvent::InputCommitted { turn, inputs, .. } => match pending.as_mut() {
                Some(user) if marker.is_some() || !tool_message.is_empty() => {
                    user.after.extend(parts(inputs));
                }
                Some(user) => user.parts.extend(parts(inputs)),
                None => {
                    let mut withdrawals = Withdrawals::new(std::mem::take(&mut deferred));
                    let blocks =
                        part_blocks(*turn, &parts(inputs), &mut blobs, &mut withdrawals, &lost)?;
                    withdrawals.finish()?;
                    append_after(&mut history, blocks, boundary);
                    let origin = Origin {
                        endpoint: current,
                        turn: *turn,
                        source,
                    };
                    origins.tag(&history, &origin);
                    open_tail = None;
                }
            },
            LogEvent::MessageCommitted {
                turn,
                message,
                stop_reason,
                model,
            } => {
                let origin = Origin {
                    endpoint: current,
                    turn: *turn,
                    source,
                };
                if let Some(user) = pending.take() {
                    points.opened(user.turn, rewind::opening_at(&history, boundary));
                    let late_marker = message.is_turn_system() && user.after.is_empty();
                    let mut withdrawals = Withdrawals::new(std::mem::take(&mut deferred));
                    let mut blocks: Vec<ContentBlock> = user
                        .lead
                        .texts()
                        .map(|s| ContentBlock::Text { text: s.to_owned() })
                        .collect();
                    blocks.extend(part_blocks(
                        user.turn,
                        &user.parts,
                        &mut blobs,
                        &mut withdrawals,
                        &lost,
                    )?);
                    append_after(&mut history, blocks, boundary);
                    if let Some(note) = user.lead.text(LEAD_TOOLS) {
                        tool_notes.insert(history.len().saturating_sub(1), note.to_owned());
                    }
                    let system =
                        Message::turn_system(marker.take(), &std::mem::take(&mut tool_message));
                    if !late_marker {
                        history.extend(system);
                    }
                    if !user.after.is_empty() {
                        let after = part_blocks(
                            user.turn,
                            &user.after,
                            &mut blobs,
                            &mut withdrawals,
                            &lost,
                        )?;
                        append_user(&mut history, after);
                    }
                    if late_marker {
                        deferred = withdrawals.rest()?;
                    } else {
                        withdrawals.finish()?;
                    }
                    origins.tag(&history, &origin);
                }
                if message.role == Role::Assistant {
                    served = model.as_deref();
                }
                if message.role == Role::Assistant && *stop_reason == Some(StopReason::PauseTurn) {
                    paused.insert(history.len());
                }
                open_tail = (message.role == Role::User).then_some(version);
                if message.role == Role::Assistant {
                    reply = Some(source);
                }
                let message = noted(message, *turn, &lost).resolve(&mut blobs).map_err(
                    |(sha256, reason)| RebuildError {
                        kind: DamageKind::Image,
                        turn: *turn,
                        sha256,
                        reason,
                        rewind_at: None,
                    },
                )?;
                history.push(message);
                origins.tag(&history, &origin);
            }
            LogEvent::StepEnded {
                turn,
                step,
                usage,
                prompt_tokens,
                used,
                first_pass,
                accepted,
                view_len,
                started_at,
                ttl,
                prefix,
                response_id,
                ..
            } => {
                if *accepted {
                    open_tail = None;
                    points.restored = None;
                }
                let logged = (
                    view_len,
                    started_at,
                    ttl,
                    prompt_tokens,
                    usage,
                    asked,
                    current,
                );
                if let (
                    Some(view_len),
                    Some(started_at),
                    Some(ttl),
                    Some(prompt_tokens),
                    Some(usage),
                    Some(model),
                    Some(endpoint_name),
                ) = logged
                {
                    let (used, first_pass) =
                        LoggedRequest::fills(*prompt_tokens, *used, *first_pass);
                    let logged = LoggedRequest {
                        reply,
                        turn: *turn,
                        step: *step,
                        view_len: Some(*view_len),
                        prompt_tokens: *prompt_tokens,
                        used,
                        first_pass,
                        output_tokens: usage.output_tokens,
                        cache_read: Some(usage.cache_read_input_tokens),
                        started_at: started_at.clone(),
                        ttl: *ttl,
                        model: model.to_owned(),
                        endpoint_name: Some(endpoint_name.to_owned()),
                        prefix: prefix.clone(),
                        response_id: response_id.clone(),
                        served: served.map(str::to_owned),
                        effort: asked_effort,
                        accepted: *accepted,
                    };
                    if *accepted {
                        points.refresh(&logged, seen.epoch);
                        seen.bytes_changed = false;
                    }
                    requests.entries.push(logged);
                }
            }
            LogEvent::RebuildMarked { reason, .. } => {
                seen.bytes_changed |= rewind::changes_bytes(reason);
                requests.marked();
            }
            LogEvent::AttachmentsWithdrawn { turn, items, .. } => {
                let tail = history.last_mut().filter(|_| open_tail.is_some());
                withdraw_from_tail(*turn, items, tail, &mut blobs, Some(&mut deferred))?;
            }
            LogEvent::TailClosed { through, calls, .. } => {
                closed.calls.extend(calls.iter().cloned());
                closed.tails.insert(*through);
            }
            LogEvent::NativeWebSearch {
                endpoint_name,
                state: FeatureState::FallenBack,
            } => {
                search_refused.insert(endpoint_name.clone());
            }
            LogEvent::ThinkingCut {
                from,
                through,
                tools_sha256,
            } => cuts.push(Cut {
                from: *from,
                through: *through,
                tools: tools_sha256.is_some(),
            }),
            LogEvent::ContextCompacted {
                epoch,
                placement,
                from,
                ..
            } => {
                at_head.get_or_insert((*from, history.len()));
                requests.clear();
                points.restored = None;
                seen.bytes_changed = false;
                boundary = placement.is_head().then_some(*from);
                open_tail = None;
                head = logged_head(event, &history);
                head_source = Some(source);
                seen.epoch = *epoch;
            }
            LogEvent::Rewound {
                point,
                messages,
                blocks,
                ..
            } => {
                pending = None;
                marker = None;
                deferred.clear();
                if !seed.probe {
                    rewind::check_rewind(*point, seq, &history, (*messages, *blocks))?;
                }
                points.rewound(point.turn());
                seen.epoch = head.as_ref().map_or(0, |h| h.epoch);
            }
            _ => {}
        }
    }
    points.settle(&history);
    let provisional = match (open_tail, history.last()) {
        (Some(version), Some(last)) if version >= PROVISIONAL_SINCE && last.role == Role::User => {
            Some(history.len() - 1)
        }
        _ => None,
    };
    let rebuilt = Rebuilt {
        head,
        messages: history,
        paused,
        closed,
        endpoints: origins.endpoints,
        turns: origins.turns,
        sources: origins.sources,
        cuts,
        provisional,
        search_refused,
        requests,
        points: points.list,
        restored: points.restored,
        tool_notes,
    };
    Ok((rebuilt, at_head))
}

fn masked_request(event: &LogEvent, seen: &Seen<'_>) -> Option<LoggedRequest> {
    let LogEvent::StepEnded {
        turn,
        step,
        prompt_tokens,
        started_at,
        ttl,
        view_len,
        accepted,
        ..
    } = event
    else {
        return None;
    };
    Some(LoggedRequest {
        reply: None,
        turn: *turn,
        step: *step,
        view_len: Some((*view_len)?),
        prompt_tokens: (*prompt_tokens)?,
        used: 0,
        first_pass: None,
        output_tokens: 0,
        cache_read: None,
        started_at: started_at.clone()?,
        ttl: (*ttl)?,
        model: seen.model?.to_owned(),
        endpoint_name: Some(seen.endpoint?.to_owned()),
        prefix: None,
        response_id: None,
        served: None,
        effort: seen.effort,
        accepted: *accepted,
    })
}
