pub mod anthropic;
mod body;
pub mod classify;
pub mod client_version;
pub mod credential;
mod foreign;
pub mod forms;
pub mod gemini;
pub(crate) mod limit;
pub mod openai;
mod sse;
pub mod voice;

use std::collections::{BTreeMap, BTreeSet};

use futures::FutureExt;
use futures::future::BoxFuture;
use futures::stream::BoxStream;
use jiff::SignedDuration;
use serde::Serialize;
use serde_json::Value;
use wisp_plugin_api::types::SliceKind;
pub use wisp_protocol::Feature;
use wisp_protocol::log::Cut;
use wisp_protocol::plan_usage::{PlanUsage, PlanUsageError};
use wisp_protocol::{
    CacheTtl, CitedSource, ContentBlock, Effort, ErrorDetail, ErrorKind, FailurePhase,
    FailureSource, FileSlice, InputTransformation, Message, NATIVE_ITEM, RenderingForm, RetryClass,
    Role, SearchAction, SearchOutcome, SessionConfig, StopReason, ToolRef, Usage,
};

use wisp_media::Tier;

use crate::endpoints::ProviderKind;
use crate::tool::ToolSpec;

pub(crate) struct Envelope<'a> {
    pub(crate) provider: &'a str,
    pub(crate) item: Option<&'a Value>,
    pub(crate) projections: Option<usize>,
    pub(crate) identity: Option<&'a str>,
}

pub(crate) fn native_envelope(block: &ContentBlock) -> Option<Envelope<'_>> {
    let ContentBlock::Opaque { provider, raw, .. } = block else {
        return None;
    };
    (raw.get("type").and_then(Value::as_str) == Some(NATIVE_ITEM)).then(|| Envelope {
        provider,
        item: raw.get("item"),
        projections: raw
            .get("projection_blocks")
            .and_then(Value::as_u64)
            .and_then(|count| usize::try_from(count).ok()),
        identity: raw.get("identity").and_then(Value::as_str),
    })
}

pub(crate) fn envelope(
    provider: &str,
    item: &Value,
    projections: usize,
    identity: &str,
) -> ContentBlock {
    let raw = serde_json::json!({"type": NATIVE_ITEM, "item": item, "projection_blocks": projections, "identity": identity});
    ContentBlock::opaque(provider, raw)
}

pub(crate) fn native_projection<'a>(
    preceding: impl DoubleEndedIterator<Item = &'a ContentBlock>,
) -> Option<&'a str> {
    preceding
        .rev()
        .enumerate()
        .find_map(|(offset, block)| {
            native_envelope(block).map(|e| {
                e.projections
                    .filter(|count| offset < *count)
                    .map(|_| e.provider)
            })
        })
        .flatten()
}

#[derive(Debug, Clone, Copy)]
pub struct Request<'a> {
    pub session: &'a SessionConfig,
    pub tools: &'a [ToolSpec],
    pub deferred: &'a [ToolSpec],
    pub by_value: &'a [ToolSpec],
    pub server_tools: &'a [ServerToolSpec],
    pub messages: &'a [Message],
    pub previous_response_id: Option<&'a str>,
    pub features: Features,
    pub options: RequestOptions,
    pub rendering: &'a Rendering,
    pub compaction: Option<&'a CompactionAsk>,
}

pub(crate) fn declare(declared: &mut BTreeSet<String>, change: &ContentBlock) {
    if let ContentBlock::ToolAddition {
        tool: ToolRef::ToolDefinition { definition },
    } = change
    {
        declared.insert(definition.name.clone());
    }
}

pub(crate) fn by_definition<'a>(
    block: &ContentBlock,
    by_value: &'a [ToolSpec],
) -> Option<&'a ToolSpec> {
    match block {
        ContentBlock::ToolAddition {
            tool: ToolRef::ToolReference { name },
        } => by_value.iter().find(|spec| spec.name == *name),
        _ => None,
    }
}

pub(crate) fn on_wire(
    declared: &mut BTreeSet<String>,
    block: &ContentBlock,
    tool: &ToolRef,
    by_value: &[ToolSpec],
) -> bool {
    if tool.by_value() {
        declare(declared, block);
        return true;
    }
    if let Some(spec) = by_definition(block, by_value) {
        declared.insert(spec.name.clone());
        return true;
    }
    declared.contains(tool.name())
}

pub(crate) fn carries_tool_changes(
    messages: &[Message],
    rendering: &Rendering,
    tool_changes: impl Fn(&ContentBlock) -> Vec<ContentBlock>,
) -> bool {
    messages.iter().enumerate().any(|(index, m)| {
        m.tool_blocks()
            .iter()
            .any(|b| b.tool_change().is_some() && !rendering.voided(index, b))
            || m.content.iter().any(|b| !tool_changes(b).is_empty())
    })
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CompactionAsk {
    pub native_instructions: Option<String>,
    pub tail: Option<TailMarker>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RequestOptions {
    pub debug_cache: bool,
    pub refusal_fallback: bool,
    pub message_threads: bool,
}

impl Default for RequestOptions {
    fn default() -> Self {
        Self {
            debug_cache: false,
            refusal_fallback: true,
            message_threads: true,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TailMarker {
    pub view_len: usize,
    pub ttl: wisp_protocol::CacheTtl,
    pub prefix: String,
    pub last_blocks: Option<usize>,
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize)]
pub struct ServerToolSpec {
    pub name: String,
    #[serde(rename = "type")]
    pub kind: String,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Rendering {
    pub closed: BTreeSet<String>,
    pub cuts: Vec<Cut>,
    pub native_as_text: Vec<bool>,
    pub paused: Vec<bool>,
    pub drop_base_marker: bool,
    pub search_only: bool,
    pub form: RenderingForm,
    pub provisional: bool,
    pub sends_media: bool,
    pub attachments_as_notes: bool,
    pub tools_point: usize,
    pub array: BTreeSet<String>,
    pub inline_search: bool,
}

pub static UNRENDERED: Rendering = Rendering {
    closed: BTreeSet::new(),
    cuts: Vec::new(),
    native_as_text: Vec::new(),
    paused: Vec::new(),
    drop_base_marker: false,
    search_only: false,
    form: RenderingForm::Text,
    provisional: false,
    sends_media: false,
    attachments_as_notes: false,
    tools_point: 0,
    array: BTreeSet::new(),
    inline_search: false,
};

fn added_by_value(block: &ContentBlock) -> Option<&str> {
    match block {
        ContentBlock::ToolAddition {
            tool: ToolRef::ToolDefinition { definition },
        } => Some(&definition.name),
        _ => None,
    }
}

pub(crate) fn voided_tool_change(
    index: usize,
    point: usize,
    block: &ContentBlock,
    in_array: impl FnOnce(&str) -> bool,
    inline_search: bool,
) -> bool {
    added_by_value(block).is_some_and(|name| {
        if name == crate::search::WEB_SEARCH {
            !inline_search || in_array(name)
        } else {
            index < point && in_array(name)
        }
    })
}

impl Rendering {
    pub fn cut(&self, message: usize, block: usize) -> bool {
        let at = wisp_protocol::log::BlockPosition { message, block };
        self.cuts
            .iter()
            .any(|c| c.from <= at && message <= c.through)
    }

    pub fn as_text(&self, message: usize) -> bool {
        self.native_as_text.get(message).copied().unwrap_or(false)
    }

    pub fn paused(&self, message: usize) -> bool {
        self.paused.get(message).copied().unwrap_or(false)
    }

    pub fn voided(&self, message: usize, block: &ContentBlock) -> bool {
        voided_tool_change(
            message,
            self.tools_point,
            block,
            |name| self.array.contains(name),
            self.inline_search,
        )
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SearchSignal {
    Started {
        call_id: String,
        query: String,
        action: SearchAction,
    },
    Ended {
        call_id: String,
        outcome: SearchOutcome,
    },
    Cited {
        sources: Vec<CitedSource>,
    },
}

pub type ServerCalls = BTreeSet<String>;

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Features {
    pub per_message_effort: Option<FeatureUse>,
    pub binding_controls: Option<FeatureUse>,
    pub native_compaction: Option<FeatureUse>,
    pub inline_tools: Option<FeatureUse>,
    pub deferred_tools: Option<FeatureUse>,
    pub documented: bool,
    pub thread: Option<Thread>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Thread {
    Create,
    Continue { held: usize },
}

impl Features {
    pub fn get(&self, feature: Feature) -> Option<FeatureUse> {
        let mut copy = *self;
        copy.slot(feature).and_then(|slot| *slot)
    }

    fn slot(&mut self, feature: Feature) -> Option<&mut Option<FeatureUse>> {
        match feature {
            Feature::PerMessageEffort => Some(&mut self.per_message_effort),
            Feature::BindingControls => Some(&mut self.binding_controls),
            Feature::NativeCompaction => Some(&mut self.native_compaction),
            Feature::InlineTools => Some(&mut self.inline_tools),
            Feature::DeferredTools => Some(&mut self.deferred_tools),
            Feature::NativeWebSearch => None,
        }
    }

    pub fn trials(&self) -> Vec<Feature> {
        pair_features()
            .filter(|f| self.get(*f) == Some(FeatureUse::Trial))
            .collect()
    }
}

pub fn pair_features() -> impl Iterator<Item = Feature> {
    Feature::ALL
        .into_iter()
        .filter(|feature| *feature != Feature::NativeWebSearch)
}

#[cfg(test)]
impl Features {
    pub(crate) fn using(features: &[Feature], used: FeatureUse) -> Self {
        let mut out = Self::default();
        for feature in features {
            if let Some(slot) = out.slot(*feature) {
                *slot = Some(used);
            }
        }
        out
    }

    pub(crate) fn trying(features: &[Feature]) -> Self {
        Self::using(features, FeatureUse::Trial)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FeatureUse {
    Established,
    Trial,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct ThinkingBudgets {
    pub low: Option<u32>,
    pub medium: Option<u32>,
    pub high: Option<u32>,
    pub xhigh: Option<u32>,
    pub max: Option<u32>,
}

impl ThinkingBudgets {
    pub fn get(self, effort: Effort) -> Option<u32> {
        match effort {
            Effort::Low => self.low,
            Effort::Medium => self.medium,
            Effort::High => self.high,
            Effort::Xhigh => self.xhigh.or(self.high),
            Effort::Max => self.max.or(self.xhigh).or(self.high),
        }
    }
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct ModelCapabilities {
    pub efforts: &'static [Effort],
    pub default_effort: Option<Effort>,
    pub per_message_effort: bool,
    pub effort_in_id: Option<Effort>,
    pub inline_tools: bool,
    pub tools_after_results: bool,
    pub deferred_tools: bool,
    pub adaptive_thinking: bool,
    pub thinking_budgets: Option<ThinkingBudgets>,
    pub max_output: Option<u32>,
    pub images: Option<ImageCaps>,
    pub slices: &'static [SliceKind],
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ImageCaps {
    pub tier: Tier,
    pub per_request: usize,
    pub many_images: usize,
    pub many_image_edge: u32,
}

const MANY_IMAGES: usize = 20;
const MANY_IMAGE_EDGE: u32 = wisp_media::MAX_LONG_EDGE;

impl ImageCaps {
    pub const fn new(tier: Tier, per_request: usize) -> Self {
        Self {
            tier,
            per_request,
            many_images: MANY_IMAGES,
            many_image_edge: MANY_IMAGE_EDGE,
        }
    }
}

impl ModelCapabilities {
    pub fn takes_effort(&self, effort: Effort) -> bool {
        self.efforts.contains(&effort)
    }

    pub(crate) fn accepts_effort(&self, named: Option<Effort>, inherited: Effort) -> bool {
        (named.is_none()
            && (self.effort_in_id.is_some()
                || wisp_protocol::settings::takes_no_effort(Some(self.efforts))))
            || self.takes_effort(named.unwrap_or(inherited))
    }

    pub fn selectable(&self, kind: ProviderKind) -> bool {
        matches!(kind, ProviderKind::OpenAi | ProviderKind::Gemini) || self.adaptive_thinking
    }

    pub fn check_selectable(&self, kind: ProviderKind, model: &str) -> Result<(), String> {
        if !self.selectable(kind) {
            return Err(format!(
                "{model} doesn't support adaptive thinking, which Wisp needs"
            ));
        }
        Ok(())
    }
}

pub fn shipped_capabilities(id: &str) -> Option<(ProviderKind, ModelCapabilities)> {
    let row = crate::models::ModelTable::shared().find(id)?;
    row.provider.zip(row.capabilities)
}

pub trait Provider: Send + Sync {
    fn generation(&self) -> Option<crate::endpoints::generation::Generation> {
        None
    }

    fn configure_plan_usage(&self, _enabled: bool) -> bool {
        false
    }

    fn plan_usage(&self) -> BoxFuture<'static, Result<Option<PlanUsage>, PlanUsageError>> {
        futures::future::ready(Ok(None)).boxed()
    }

    fn stream(
        &self,
        request: &Request<'_>,
    ) -> BoxStream<'static, Result<ProviderEvent, ProviderError>>;

    fn echoed<'a>(
        &self,
        content: &'a [ContentBlock],
        _earlier: &ServerCalls,
    ) -> Vec<&'a ContentBlock> {
        content.iter().collect()
    }

    fn pause_limit(&self) -> u32 {
        crate::agent::MAX_CONTINUATIONS
    }

    fn search_signal(&self, _block: &ContentBlock) -> Option<SearchSignal> {
        None
    }

    fn capabilities(&self, _model: &str) -> Option<ModelCapabilities> {
        None
    }

    fn model_list(&self) -> Option<std::sync::Arc<crate::models::list::ModelList>> {
        None
    }

    fn complete_model_list(&self) -> Option<std::sync::Arc<crate::models::list::ModelList>> {
        None
    }

    fn list_key(&self) -> Option<&str> {
        None
    }

    fn refresh_list(&self, _folder: std::path::PathBuf, _always: bool) -> BoxFuture<'static, ()> {
        futures::future::ready(()).boxed()
    }

    fn model_ids(&self) -> BoxFuture<'_, Result<Vec<String>, ProviderError>> {
        let refused = crate::models::list::Unusable::Unreachable.error(self.kind());
        futures::future::ready(Err(refused)).boxed()
    }

    fn media_types(&self) -> &'static [&'static str] {
        &[]
    }

    fn kind(&self) -> ProviderKind {
        ProviderKind::IMPLICIT
    }

    fn official(&self) -> bool {
        true
    }

    fn default_model(&self) -> String {
        self.kind().default_model().to_owned()
    }

    fn web_search_models(&self) -> Vec<String> {
        Vec::new()
    }

    fn features_documented(&self) -> bool {
        false
    }

    fn billing(&self) -> wisp_protocol::Billing {
        wisp_protocol::Billing::Metered
    }

    fn cache_life(&self, ttl: CacheTtl) -> SignedDuration {
        crate::context::ttl_span(ttl)
    }

    fn keeps_warm(&self) -> bool {
        false
    }

    fn busy_waits_5xx(&self) -> bool {
        true
    }

    fn account_bound_thinking(&self, _model: &str) -> bool {
        false
    }

    fn count_tokens(&self, _request: &Request<'_>) -> BoxFuture<'static, TokenCount> {
        futures::future::ready(TokenCount::Unsupported).boxed()
    }

    fn cache_prefix(&self, request: &Request<'_>) -> Option<String>;

    fn betas(&self, _request: &Request<'_>) -> Vec<String> {
        Vec::new()
    }

    fn keys_cache(&self, _beta: &str) -> bool {
        true
    }

    fn threads(&self, _request: &Request<'_>) -> bool {
        false
    }

    fn reply_print(&self, _content: &[ContentBlock]) -> Option<String> {
        None
    }

    fn tool_changes(&self, _block: &ContentBlock) -> Vec<ContentBlock> {
        Vec::new()
    }

    fn prefix_bytes(&self, _request: &Request<'_>) -> Option<PrefixBytes> {
        None
    }

    fn wire_print(&self, _request: &Request<'_>) -> Option<WirePrint> {
        None
    }

    fn voice(&self) -> Option<std::sync::Arc<dyn voice::VoiceRoute>> {
        None
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WirePrint {
    pub betas: String,
    pub pinned: String,
    pub blocks: Option<usize>,
    pub messages: Vec<String>,
    pub thinking: BTreeSet<(usize, usize)>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PrefixBytes {
    pub model: Vec<u8>,
    pub effort: Vec<u8>,
    pub thinking: Vec<u8>,
    pub tools: Vec<u8>,
    pub system: Vec<u8>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TokenCount {
    Counted { tokens: u64 },
    Unsupported,
    Failed { reason: String },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Iteration {
    pub kind: String,
    pub model: String,
    pub usage: Usage,
}

#[derive(Debug, Clone, PartialEq)]
pub enum ProviderEvent {
    PlanUsage(Result<PlanUsage, PlanUsageError>),
    UsageUnavailable,
    Sending,
    MessageStart {
        id: Option<String>,
        model: String,
        usage: Usage,
        iterations: Vec<Iteration>,
        diagnostics: Option<Value>,
    },
    BlockStart {
        index: usize,
        block: ContentBlock,
    },
    BlockDelta {
        index: usize,
        delta: Delta,
    },
    BlockStop {
        index: usize,
    },
    Fallback {
        from: String,
        to: String,
    },
    MessageDelta {
        stop_reason: Option<StopReason>,
        stop_details: Option<Value>,
        usage: Usage,
        iterations: Vec<Iteration>,
        diagnostics: Option<Value>,
    },
    MessageStop,
    Search {
        index: usize,
        signal: SearchSignal,
    },
    InputTransformations {
        provider: String,
        transformations: Vec<InputTransformation>,
    },
    Unknown {
        provider: String,
        raw: Value,
    },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Delta {
    Text(String),
    Thinking(String),
    Signature(String),
    InputJson(String),
    Unknown { provider: String, raw: Value },
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("{message}")]
pub struct ProviderError {
    pub kind: ErrorKind,
    pub message: String,
    diagnostic_reason: Option<String>,
    pub possibly_billed: bool,
    pub usage_complete: bool,
    pub rejected: Vec<Feature>,
    pub refused: Option<Refused>,
    pub content_refused: bool,
    pub detail: Box<ErrorDetail>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Refused {
    NativeSearch,
    SearchSuspect,
    CrossAccount,
    Rendering,
    Attachment,
    CompactionBlock,
    CompactionUnavailable,
    Thread,
}

impl ProviderError {
    pub fn new(kind: ErrorKind, message: impl Into<String>) -> Self {
        Self::classified(
            kind,
            message,
            ErrorDetail::new(
                FailurePhase::Connect,
                FailureSource::Local,
                RetryClass::Never,
            ),
        )
    }

    pub fn classified(
        kind: ErrorKind,
        message: impl Into<String>,
        mut detail: ErrorDetail,
    ) -> Self {
        detail.error_type = detail.error_type.map(|name| scrub_error_type(&name));
        Self {
            kind,
            message: scrub_error_text(&message.into(), ErrorText::Whole),
            diagnostic_reason: None,
            possibly_billed: detail.possibly_billed,
            usage_complete: false,
            rejected: Vec::new(),
            refused: None,
            content_refused: false,
            detail: Box::new(detail),
        }
    }

    #[must_use]
    pub fn malformed_call(&self) -> bool {
        self.detail.malformed_call.is_some()
    }

    #[must_use]
    pub(crate) fn with_diagnostic_reason(mut self, reason: Option<&str>) -> Self {
        self.diagnostic_reason = reason.and_then(|reason| clean_error_message(reason, None));
        self
    }

    pub(crate) fn replace_reported_message(&mut self, message: String) {
        self.message = message;
        self.diagnostic_reason = None;
    }

    pub(crate) fn reported_message(&self) -> String {
        match &self.diagnostic_reason {
            Some(reason) => format!("{}: {reason}", self.message),
            None => self.message.clone(),
        }
    }

    pub fn malformed(message: impl Into<String>) -> Self {
        Self::classified(
            ErrorKind::InvalidResponse,
            message,
            ErrorDetail::new(
                FailurePhase::Stream,
                FailureSource::Local,
                RetryClass::Never,
            ),
        )
        .possibly_billed()
    }

    pub(crate) fn reply_cut(provider: &str, retry: RetryClass) -> Self {
        let mut detail = ErrorDetail::new(FailurePhase::Stream, FailureSource::Endpoint, retry);
        detail.status = Some(200);
        detail.error_type = Some(StopReason::MaxTokens.as_str().to_owned());
        detail.possibly_billed = true;
        let mut error = Self::classified(
            ErrorKind::InvalidResponse,
            format!("{provider} returned a reply that ran to the output limit"),
            detail,
        );
        error.usage_complete = true;
        error
    }

    #[must_use]
    pub fn possibly_billed(mut self) -> Self {
        if self.rejected.is_empty() {
            self.possibly_billed = true;
            self.detail.possibly_billed = true;
        }
        self
    }

    pub fn maybe_billed(&self, usage: bool) -> bool {
        usage || self.possibly_billed
    }

    pub fn uncounted(&self, usage: bool) -> bool {
        self.maybe_billed(usage) && !self.usage_complete
    }

    #[must_use]
    pub fn binding_thinking(mut self) -> Self {
        self.kind = ErrorKind::ThinkingBinding;
        self.rejected = Vec::new();
        self.detail.retry = RetryClass::Never;
        self
    }

    #[must_use]
    pub fn refusing(mut self, refused: Refused) -> Self {
        self.refused = Some(refused);
        self
    }

    #[must_use]
    pub fn rejecting(mut self, features: Vec<Feature>) -> Self {
        if !features.is_empty() {
            self.possibly_billed = false;
            self.detail.possibly_billed = false;
            self.detail.retry = RetryClass::Never;
            self.rejected = features;
        }
        self
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct AssembledMessage {
    pub id: Option<String>,
    pub model: String,
    pub content: Vec<ContentBlock>,
    pub indices: Vec<usize>,
    pub stop_reason: StopReason,
    pub stop_details: Option<Value>,
    pub usage: Usage,
    pub iterations: Vec<Iteration>,
    pub diagnostics: Option<Value>,
    pub drops: Vec<ThinkingDrop>,
    pub reencoded_input: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ThinkingDrop {
    pub path: String,
    pub reason: Option<String>,
}

impl AssembledMessage {
    pub fn into_message(self) -> Message {
        Message {
            role: Role::Assistant,
            content: self.content,
        }
    }
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Discarded {
    pub blocks: u32,
    pub chars: u64,
    pub text: String,
}

#[derive(Debug)]
struct PartialBlock {
    block: ContentBlock,
    quiet: bool,
    json: String,
    done: bool,
}

#[derive(Debug, Default)]
pub struct Assembler {
    id: Option<String>,
    model: String,
    blocks: BTreeMap<usize, PartialBlock>,
    stop_reason: Option<StopReason>,
    stop_details: Option<Value>,
    usage: Usage,
    iterations: Vec<Iteration>,
    diagnostics: Option<Value>,
    drops: Vec<ThinkingDrop>,
    started: bool,
    usage_known: bool,
    stopped: bool,
    reencoded_input: bool,
}

impl Assembler {
    pub fn usage(&self) -> Option<Usage> {
        (self.started && self.usage_known).then_some(self.usage)
    }

    pub fn partial_text(&self) -> String {
        self.blocks
            .values()
            .filter_map(|b| match &b.block {
                ContentBlock::Text { text } => Some(text.as_str()),
                ContentBlock::Opaque { raw, .. }
                    if raw.get("type").and_then(Value::as_str) == Some("text") =>
                {
                    raw.get("text").and_then(Value::as_str)
                }
                _ => None,
            })
            .collect::<Vec<_>>()
            .join("\n\n")
    }

    pub fn iterations(&self) -> &[Iteration] {
        &self.iterations
    }

    pub fn discarded(&self) -> Discarded {
        let chars = |s: &str| u64::try_from(s.chars().count()).unwrap_or(u64::MAX);
        let streamed = |b: &PartialBlock| -> u64 {
            let own = match &b.block {
                ContentBlock::Text { text } => chars(text),
                ContentBlock::Thinking { thinking, .. } => chars(thinking),
                ContentBlock::Opaque { raw, .. } => ["text", "thinking"]
                    .iter()
                    .filter_map(|k| raw.get(*k).and_then(Value::as_str))
                    .map(chars)
                    .sum(),
                _ => 0,
            };
            own.saturating_add(chars(&b.json))
        };
        Discarded {
            blocks: u32::try_from(self.blocks.len()).unwrap_or(u32::MAX),
            chars: self
                .blocks
                .values()
                .map(streamed)
                .fold(0, u64::saturating_add),
            text: self.partial_text(),
        }
    }

    pub fn model(&self) -> &str {
        &self.model
    }

    pub fn blocks(&self) -> impl Iterator<Item = &ContentBlock> {
        self.blocks.values().map(|b| &b.block)
    }

    pub(crate) fn quiet_block(&self, index: usize) -> bool {
        self.blocks.get(&index).is_some_and(|block| block.quiet)
    }

    pub fn block(&self, index: usize) -> Option<&ContentBlock> {
        self.blocks.get(&index).filter(|b| b.done).map(|b| &b.block)
    }

    pub fn apply(&mut self, event: ProviderEvent) -> Result<(), ProviderError> {
        match event {
            ProviderEvent::Sending
            | ProviderEvent::PlanUsage(_)
            | ProviderEvent::Search { .. }
            | ProviderEvent::Unknown { .. } => {}
            ProviderEvent::InputTransformations {
                transformations, ..
            } => {
                self.drops = transformations
                    .into_iter()
                    .filter(|t| t.kind == anthropic::THINKING_DROPPED)
                    .filter_map(|t| {
                        Some(ThinkingDrop {
                            path: t.path.flatten()?,
                            reason: t.reason.flatten(),
                        })
                    })
                    .collect();
            }
            ProviderEvent::UsageUnavailable => self.usage_known = false,
            ProviderEvent::MessageStart {
                id,
                model,
                usage,
                iterations,
                diagnostics,
            } => {
                self.id = id;
                self.model = model;
                self.usage = usage;
                self.usage_known = true;
                if !iterations.is_empty() {
                    self.iterations = iterations;
                }
                self.set_diagnostics(diagnostics);
                self.started = true;
            }
            ProviderEvent::Fallback { to, .. } => self.model = to,
            ProviderEvent::BlockStart { index, block } => {
                self.blocks.insert(
                    index,
                    PartialBlock {
                        quiet: quiet_block(&block),
                        block,
                        json: String::new(),
                        done: false,
                    },
                );
            }
            ProviderEvent::BlockDelta { index, delta } => {
                let partial = self.blocks.get_mut(&index).ok_or_else(|| {
                    ProviderError::malformed(format!("delta for unknown block {index}"))
                })?;
                match (&mut partial.block, delta) {
                    (ContentBlock::Text { text }, Delta::Text(d)) => text.push_str(&d),
                    (ContentBlock::Thinking { thinking, .. }, Delta::Thinking(d)) => {
                        thinking.push_str(&d)
                    }
                    (ContentBlock::Thinking { signature, .. }, Delta::Signature(d)) => {
                        signature.push_str(&d)
                    }
                    (
                        ContentBlock::ToolUse { .. } | ContentBlock::Opaque { .. },
                        Delta::InputJson(d),
                    ) => partial.json.push_str(&d),
                    (ContentBlock::Opaque { raw, .. }, Delta::Text(d)) => {
                        append_str(raw, "text", &d)
                    }
                    (ContentBlock::Opaque { raw, .. }, Delta::Thinking(d)) => {
                        append_str(raw, "thinking", &d)
                    }
                    (ContentBlock::Opaque { raw, .. }, Delta::Signature(d)) => {
                        append_str(raw, "signature", &d)
                    }
                    (block, Delta::Unknown { provider, raw }) => {
                        fold_unknown_delta(&provider, block, raw)
                    }
                    (_, delta) => {
                        return Err(ProviderError::malformed(format!(
                            "{delta:?} doesn't fit block {index}"
                        )));
                    }
                }
            }
            ProviderEvent::BlockStop { index } => {
                let partial = self.blocks.get_mut(&index).ok_or_else(|| {
                    ProviderError::malformed(format!("stop for unknown block {index}"))
                })?;
                if !partial.json.is_empty() {
                    let input: Value = serde_json::from_str(&partial.json).map_err(|e| {
                        ProviderError::malformed(format!(
                            "block {index} has incomplete or invalid tool input JSON, so the reply was cut off or malformed: {e}"
                        ))
                    })?;
                    match &mut partial.block {
                        ContentBlock::ToolUse { input: slot, .. } => {
                            self.reencoded_input |= reencoded_json(&partial.json, &input);
                            *slot = input;
                        }
                        ContentBlock::Opaque {
                            raw: Value::Object(map),
                            ..
                        } => {
                            self.reencoded_input |= reencoded_json(&partial.json, &input);
                            map.insert("input".into(), input);
                        }
                        _ => {}
                    }
                }
                partial.done = true;
            }
            ProviderEvent::MessageDelta {
                stop_reason,
                stop_details,
                usage,
                iterations,
                diagnostics,
            } => {
                if !iterations.is_empty() {
                    self.iterations = iterations;
                }
                if stop_reason.is_some() {
                    self.stop_reason = stop_reason;
                }
                if stop_details.is_some() {
                    self.stop_details = stop_details;
                }
                if diagnostics.is_some() {
                    self.set_diagnostics(diagnostics);
                }
                self.usage = usage;
                self.usage_known = true;
            }
            ProviderEvent::MessageStop => self.stopped = true,
        }
        Ok(())
    }

    fn set_diagnostics(&mut self, diagnostics: Option<Value>) {
        self.diagnostics = diagnostics.map(without_identity);
    }

    pub fn finish(self) -> Result<AssembledMessage, ProviderError> {
        let stop_reason = self.ended()?;
        Ok(AssembledMessage {
            id: self.id,
            model: self.model,
            indices: self.blocks.keys().copied().collect(),
            content: self.blocks.into_values().map(|b| b.block).collect(),
            stop_reason,
            stop_details: self.stop_details,
            usage: self.usage,
            iterations: self.iterations,
            diagnostics: self.diagnostics,
            drops: self.drops,
            reencoded_input: self.reencoded_input,
        })
    }

    pub fn ended(&self) -> Result<StopReason, ProviderError> {
        if !self.stopped {
            return Err(ProviderError::classified(
                ErrorKind::Network,
                "stream ended before the message completed",
                ErrorDetail::new(
                    FailurePhase::Stream,
                    FailureSource::Local,
                    RetryClass::Transient,
                ),
            )
            .possibly_billed());
        }
        let stop_reason = self
            .stop_reason
            .clone()
            .ok_or_else(|| ProviderError::malformed("message completed without a stop reason"))?;
        if let Some((index, _)) = self.blocks.iter().find(|(_, b)| !b.done) {
            return Err(ProviderError::malformed(format!(
                "block {index} never finished, so the reply was cut off"
            )));
        }
        Ok(stop_reason)
    }
}

pub(crate) const MODEL_CANT_TAKE: &str = "this model can't take it";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum NotSent {
    Server,
    Model,
    Slice,
}

pub(crate) fn not_sent_note(media_type: &str, slice: Option<FileSlice>, why: NotSent) -> String {
    let reason = match why {
        NotSent::Server => "this server can't carry it",
        NotSent::Model => MODEL_CANT_TAKE,
        NotSent::Slice => "this model can't read the selected part",
    };
    match slice {
        Some(slice) => format!("[{media_type} file ({slice}) not sent: {reason}]"),
        None => format!("[{media_type} file not sent: {reason}]"),
    }
}

pub(crate) const NO_TEXT: &str = "[no text]";
pub(crate) const REASONING_ITEM: &str = "reasoning";

fn said<'a>(blocks: impl IntoIterator<Item = &'a ContentBlock>) -> Option<&'a str> {
    let mut said = None;
    for block in blocks {
        match view(block) {
            View::Text(text) if said.is_none() => said = Some(text),
            seen if never_content(&seen) => {}
            View::Other if native_envelope(block).is_some_and(|e| e.projections > Some(0)) => {}
            _ => return None,
        }
    }
    Some(said.unwrap_or_default())
}

pub(crate) fn quiet_end<'a>(blocks: impl IntoIterator<Item = &'a ContentBlock>) -> bool {
    said(blocks).is_some_and(|text| text.trim() == NO_TEXT)
}

pub(crate) fn may_end_quietly<'a>(blocks: impl IntoIterator<Item = &'a ContentBlock>) -> bool {
    said(blocks).is_some_and(|text| NO_TEXT.starts_with(text.trim()))
}

pub(crate) fn has_content(echoed: &[&ContentBlock]) -> bool {
    echoed.iter().any(|b| match view(b) {
        View::Text(text) => !text.trim().is_empty(),
        seen => !never_content(&seen),
    })
}

fn never_content(seen: &View<'_>) -> bool {
    match seen {
        View::Thinking { .. } | View::Marker | View::Lossy => true,
        View::Text(_) | View::ToolUse | View::Other => false,
    }
}

pub(crate) enum View<'a> {
    Text(&'a str),
    Thinking { signed: bool },
    ToolUse,
    Marker,
    Lossy,
    Other,
}

pub(crate) fn quiet_block(block: &ContentBlock) -> bool {
    let ContentBlock::Opaque { raw, .. } = block else {
        return false;
    };
    opaque_marker(raw.get("type").and_then(Value::as_str).unwrap_or_default())
}

pub(crate) fn opaque_marker(kind: &str) -> bool {
    matches!(
        kind,
        "redacted_thinking" | "fallback" | gemini::CALL_SIGNATURES_ITEM
    )
}

pub(crate) fn view(block: &ContentBlock) -> View<'_> {
    match block {
        ContentBlock::Text { text } => View::Text(text),
        ContentBlock::Thinking { signature, .. } => View::Thinking {
            signed: !signature.is_empty(),
        },
        ContentBlock::RedactedThinking { .. } => View::Marker,
        ContentBlock::ToolUse { .. } => View::ToolUse,
        ContentBlock::ToolResult { .. }
        | ContentBlock::Effort { .. }
        | ContentBlock::Image { .. }
        | ContentBlock::Media { .. }
        | ContentBlock::ToolAddition { .. }
        | ContentBlock::ToolRemoval { .. } => View::Other,
        ContentBlock::Opaque { .. }
            if openai::native_item(block)
                .or_else(|| gemini::native_item(block))
                .is_some_and(|(item, ..)| {
                    item.get("type").and_then(Value::as_str) == Some(REASONING_ITEM)
                }) =>
        {
            View::Thinking { signed: true }
        }
        ContentBlock::Opaque { .. }
            if gemini::native_item(block).is_some_and(|(item, ..)| {
                item.get("type").and_then(Value::as_str) == Some(gemini::TEXT_SIGNATURE_ITEM)
            }) =>
        {
            View::Marker
        }
        ContentBlock::Opaque { raw, deltas, .. } => {
            let str_field = |k: &str| raw.get(k).and_then(|v| v.as_str()).unwrap_or_default();
            if !deltas.is_empty() && str_field("type") != "text" {
                return View::Lossy;
            }
            match str_field("type") {
                "text" => View::Text(str_field("text")),
                "thinking" => View::Thinking {
                    signed: !str_field("signature").is_empty(),
                },
                kind if opaque_marker(kind) => View::Marker,
                "tool_use" => View::ToolUse,
                _ => View::Other,
            }
        }
    }
}

pub(crate) fn reply_text<'a, I>(blocks: I) -> Option<String>
where
    I: IntoIterator<Item = &'a ContentBlock>,
    I::IntoIter: Clone,
{
    let blocks = blocks.into_iter();
    (!quiet_end(blocks.clone())).then(|| {
        blocks
            .filter_map(|block| match block {
                ContentBlock::Text { text } => Some(text.as_str()),
                _ => None,
            })
            .collect::<Vec<_>>()
            .join("\n\n")
    })
}

pub(crate) fn without_identity(mut value: Value) -> Value {
    match &mut value {
        Value::Object(fields) => {
            fields.retain(|key, _| {
                !matches!(
                    key.as_str(),
                    "safety_identifier"
                        | "user"
                        | "user_id"
                        | "account_id"
                        | "organization_id"
                        | "cloudaicompanionProject"
                        | "email"
                        | "metadata"
                        | "plan_type"
                        | "credits"
                        | "promo"
                )
            });
            for field in fields.values_mut() {
                *field = without_identity(field.take());
            }
        }
        Value::Array(items) => {
            for item in items {
                *item = without_identity(item.take());
            }
        }
        _ => {}
    }
    value
}

const ID_PREFIXES: [&str; 3] = ["org-", "user-", "proj_"];

const ID_MIN_CHARS: usize = 20;

pub(crate) fn without_secret_text(text: &str) -> String {
    without_secret_tokens(&crate::session::without_registered_secrets(text))
}

fn without_secret_tokens(text: &str) -> String {
    redacting_tokens(text, |token| {
        is_identity(token)
            || wisp_protocol::has_known_key_prefix(token)
            || wisp_protocol::model_looks_like_key(token)
    })
}

fn redact_after_prefix(text: &str, prefix: &str, allow_space: bool, punctuation: &[u8]) -> String {
    let lower = text.to_ascii_lowercase();
    let mut out = String::with_capacity(text.len());
    let mut from = 0;
    while let Some(rel) = lower.get(from..).and_then(|s| s.find(prefix)) {
        let prefix_end = from + rel + prefix.len();
        out.push_str(text.get(from..prefix_end).unwrap_or_default());
        let tail = text.get(prefix_end..).unwrap_or_default();
        let ws_len = if allow_space {
            tail.bytes()
                .take_while(|b| matches!(b, b' ' | b'\t'))
                .count()
        } else {
            0
        };
        out.push_str(tail.get(..ws_len).unwrap_or_default());
        let after_ws = tail.get(ws_len..).unwrap_or_default();
        let raw_len = after_ws
            .bytes()
            .take_while(|b| b.is_ascii_alphanumeric() || punctuation.contains(b))
            .count();
        let run = after_ws
            .get(..raw_len)
            .unwrap_or_default()
            .trim_end_matches('.');
        if run.is_empty() {
            from = prefix_end + ws_len;
        } else {
            out.push_str(crate::session::REDACTED);
            from = prefix_end + ws_len + run.len();
        }
    }
    out.push_str(text.get(from..).unwrap_or_default());
    out
}

enum ErrorText {
    Whole,
    Prose(Option<fn(&str) -> String>),
}

fn scrub_error_text(message: &str, mode: ErrorText) -> String {
    use crate::session::REDACTED;

    static BEARER: std::sync::LazyLock<Option<regex::Regex>> = std::sync::LazyLock::new(|| {
        let gap = r"[\s\u{200B}]";
        let quote = r#"(?:\\?['"`])"#;
        let value_open = format!("(?:{quote}?<{quote}?|{quote})");
        regex::Regex::new(&format!(
            r#"(?ix)
                ((?:bearer(?:{gap}+token)?
                    (?:{quote}?(?:{gap}*[:=]{gap}*|{gap}+){value_open}?|{value_open}))+)
                (?:{}|[a-z0-9_.~+/=-]*[a-z0-9_~+/=-])"#,
            regex::escape(REDACTED),
        ))
        .ok()
    });
    static EMAIL: std::sync::LazyLock<Option<regex::Regex>> = std::sync::LazyLock::new(|| {
        regex::Regex::new(r"[\w.!#$%&'*+/=?^`{|}~-]+(?:@|%40)[\w-]+(?:(?:\.|%2[eE])[\w-]+)+").ok()
    });
    let (Some(bearer), Some(email)) = (BEARER.as_ref(), EMAIL.as_ref()) else {
        return REDACTED.to_owned();
    };
    let registered = crate::session::without_registered_secrets(message);
    let (text, fields) = match mode {
        ErrorText::Whole => (registered.as_str(), None),
        ErrorText::Prose(redact) => {
            let prose = error_prose(&registered);
            (prose, redact.map(|redact| redact(prose)))
        }
    };
    let without_email = email.replace_all(fields.as_deref().unwrap_or(text), REDACTED);
    let without_bearer = bearer.replace_all(&without_email, |found: &regex::Captures<'_>| {
        format!("{}{REDACTED}", &found[1])
    });
    without_secret_tokens(&without_bearer)
}

fn error_prose(message: &str) -> &str {
    let mut path_end = None;
    message
        .match_indices(['{', '['])
        .find(|&(at, bracket)| {
            if bracket == "{" {
                return true;
            }
            if message[at..].starts_with(crate::session::REDACTED) {
                return false;
            }
            let follows_path = path_end == Some(at)
                || message[..at].ends_with(|c: char| c.is_alphanumeric() || c == '_');
            if follows_path
                && let Some((name, _)) = message[at + 1..].split_once(']')
                && !name.is_empty()
                && name.chars().all(is_token_char)
            {
                path_end = Some(at + name.len() + 2);
                return false;
            }
            true
        })
        .map_or(message, |(at, _)| message[..at].trim())
}

fn clean_error_message(message: &str, redact_fields: Option<fn(&str) -> String>) -> Option<String> {
    const MAX_ERROR_CHARS: usize = 240;
    let scrubbed = scrub_error_text(message, ErrorText::Prose(redact_fields));
    let capped: String = scrubbed.chars().take(MAX_ERROR_CHARS).collect();
    (!capped.trim().is_empty()).then_some(capped)
}

fn redacting_tokens(text: &str, redacts: impl Fn(&str) -> bool) -> String {
    let mut kept = String::with_capacity(text.len());
    let mut rest = text;
    while let Some(start) = rest.find(is_token_char) {
        let (before, tail) = rest.split_at(start);
        let end = tail.find(|c| !is_token_char(c)).unwrap_or(tail.len());
        let (token, after) = tail.split_at(end);
        kept.push_str(before);
        let escaped = before.ends_with('\\') && token.starts_with(['n', 'r', 't']);
        let (escape, candidate) = token.split_at(usize::from(escaped));
        if redacts(candidate) {
            kept.push_str(escape);
            kept.push_str(crate::session::REDACTED);
        } else if escaped && redacts(token) {
            kept.push_str(crate::session::REDACTED);
        } else {
            kept.push_str(token);
        }
        rest = after;
    }
    kept.push_str(rest);
    kept
}

fn scrub_error_text_capped(text: &str, max_chars: usize) -> String {
    scrub_error_text(text, ErrorText::Whole)
        .chars()
        .take(max_chars)
        .collect()
}

const MAX_ERROR_DETAIL_CHARS: usize = 64;
const KNOWN_LONG_ERROR_TYPES: &[&str] = &["ServiceUnavailableException"];

fn scrub_error_type(text: &str) -> String {
    if KNOWN_LONG_ERROR_TYPES.contains(&text)
        && crate::session::without_registered_secrets(text) == text
    {
        return text.to_owned();
    }
    scrub_error_text_capped(text, MAX_ERROR_DETAIL_CHARS)
}

fn is_token_char(c: char) -> bool {
    c.is_ascii_alphanumeric() || c == '-' || c == '_'
}

fn is_identity(token: &str) -> bool {
    let uuid = token.len() == 36 && uuid::Uuid::try_parse(token).is_ok();
    uuid || ID_PREFIXES.iter().any(|prefix| {
        token.strip_prefix(prefix).is_some_and(|id| {
            id.len() >= ID_MIN_CHARS && id.bytes().all(|b| b.is_ascii_alphanumeric())
        })
    })
}

fn append_str(raw: &mut Value, field: &str, s: &str) {
    if let Value::Object(map) = raw {
        match map.get_mut(field) {
            Some(Value::String(existing)) => existing.push_str(s),
            _ => {
                map.insert(field.to_owned(), Value::String(s.to_owned()));
            }
        }
    }
}

fn fold_unknown_delta(provider: &str, block: &mut ContentBlock, delta: Value) {
    if provider == anthropic::PROVIDER {
        return anthropic::fold_unknown_delta(block, delta);
    }
    if !matches!(block, ContentBlock::Opaque { .. }) {
        let raw = serde_json::to_value(&*block).unwrap_or_default();
        *block = ContentBlock::opaque(provider, raw);
    }
    if let ContentBlock::Opaque { deltas, .. } = block {
        push_unknown(deltas, delta);
    }
}

fn push_unknown(deltas: &mut Vec<Value>, delta: Value) {
    deltas.push(without_identity(delta));
}

fn reencoded_json(raw: &str, value: &Value) -> bool {
    let Ok(encoded) = serde_json::to_string(value) else {
        return true;
    };
    let mut compact = String::with_capacity(raw.len());
    let mut in_string = false;
    let mut escaped = false;
    for ch in raw.chars() {
        if in_string {
            compact.push(ch);
            if escaped {
                escaped = false;
            } else if ch == '\\' {
                escaped = true;
            } else if ch == '"' {
                in_string = false;
            }
        } else if ch == '"' {
            in_string = true;
            compact.push(ch);
        } else if !matches!(ch, ' ' | '\t' | '\n' | '\r') {
            compact.push(ch);
        }
    }
    compact != encoded
}
