use std::collections::BTreeMap;
use std::fmt;
use std::str::FromStr;
use std::sync::Arc;

use serde::{Deserialize, Serialize};
use serde_json::Value;

mod adapted;
pub mod agents;
mod cache;
mod command_secret;
mod compaction;
pub mod context;
pub mod endpoint;
pub mod filter;
mod hook;
mod instructions;
pub mod jsonl;
pub mod link;
pub mod log;
mod media;
mod names;
pub mod opening;
mod paths;
pub mod plan_usage;
pub mod plugin;
mod process;
mod question;
mod retry;
mod rewind;
mod search;
pub use command_secret::{command_secret, masked_command};
mod secret;
pub mod settings;
pub mod slot;
pub mod subscription_auth;
pub mod todos;
mod tool;
mod transformation;
mod variable;
mod view;
pub mod voice;

pub use view::{NextTurn, Now, Pair, Returned, ReturnedImage, SessionView};

pub use adapted::HistoryChange;
pub use agents::{
    Activity, AgentAction, AgentEntry, AgentPath, AgentState, ContextFill, HoldReason, IdleOutcome,
    KeepWarm, MailFrom, MailId, MailKind, ParentLink, PresetChange, QueueReason, ReportOutcome,
    SpawnChoices, SpawnRequest, WakeSource, mail_body, render_mail, report_parts, report_text,
};
pub use cache::{MissCause, RefreshOutcome};
pub use compaction::{
    ALREADY_COMPACTING, COMPACT_BETWEEN_TURNS, CONTINUE_NOTE, CompactionEngine, CompactionFailure,
    CompactionTrigger, Covers, DiscardReason, NO_NATIVE_COMPACTION, NOTHING_TO_COMPACT, Placement,
    SwitchCompaction, SwitchCompactionCause,
};
pub use context::{Marked, SUBSCRIPTION_WORD, SUBSCRIPTIONS_WORD, format_usd, marked_amount};

pub const CLOSED_PAUSE_NOTE: &str = "Wisp closed the paused reply above because it couldn't be \
continued; continue the task from where you left off.";

pub use hook::{HookChange, HookOutcome};

pub use instructions::{
    CandidateInfo, Diagnostic, DiagnosticKind, InstructionsSource, InstructionsSummary, Scope,
    SourceChange, SourceInfo,
};

pub use media::{AttachmentRef, BlobRef, MAX_MIME_LEN, MediaSource, Mime};

pub use names::{
    ACCOUNT_FLAG, ACCOUNT_NAME_LABEL, ACCOUNT_NAME_RULE, CLAUDE_API, CONFIG_FILE, CREDENTIALS,
    DEFAULT_CONFIG_FILE, DEFAULT_KEYS_FILE, GATEWAY, GEMINI_API, GIVEN_CONFIG, GIVEN_CREDENTIALS,
    GIVEN_ENV_FILE, IN_KEYCHAIN, KEY_LETTER_RUN, KEY_RUN, KEYS_FILE, KEYS_FILE_ACCOUNT,
    KEYS_FILE_ALIAS, KEYS_FILE_NAMES, LOCK_FILE, MAX_ENDPOINT_NAME, MAX_MODEL_ID, MODEL_ID_CHARS,
    MODEL_IDS_RULE, MODEL_PLACEHOLDER, ModelIdProblem, NOT_A_MODEL_ID, OPENAI_API, THIS_MODEL,
    account_name, account_name_error, account_name_problem, account_name_reserved,
    account_name_taken, bad_account_name, check_model_id, default_api_account_name, endpoint_char,
    endpoint_name_rule, hide_models, hide_quoted_models, keys_file_name, model_hidden,
    model_id_char, model_looks_like_key, model_or_placeholder, model_words, setting_looks_like_key,
    shown_model, shown_text, valid_endpoint_name, valid_model_id,
};

pub use opening::{Continued, Opening, StartSaid};
pub use paths::{INVALID_SESSION_ID, short_id, shorten_path, valid_id};
pub use plugin::{
    BUILT_IN_COMMANDS, Catalog, CommandLine, CommandOutcome, CommandRefusal, CommandWait,
    DoctorCell, DoctorRow, PluginEntry, PluginRecord, PluginSource,
};

pub use process::{
    AttachInput, AttachItems, AttachKey, AttachKeyCode, AttachRequestEnd, BackgroundReason,
    DetachReason, LostReason, MAX_FUNCTION_KEY, NeedsInputKind, NoTerminalCode, ProcessEnd,
    ReportedEnd, SecretRequestEnd, SignalKind, StoppedBy, TermSize, TurnOrigin, control_byte,
};

pub use question::{
    Answer, AskedBy, Question, QuestionEnd, QuestionOption, QuestionSource, answer_refusal,
    check_answers, report_refusal, secret_refusal,
};

pub use retry::{RetryFocus, RetryOffer};

pub use rewind::{
    CompactionNote, Dropped, MissingImage, NOTHING_TO_REWIND, PointInfo, REWIND_CANCELLED,
    REWIND_SESSION_ENDED, REWIND_WHILE_COMPACTING, REWIND_WHILE_RUNNING, ReturnedInput,
    RewindPoint, RewindQuote, RewindReason, SENT_BEFORE_REWIND, Warmth, not_a_point, rewound_away,
};

pub use search::{
    CitedSource, NoSearch, RenderingForm, Resend, SEARCH_CITATION, SEARCH_RESULT, SERVER_TOOL_USE,
    SearchAction, SearchEnd, SearchFailure, SearchForm, SearchHit, SearchOutcome, UnfinishedReason,
    WEB_SEARCH,
};

pub use secret::{
    CodeProblem, Ends, KEY_MAX, KEY_PREFIXES, KeyProblem, MASK_SHORT, SECRET_FIELDS, Secret,
    check_key, check_secret, has_known_key_prefix, key_tail, masked, masked_ends, split_code_state,
};
pub use variable::{
    KEY_SHAPED_MIN, NOT_SHOWN, VAR_NAME_CHARS, VariableName, is_variable_name, looks_like_key,
    plain_variable, shown_variable,
};

pub use todos::{TodoItem, TodoProgress, TodoStatus};
pub use tool::{
    DiffView, ExitStatus, Fact, NotRunReason, OutputKind, ToolAction, ToolChangeVia, ToolDisplay,
    ToolOutcome, ToolOutputView, ToolPurpose, ToolView, Typed, Verbs, WorkspaceChange,
};
pub use transformation::InputTransformation;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Role {
    User,
    Assistant,
    System,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Message {
    pub role: Role,
    pub content: Vec<ContentBlock>,
}

impl Message {
    pub fn user_text(text: impl Into<String>) -> Self {
        Self {
            role: Role::User,
            content: vec![ContentBlock::Text { text: text.into() }],
        }
    }

    pub fn user_stamped(stamp: Option<&str>, text: impl Into<String>) -> Self {
        Self {
            role: Role::User,
            content: user_blocks(None, stamp, text),
        }
    }

    pub fn effort(marker: EffortMarker) -> Self {
        Self {
            role: Role::System,
            content: vec![ContentBlock::Effort {
                level: marker.level,
                rendered: marker.rendered,
            }],
        }
    }

    pub fn turn_system(marker: Option<EffortMarker>, tools: &[ContentBlock]) -> Option<Self> {
        let mut content: Vec<ContentBlock> = marker
            .map(|m| ContentBlock::Effort {
                level: m.level,
                rendered: m.rendered,
            })
            .into_iter()
            .collect();
        content.extend(tools.iter().cloned());
        (!content.is_empty()).then_some(Self {
            role: Role::System,
            content,
        })
    }

    pub fn effort_marker(&self) -> Option<EffortMarker> {
        match (self.role, self.content.first()) {
            (Role::System, Some(ContentBlock::Effort { level, rendered })) => Some(EffortMarker {
                level: *level,
                rendered: *rendered,
            }),
            _ => None,
        }
    }

    pub fn is_turn_system(&self) -> bool {
        self.role == Role::System
            && !self.content.is_empty()
            && self.content.iter().all(|b| {
                matches!(
                    b,
                    ContentBlock::Effort { .. }
                        | ContentBlock::ToolAddition { .. }
                        | ContentBlock::ToolRemoval { .. }
                        | ContentBlock::Text { .. }
                )
            })
            && self
                .content
                .iter()
                .skip(1)
                .all(|b| !matches!(b, ContentBlock::Effort { .. }))
    }

    pub fn tool_blocks(&self) -> &[ContentBlock] {
        match (self.role, self.content.first()) {
            (Role::System, Some(ContentBlock::Effort { .. })) => {
                self.content.get(1..).unwrap_or(&[])
            }
            (Role::System, _) => &self.content,
            _ => &[],
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct EffortMarker {
    pub level: Effort,
    pub rendered: bool,
}

pub fn user_blocks(
    context: Option<&str>,
    stamp: Option<&str>,
    text: impl Into<String>,
) -> Vec<ContentBlock> {
    user_blocks_with_images(context, stamp, Vec::new(), text)
}

pub fn user_blocks_with_images(
    context: Option<&str>,
    stamp: Option<&str>,
    images: Vec<ContentBlock>,
    text: impl Into<String>,
) -> Vec<ContentBlock> {
    let mut content: Vec<ContentBlock> = context
        .into_iter()
        .chain(stamp)
        .map(|s| ContentBlock::Text { text: s.to_owned() })
        .collect();
    content.extend(images);
    content.push(ContentBlock::Text { text: text.into() });
    content
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ImageData {
    pub data: Arc<str>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub enum ImageType {
    #[serde(rename = "image/png")]
    Png,
    #[serde(rename = "image/jpeg")]
    Jpeg,
}

impl ImageType {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Png => "image/png",
            Self::Jpeg => "image/jpeg",
        }
    }

    pub fn extension(self) -> &'static str {
        match self {
            Self::Png => "png",
            Self::Jpeg => "jpg",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ImageSource {
    Base64 {
        media_type: ImageType,
        data: Arc<str>,
    },
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ContentBlock {
    Text {
        text: String,
    },
    Thinking {
        thinking: String,
        signature: String,
    },
    RedactedThinking {
        data: String,
    },
    ToolUse {
        id: String,
        name: String,
        input: Value,
    },
    ToolResult {
        tool_use_id: String,
        content: Vec<ContentBlock>,
        #[serde(default)]
        is_error: bool,
    },
    Opaque {
        provider: String,
        raw: Value,
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        deltas: Vec<Value>,
    },
    Effort {
        level: Effort,
        rendered: bool,
    },
    Image {
        source: ImageSource,
    },
    Media {
        source: MediaSource,
    },
    ToolAddition {
        tool: ToolRef,
    },
    ToolRemoval {
        tool: ToolRef,
    },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ToolRef {
    ToolReference { name: String },
    ToolDefinition { definition: ToolDefinition },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ToolDefinition {
    pub description: String,
    pub input_schema: Value,
    pub name: String,
}

impl ToolRef {
    pub fn name(&self) -> &str {
        match self {
            Self::ToolReference { name } => name,
            Self::ToolDefinition { definition } => &definition.name,
        }
    }

    pub fn by_value(&self) -> bool {
        matches!(self, Self::ToolDefinition { .. })
    }
}

impl ContentBlock {
    pub fn tool_change(&self) -> Option<&ToolRef> {
        match self {
            Self::ToolAddition { tool } | Self::ToolRemoval { tool } => Some(tool),
            _ => None,
        }
    }
}

impl ClientCommand {
    pub fn send(id: impl Into<String>, text: impl Into<String>) -> Self {
        Self::Send {
            id: InputId(id.into()),
            text: text.into(),
            images: Vec::new(),
            mode: DeliveryMode::Steer,
            turn: None,
            replaces: None,
        }
    }

    pub fn queue(id: impl Into<String>, text: impl Into<String>) -> Self {
        Self::Send {
            id: InputId(id.into()),
            text: text.into(),
            images: Vec::new(),
            mode: DeliveryMode::Queue,
            turn: None,
            replaces: None,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(transparent)]
pub struct InputId(pub String);

impl InputId {
    pub fn as_str(&self) -> &str {
        &self.0
    }

    pub fn highest_numbered<'a>(prefix: &str, ids: impl IntoIterator<Item = &'a str>) -> u64 {
        ids.into_iter()
            .filter_map(|id| id.strip_prefix(prefix)?.parse().ok())
            .max()
            .unwrap_or(0)
    }
}

impl fmt::Display for InputId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DeliveryMode {
    Steer,
    Queue,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum WithdrawnBy {
    User,
    TurnStopped,
    TurnFailed,
    SessionEnded,
    Replaced,
    NotKept,
    Rewound,
    Hook { hook: String, reason: String },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RejectKind {
    Empty,
    Image,
    TooMany,
    Duplicate,
    AlreadyDelivered,
    UnknownId,
    Withdrawn,
    NothingHeld,
    NothingRunning,
    NoQuestion,
    BadAnswer,
    NoProcess,
    ProcessEnded,
    NotMovable,
    NotATerminal,
    AlreadyAttached,
    NotAttached,
    InputTooLarge,
    NoAttachRequest,
    TurnRunning,
    NothingToCompact,
    Compacting,
    Endpoint,
    NotRetryable,
    NotRewindable,
    InputBlocked,
    InputCancelled,
    AccountGone,
    #[serde(untagged)]
    Other(String),
}

pub const NOTHING_RUNNING: &str = "nothing is running";
pub const NOTHING_TO_STEER: &str = "nothing is running to steer; enter sends held messages";
pub const NOTHING_HELD: &str = "no held messages to send";
pub const NOT_MOVABLE: &str = "only a running foreground command can move to the background";

pub fn process_refusal(kind: &RejectKind, id: impl std::fmt::Display) -> String {
    match kind {
        RejectKind::ProcessEnded => format!("process {id} has already ended"),
        RejectKind::NotMovable => NOT_MOVABLE.to_owned(),
        RejectKind::NotATerminal => format!("process {id} is not a terminal"),
        RejectKind::AlreadyAttached => format!("another client has taken over process {id}"),
        RejectKind::NotAttached => format!("you have not taken over process {id}"),
        RejectKind::InputTooLarge => format!("input for process {id} is over 1 MiB"),
        RejectKind::NoAttachRequest => format!("no take-over request {id} is open"),
        _ => format!("there is no process {id}"),
    }
}

impl ContentBlock {
    pub fn image(media_type: ImageType, data: impl Into<Arc<str>>) -> Self {
        Self::Image {
            source: ImageSource::Base64 {
                media_type,
                data: data.into(),
            },
        }
    }

    pub fn media(media_type: Mime, data: impl Into<Arc<str>>) -> Self {
        Self::Media {
            source: MediaSource::Base64 {
                media_type,
                data: data.into(),
            },
        }
    }

    pub fn opaque(provider: impl Into<String>, raw: Value) -> Self {
        Self::Opaque {
            provider: provider.into(),
            raw,
            deltas: Vec::new(),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Effort {
    Low,
    Medium,
    High,
    Xhigh,
    Max,
}

impl Effort {
    pub const ALL: [Effort; 5] = [Self::Low, Self::Medium, Self::High, Self::Xhigh, Self::Max];
    pub const WORDS: [&str; Self::ALL.len()] = ["low", "medium", "high", "xhigh", "max"];

    pub const fn as_str(self) -> &'static str {
        Self::WORDS[self as usize]
    }
}

impl FromStr for Effort {
    type Err = ParseError;
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        settings::named(&Self::WORDS, &Self::ALL, s)
            .ok_or_else(|| unknown("effort", s, &Self::WORDS))
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CacheTtl {
    FiveMinutes,
    OneHour,
}

impl CacheTtl {
    pub const ALL: [CacheTtl; 2] = [Self::FiveMinutes, Self::OneHour];
    pub const WORDS: [&str; Self::ALL.len()] = ["5m", "1h"];

    pub const fn as_str(self) -> &'static str {
        Self::WORDS[self as usize]
    }
}

impl FromStr for CacheTtl {
    type Err = ParseError;
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        settings::named(&Self::WORDS, &Self::ALL, s)
            .ok_or_else(|| unknown("cache TTL", s, &Self::WORDS))
    }
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MessageTime {
    #[default]
    Local,
    Utc,
    Off,
}

impl MessageTime {
    pub const ALL: [MessageTime; 3] = [Self::Local, Self::Utc, Self::Off];
    pub const WORDS: [&str; Self::ALL.len()] = ["local", "utc", "off"];

    pub const fn as_str(self) -> &'static str {
        Self::WORDS[self as usize]
    }
}

impl FromStr for MessageTime {
    type Err = ParseError;
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        settings::named(&Self::WORDS, &Self::ALL, s)
            .ok_or_else(|| unknown("time mode", s, &Self::WORDS))
    }
}

fn unknown(what: &str, s: &str, words: &[&str]) -> ParseError {
    let expected = settings::listed(words, "or");
    ParseError(format!("unknown {what} `{s}` (expected {expected})"))
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ParseError(pub String);

impl fmt::Display for ParseError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for ParseError {}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SessionConfig {
    pub model: String,
    pub system: Option<String>,
    pub effort: Effort,
    pub cache_ttl: CacheTtl,
    pub max_tokens: u32,
    #[serde(default)]
    pub message_time: MessageTime,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub instructions: Option<String>,
    #[serde(default = "context_note_default", skip_serializing_if = "is_on")]
    pub context_note: bool,
}

fn context_note_default() -> bool {
    true
}

#[expect(
    clippy::trivially_copy_pass_by_ref,
    reason = "serde's skip_serializing_if passes a reference"
)]
fn is_on(value: &bool) -> bool {
    *value
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(transparent)]
pub struct RequestId(pub u64);

pub const WISP_JOB: &str = "WISP_JOB";

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ChatRow {
    pub id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    #[serde(default)]
    pub topic: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub typed: String,
    #[serde(default)]
    pub active_at: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub account: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mark: Option<u8>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
    #[serde(default, skip_serializing_if = "is_zero")]
    pub agents: u32,
    #[serde(default, skip_serializing_if = "is_false")]
    pub open_elsewhere: bool,
    #[serde(default, skip_serializing_if = "is_false")]
    pub hosted: bool,
}

impl ChatRow {
    pub fn can_open(&self) -> bool {
        !self.open_elsewhere || self.hosted
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ClientCommand {
    SubscriptionAuth {
        command: subscription_auth::AuthCommand,
    },
    PlanUsage {
        request: RequestId,
        endpoint: String,
        action: plan_usage::UsageAction,
    },
    Send {
        id: InputId,
        text: String,
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        images: Vec<ImageData>,
        mode: DeliveryMode,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        turn: Option<u64>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        replaces: Option<InputId>,
    },
    Withdraw {
        ids: Vec<InputId>,
    },
    Release,
    Cancel,
    Answer {
        call_id: String,
        answers: Vec<Answer>,
    },
    ScreenReport {
        call_id: String,
        report: Value,
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        images: Vec<ImageData>,
    },
    ChangeSettings {
        request: RequestId,
        changes: Vec<settings::SettingChange>,
        #[serde(default, skip_serializing_if = "is_false")]
        save: bool,
    },
    SettingsUsed {
        names: Vec<String>,
    },
    DescribeSettings {
        request: RequestId,
    },
    ContextReport {
        detail: context::ReportDetail,
        request: RequestId,
    },
    StopProcess {
        id: u32,
    },
    CheckProcesses,
    Background {
        call_id: String,
    },
    AttachProcess {
        id: u32,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        size: Option<TermSize>,
    },
    ProcessInput {
        id: u32,
        items: AttachItems,
    },
    DetachProcess {
        id: u32,
    },
    DeclineAttach {
        call_id: String,
    },
    AnswerSecret {
        call_id: String,
        value: Secret,
    },
    SaveEndpoint {
        request: RequestId,
        endpoint: endpoint::EndpointSpec,
        key: Secret,
        check: bool,
    },
    EditEndpoint {
        request: RequestId,
        name: String,
        edit: endpoint::EndpointEdit,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        store: Option<endpoint::StoreChoice>,
        check: bool,
    },
    RemoveEndpoint {
        request: RequestId,
        name: String,
        #[serde(default, skip_serializing_if = "std::ops::Not::not")]
        keep_key: bool,
    },
    Compact {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        focus: Option<String>,
        request: RequestId,
    },
    Retry {
        turn: u64,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        endpoint: Option<String>,
        request: RequestId,
    },
    RewindPoints {
        request: RequestId,
    },
    ListChats {
        request: RequestId,
    },
    ListSpend {
        request: RequestId,
    },
    StartOn {
        request: RequestId,
        account: String,
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        follow: Vec<String>,
    },
    Rewind {
        to: RewindPoint,
        request: RequestId,
    },
    Child {
        agent: agents::AgentPath,
        command: Box<ClientCommand>,
    },
    SpawnAgent {
        request: RequestId,
        spawn: agents::SpawnRequest,
    },
    ListAgents {
        request: RequestId,
    },
    DescribeSession {
        request: RequestId,
    },
    ReturnedTaken {
        ids: Vec<InputId>,
    },
    WindowGone,
    GetReady {
        on: bool,
    },
    EndAgent {
        agent: agents::AgentPath,
    },
    RenameAgent {
        request: RequestId,
        agent: agents::AgentPath,
        name: String,
    },
    WatchAgent {
        agent: agents::AgentPath,
        live: bool,
    },
    SlotsShown {
        sections: Vec<String>,
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        pages: Vec<String>,
    },
    Terminal {
        size: TermSize,
    },
    RunPluginCommand {
        name: String,
        #[serde(default)]
        args: Secret,
        request: RequestId,
    },
    Voice {
        command: voice::VoiceCommand,
    },
}

impl ClientCommand {
    pub fn shortcut(
        request: RequestId,
        endpoint: Option<String>,
        model: Option<String>,
        effort: Option<Effort>,
    ) -> Self {
        Self::ChangeSettings {
            request,
            changes: Self::shortcut_changes(endpoint, model, effort),
            save: false,
        }
    }

    pub fn shortcut_changes(
        endpoint: Option<String>,
        model: Option<String>,
        effort: Option<Effort>,
    ) -> Vec<settings::SettingChange> {
        let named = [
            (settings::names::ENDPOINT, endpoint),
            (settings::names::MODEL, model),
            (
                settings::names::EFFORT,
                effort.map(|e| e.as_str().to_owned()),
            ),
        ];
        let changes = named
            .into_iter()
            .filter_map(|(name, value)| {
                settings::SettingChange::named(name, settings::SettingValue::Text(value?))
            })
            .collect();
        settings::sorted_changes(changes)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Feature {
    PerMessageEffort,
    BindingControls,
    NativeWebSearch,
    NativeCompaction,
    InlineTools,
    DeferredTools,
}

impl Feature {
    pub const ALL: [Feature; 6] = [
        Self::PerMessageEffort,
        Self::BindingControls,
        Self::NativeWebSearch,
        Self::NativeCompaction,
        Self::InlineTools,
        Self::DeferredTools,
    ];
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum CacheEffect {
    Kept,
    KeptIfPassedOn,
    Rebuilt { reason: RebuildReason },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RebuildReason {
    ModelSwitch,
    EffortChange,
    PerMessageEffortRejected,
    EndpointSwitch,
    NativeSearchRefused,
    CacheTtlChange,
    RequestOptionChange,
    Compaction,
    Rewind,
    ToolsChange,
    #[serde(untagged)]
    Other(String),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AttachmentKind {
    Image,
    File,
}

impl AttachmentKind {
    pub fn noun(self) -> &'static str {
        match self {
            Self::Image => "image",
            Self::File => "file",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WithdrawnAttachment {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub call_id: Option<String>,
    pub ordinal: usize,
    pub kind: AttachmentKind,
    pub label: String,
    pub reason: String,
    pub note: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SecretDestination {
    pub server: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub command: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RestartState {
    GettingReady,
    Ready,
    Cancelled,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Unready {
    pub agent: AgentPath,
    pub what: Unfinished,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    pub target: String,
    pub running_ms: u64,
    #[serde(default, skip_serializing_if = "is_false")]
    pub opening: bool,
}

pub fn age_unready(open: &mut [Unready], elapsed: std::time::Duration) {
    let elapsed = u64::try_from(elapsed.as_millis()).unwrap_or(u64::MAX);
    for row in open {
        row.running_ms = row.running_ms.saturating_add(elapsed);
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Unfinished {
    Reply,
    ToolCall,
    Process,
    #[serde(other)]
    Other,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum AgentEvent {
    Restart {
        state: RestartState,
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        open: Vec<Unready>,
    },
    ReadyToRestart,
    SubscriptionAuth {
        event: subscription_auth::AuthEvent,
    },
    Voice {
        event: voice::VoiceEvent,
    },
    AccountsGone {
        names: Vec<String>,
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        moved: Vec<subscription_auth::Followed>,
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        stay: Vec<subscription_auth::Stayed>,
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        ways: Vec<subscription_auth::Way>,
    },
    LeaveNeeds {
        request: RequestId,
        ways: Vec<subscription_auth::Way>,
    },
    PlanUsageWarning {
        endpoint: String,
        window: plan_usage::UsageWindow,
        threshold: u8,
    },
    PlanUsageChanged {
        request: Option<RequestId>,
        endpoint: String,
        state: plan_usage::UsageState,
    },
    TurnStarted {
        turn: u64,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        origin: Option<TurnOrigin>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        retry_of: Option<u64>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        answers: Option<RequestId>,
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        ends: Vec<ReportedEnd>,
    },
    InputAccepted {
        #[serde(default, skip_serializing_if = "is_false")]
        opening: bool,
        id: InputId,
        mode: DeliveryMode,
        text: String,
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        images: Vec<log::ImageRef>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        replaces: Option<InputId>,
        held: bool,
        #[serde(default, skip_serializing_if = "is_false")]
        front: bool,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        from: Option<agents::MailFrom>,
    },
    InputHeld {
        ids: Vec<InputId>,
        by: WithdrawnBy,
    },
    InputUnheld {
        ids: Vec<InputId>,
        #[serde(default, skip_serializing_if = "is_false")]
        front: bool,
    },
    InputDelivered {
        ids: Vec<InputId>,
        step: u32,
    },
    InputWithdrawn {
        ids: Vec<InputId>,
        by: WithdrawnBy,
    },
    AttachmentsWithdrawn {
        step: u32,
        items: Vec<WithdrawnAttachment>,
    },
    Retrying {
        attempt: u32,
        delay_ms: u64,
        reason: String,
        #[serde(default)]
        step: u32,
        #[serde(default = "unknown_kind")]
        kind: ErrorKind,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        detail: Option<ErrorDetail>,
        #[serde(default)]
        max_attempts: u32,
        #[serde(default)]
        discarded_blocks: u32,
        #[serde(default)]
        discarded_chars: u64,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        usage: Option<Usage>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        model: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        cost_usd: Option<f64>,
        #[serde(default, skip_serializing_if = "is_false")]
        lower_bound: bool,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        resend: Option<Resend>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        busy: Option<BusyWait>,
    },
    BlockStarted {
        index: usize,
        kind: BlockKind,
    },
    TextDelta {
        index: usize,
        text: String,
    },
    ThinkingDelta {
        index: usize,
        text: String,
    },
    BlockStopped {
        index: usize,
    },
    Fallback {
        from: String,
        to: String,
    },
    CacheMiss {
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
        cause: MissCause,
        count: u32,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        usd: Option<f64>,
    },
    CacheRefreshed {
        outcome: RefreshOutcome,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        detail: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        usage: Option<Usage>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        cost: Option<Cost>,
    },
    ModelAsked {
        call_id: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        cost: Option<Cost>,
    },
    SessionNamed {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        name: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        cost: Option<Cost>,
    },
    CacheDiagnostics {
        diagnostics: Value,
    },
    Rejected {
        reason: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        id: Option<InputId>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        kind: Option<RejectKind>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        request: Option<RequestId>,
    },
    WakeHeld {
        sources: Vec<agents::WakeSource>,
        reason: agents::HoldReason,
    },
    AgentRefused {
        by: agents::AgentPath,
        action: agents::AgentAction,
        reason: String,
    },
    AgentSpawned {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        request: Option<RequestId>,
        agent: agents::AgentEntry,
        session_id: String,
        config: agents::SpawnChoices,
    },
    AgentRenamed {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        request: Option<RequestId>,
        agent: agents::AgentPath,
        to: agents::AgentPath,
    },
    AgentStateChanged {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        plan_usd: Option<f64>,
        #[serde(default, skip_serializing_if = "Billing::is_metered")]
        billing: Billing,
        agent: agents::AgentPath,
        state: agents::AgentState,
        activity: agents::Activity,
        spent_usd: f64,
        #[serde(default, skip_serializing_if = "is_false")]
        lower_bound: bool,
        #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
        accounts: BTreeMap<String, AccountSpend>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        since_ms: Option<u64>,
    },
    SpendChanged {
        total: AccountSpend,
        #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
        accounts: BTreeMap<String, AccountSpend>,
    },
    KeepWarmChanged {
        agent: agents::AgentPath,
        keep_warm: agents::KeepWarm,
    },
    AgentReported {
        agent: agents::AgentPath,
        turn: u64,
        outcome: agents::ReportOutcome,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        report: Option<Value>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        cost: Option<f64>,
        duration_ms: u64,
    },
    Agent {
        agent: agents::AgentPath,
        event: Box<AgentEvent>,
    },
    SessionSnapshot {
        answers: RequestId,
        view: SessionView,
        now: Now,
        state: Vec<AgentEvent>,
    },
    AgentsSnapshot {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        request: Option<RequestId>,
        agents: Vec<agents::AgentEntry>,
    },
    UnknownProviderEvent {
        provider: String,
        event: Value,
    },
    InputTransformations {
        provider: String,
        transformations: Vec<InputTransformation>,
    },
    LogDisabled {
        reason: String,
    },
    SessionResumed {
        turns: u64,
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        moved: Vec<subscription_auth::Followed>,
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        stay: Vec<subscription_auth::Stayed>,
    },
    TurnEnded {
        outcome: TurnOutcome,
        stats: TurnStats,
    },
    StepStarted {
        step: u32,
    },
    RequestQueued {
        step: u32,
        endpoint: String,
        ahead: u32,
    },
    RequestAdmitted {
        step: u32,
        waited_ms: u64,
    },
    ToolCallRequested {
        step: u32,
        block: usize,
        call_id: String,
        tool: String,
        input: Value,
        display: ToolDisplay,
    },
    ToolCallStarted {
        call_id: String,
        cwd: String,
        #[serde(default, skip_serializing_if = "std::ops::Not::not")]
        movable: bool,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        file: Option<String>,
    },
    StepEnded {
        step: u32,
        usage: Option<Usage>,
        cost_usd: Option<f64>,
        lower_bound: bool,
    },
    ToolOutput {
        call_id: String,
        text: String,
    },
    ToolCallFinished {
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
        #[serde(default)]
        notes: Vec<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        typed: Option<Typed>,
    },
    QuestionAsked {
        call_id: String,
        questions: Vec<Question>,
    },
    QuestionClosed {
        call_id: String,
        how: QuestionEnd,
    },
    ToolsChanged {
        added: Vec<String>,
        removed: Vec<String>,
        via: ToolChangeVia,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        cache: Option<CacheEffect>,
    },
    SettingsChanged {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        answers: Option<RequestId>,
        endpoint: String,
        model: String,
        effort: Effort,
        cache: CacheEffect,
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        account_bound: Vec<String>,
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        changes: Vec<settings::AppliedChange>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        withdrawn: Option<settings::Withdrawal>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        restored: Option<settings::Restored>,
    },
    SettingsSnapshot(settings::SettingsSnapshot),
    FeatureStateChanged {
        endpoint: String,
        model: String,
        feature: Feature,
        state: log::FeatureState,
    },
    SearchNotRun {
        endpoint_name: String,
        model: String,
    },
    HistoryAdapted {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        endpoint_name: Option<String>,
        change: HistoryChange,
    },
    SearchStarted {
        step: u32,
        attempt: u32,
        call_id: String,
        query: String,
        #[serde(default, skip_serializing_if = "SearchAction::is_search")]
        action: SearchAction,
    },
    SearchEnded {
        step: u32,
        attempt: u32,
        call_id: String,
        outcome: SearchEnd,
    },
    Citations {
        step: u32,
        attempt: u32,
        index: usize,
        sources: Vec<CitedSource>,
    },
    InstructionsLoaded {
        turn: Option<u64>,
        chosen: Option<SourceInfo>,
        imports: Vec<SourceInfo>,
        candidates: Vec<CandidateInfo>,
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
        from: Option<Box<SourceInfo>>,
        to: Option<Box<SourceInfo>>,
        changed: Vec<SourceChange>,
        chosen: Option<Box<SourceInfo>>,
        imports: Vec<SourceInfo>,
        candidates: Vec<CandidateInfo>,
        sent: bool,
        diagnostics: Vec<Diagnostic>,
    },
    InstructionsWarning {
        diagnostic: Diagnostic,
    },
    ContextChanged {
        used: u64,
        used_estimated: bool,
        next: u64,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        window: Option<u64>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        compact_at: Option<u64>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        meter_limit: Option<u64>,
        #[serde(default)]
        auto_compact: bool,
        epoch: u64,
    },
    ContextReport(context::ContextReport),
    WindowLowered {
        endpoint: String,
        model: String,
        tokens: u64,
    },
    TodosUpdated {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        call_id: Option<String>,
        items: Vec<TodoItem>,
    },
    PastCompactionPoint {
        endpoint: String,
        model: String,
    },
    AutoCompactionHeld {
        tokens: u64,
        point: u64,
    },
    ProcessStarted {
        id: u32,
        call_id: String,
        target: String,
        tty: bool,
        stdin: bool,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        size: Option<TermSize>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        file: Option<String>,
        reason: BackgroundReason,
        #[serde(default, skip_serializing_if = "is_false")]
        keeps_running: bool,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        pid: Option<u32>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        log: Option<String>,
    },
    ProcessOutput {
        id: u32,
        text: String,
        skipped: u64,
    },
    ProcessResized {
        id: u32,
        size: TermSize,
    },
    ProcessEnded {
        id: u32,
        end: ProcessEnd,
        bytes: u64,
        lines: u64,
        duration_ms: u64,
    },
    ProcessRestored {
        id: u32,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        call_id: Option<String>,
        target: String,
        tty: bool,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        end: Option<ProcessEnd>,
        duration_ms: u64,
        #[serde(default, skip_serializing_if = "is_false")]
        keeps_running: bool,
        #[serde(default, skip_serializing_if = "is_false")]
        unchecked: bool,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        pid: Option<u32>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        log: Option<String>,
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
    },
    ProcessNeedsInput {
        id: u32,
        kind: NeedsInputKind,
    },
    ProcessNeedsInputCleared {
        id: u32,
    },
    AttachRequested {
        call_id: String,
        id: u32,
        reason: String,
    },
    AttachRequestClosed {
        call_id: String,
        how: AttachRequestEnd,
    },
    SecretRequested {
        call_id: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        id: Option<u32>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        destination: Option<SecretDestination>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        asked_by: Option<AskedBy>,
        name: String,
        reason: String,
        show: Ends,
    },
    SecretRequestClosed {
        call_id: String,
        how: SecretRequestEnd,
    },
    EndpointSaved {
        answers: RequestId,
        name: String,
        store: endpoint::StoreKind,
        check: endpoint::CheckResult,
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        notes: Vec<String>,
    },
    EndpointNotSaved {
        answers: RequestId,
        name: String,
        why: endpoint::NotSavedReason,
    },
    EndpointEdited {
        answers: RequestId,
        name: String,
        changed: Vec<endpoint::ConnectionField>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        store: Option<endpoint::StoreKind>,
        check: endpoint::CheckResult,
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        notes: Vec<String>,
    },
    EndpointRemoved {
        answers: RequestId,
        name: String,
        kept_item: bool,
        default_cleared: bool,
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        notes: Vec<String>,
    },
    EndpointsChanged {
        endpoints: Vec<settings::EndpointSummary>,
    },
    CatalogChanged {
        catalog: plugin::Catalog,
    },
    PluginsChanged {
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        added: Vec<PluginRecord>,
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        updated: Vec<PluginRecord>,
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        removed: Vec<PluginRecord>,
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        kept: Vec<PluginRecord>,
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        notes: Vec<plugin::PluginNote>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        links: Option<Vec<plugin::LinkNote>>,
    },
    SlotContent {
        section: String,
        #[serde(default, skip_serializing_if = "slot::SlotView::is_panel")]
        view: slot::SlotView,
        content: slot::SlotContent,
    },
    SlotFailed {
        section: String,
        #[serde(default, skip_serializing_if = "slot::SlotView::is_panel")]
        view: slot::SlotView,
        failure: slot::SlotFailure,
        parked: bool,
        #[serde(default, skip_serializing_if = "is_false")]
        restarted: bool,
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
    CommandDone {
        answers: RequestId,
        outcome: CommandOutcome,
    },
    CommandDeferred {
        request: RequestId,
        until: CommandWait,
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
    CompactionStarted {
        trigger: CompactionTrigger,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        turn: Option<u64>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        switch: Option<SwitchCompaction>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        requested_by: Option<String>,
    },
    Compacted {
        trigger: CompactionTrigger,
        engine: CompactionEngine,
        before: u64,
        after: u64,
        #[serde(default, skip_serializing_if = "is_false")]
        partial: bool,
        #[serde(default, skip_serializing_if = "is_false")]
        converted: bool,
        cache_effect: CacheEffect,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        usage: Option<Usage>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        cost: Option<Cost>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        answers: Option<RequestId>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        turn: Option<u64>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        summary: Option<String>,
        #[serde(default, skip_serializing_if = "is_false")]
        paused: bool,
    },
    CompactionPrepared {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        covers: Option<Covers>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        usage: Option<Usage>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        cost: Option<Cost>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        turn: Option<u64>,
    },
    CompactionFailed {
        trigger: CompactionTrigger,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        engine: Option<CompactionEngine>,
        kind: CompactionFailure,
        message: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        usage: Option<Usage>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        cost: Option<Cost>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        answers: Option<RequestId>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        turn: Option<u64>,
        #[serde(default, skip_serializing_if = "is_false")]
        paused: bool,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        requested_by: Option<String>,
    },
    RewindPoints {
        answers: RequestId,
        points: Vec<PointInfo>,
    },
    ChatsListed {
        request: RequestId,
        chats: Vec<ChatRow>,
    },
    SpendListed {
        request: RequestId,
        days: Vec<SpendDay>,
        window: SpendWindow,
    },
    Rewound {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        answers: Option<RequestId>,
        point: RewindPoint,
        reason: RewindReason,
        dropped: Dropped,
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        returned: Vec<ReturnedInput>,
        cache: CacheEffect,
    },
}

impl AgentEvent {
    pub fn private_auth_request(&self) -> Option<RequestId> {
        match self {
            Self::SubscriptionAuth { event } if event.is_private() => Some(event.request),
            Self::Agent { event, .. } => event.private_auth_request(),
            _ => None,
        }
    }

    pub fn without_links(self) -> Option<Self> {
        match self {
            Self::PluginsChanged {
                added,
                updated,
                removed,
                kept,
                notes,
                ..
            } => {
                if added.is_empty()
                    && updated.is_empty()
                    && removed.is_empty()
                    && kept.is_empty()
                    && notes.is_empty()
                {
                    None
                } else {
                    Some(Self::PluginsChanged {
                        added,
                        updated,
                        removed,
                        kept,
                        notes,
                        links: None,
                    })
                }
            }
            Self::Agent { agent, event } => event.without_links().map(|event| Self::Agent {
                agent,
                event: Box::new(event),
            }),
            event => Some(event),
        }
    }

    pub fn links_mut(&mut self) -> &mut [plugin::LinkNote] {
        match self {
            Self::PluginsChanged { links, .. } => links.as_deref_mut().unwrap_or_default(),
            Self::Agent { event, .. } => event.links_mut(),
            _ => &mut [],
        }
    }

    pub fn is_private_auth(&self) -> bool {
        self.private_auth_request().is_some()
    }

    pub fn is_account_telemetry(&self) -> bool {
        match self {
            Self::PlanUsageChanged { .. } | Self::PlanUsageWarning { .. } => true,
            Self::Agent { event, .. } => event.is_account_telemetry(),
            _ => false,
        }
    }

    pub fn turn_started(turn: u64) -> Self {
        Self::TurnStarted {
            turn,
            origin: None,
            retry_of: None,
            answers: None,
            ends: Vec::new(),
        }
    }

    pub fn retry_started(turn: u64, retry_of: u64, answers: Option<RequestId>) -> Self {
        Self::TurnStarted {
            turn,
            origin: None,
            retry_of: Some(retry_of),
            answers,
            ends: Vec::new(),
        }
    }

    pub fn reporting(mut self, reported: Vec<ReportedEnd>) -> Self {
        if let Self::TurnStarted { ends, .. } = &mut self {
            *ends = reported;
        }
        self
    }

    pub fn call_started(call_id: impl Into<String>, cwd: impl Into<String>) -> Self {
        Self::ToolCallStarted {
            call_id: call_id.into(),
            cwd: cwd.into(),
            movable: false,
            file: None,
        }
    }

    pub fn call_finished(call_id: impl Into<String>, outcome: ToolOutcome) -> Self {
        Self::ToolCallFinished {
            call_id: call_id.into(),
            outcome,
            output: ToolOutputView::default(),
            facts: Vec::new(),
            state: None,
            duration_ms: 0,
            result: None,
            process: None,
            attachments: Vec::new(),
            notes: Vec::new(),
            typed: None,
        }
    }

    pub fn rejected(reason: impl Into<String>) -> Self {
        Self::refused(None, reason)
    }

    pub fn refused(request: impl Into<Option<RequestId>>, reason: impl Into<String>) -> Self {
        Self::Rejected {
            reason: reason.into(),
            id: None,
            kind: None,
            request: request.into(),
        }
    }

    pub fn endpoint_refused(request: Option<RequestId>, reason: impl Into<String>) -> Self {
        Self::Rejected {
            reason: reason.into(),
            id: None,
            kind: Some(RejectKind::Endpoint),
            request,
        }
    }
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct TurnStats {
    pub started_at: String,
    pub first_byte_ms: Option<u64>,
    pub duration_ms: u64,
    pub retries: u32,
    pub previous_prompt_tokens: Option<u64>,
}

pub const NATIVE_ITEM: &str = "wisp_native_item";

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum BlockKind {
    Text,
    Thinking,
    RedactedThinking,
    ToolUse {
        name: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        writing: Option<String>,
    },
    Other {
        name: String,
    },
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "status", rename_all = "snake_case")]
pub enum TurnOutcome {
    Completed {
        stop_reason: StopReason,
        model: String,
        usage: Usage,
        cost: Option<Cost>,
        committed: bool,
        #[serde(default = "one")]
        steps: u32,
        #[serde(default)]
        tool_calls: u32,
    },
    Cancelled {
        usage: Option<Usage>,
        cost: Option<Cost>,
        #[serde(default)]
        committed_steps: u32,
    },
    Error {
        kind: ErrorKind,
        message: String,
        usage: Option<Usage>,
        cost: Option<Cost>,
        #[serde(default)]
        committed_steps: u32,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        detail: Option<ErrorDetail>,
        #[serde(default, skip_serializing_if = "is_zero")]
        attempts: u32,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        gave_up: Option<GaveUp>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        offer: Option<RetryOffer>,
    },
}

impl TurnOutcome {
    pub fn usage(&self) -> Option<&Usage> {
        match self {
            TurnOutcome::Completed { usage, .. } => Some(usage),
            TurnOutcome::Cancelled { usage, .. } | TurnOutcome::Error { usage, .. } => {
                usage.as_ref()
            }
        }
    }

    pub fn cost(&self) -> Option<Cost> {
        match self {
            TurnOutcome::Completed { cost, .. }
            | TurnOutcome::Cancelled { cost, .. }
            | TurnOutcome::Error { cost, .. } => *cost,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum GaveUp {
    NotRetryable,
    StepLimit,
    TurnLimit,
    Budget,
    RetryAfterTooLong,
    BusyWindow,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct BusyWait {
    pub waited_ms: u64,
    pub window_ms: u64,
}

fn is_false(b: &bool) -> bool {
    !*b
}

fn is_zero(n: &u32) -> bool {
    *n == 0
}

fn is_zero_u64(n: &u64) -> bool {
    *n == 0
}

fn unknown_kind() -> ErrorKind {
    ErrorKind::Other
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct StreamDiagnostics {
    pub bytes: u64,
    pub events: u64,
    pub text_parts: u64,
    pub thinking_parts: u64,
    pub tool_use_parts: u64,
    pub last_event_keys: Vec<String>,
    pub partial_event: bool,
    pub stop_reason_seen: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub first_byte_ms: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_byte_ms: Option<u64>,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CallForm {
    #[default]
    Absent,
    Call,
    Python,
    EmptyCall,
    #[serde(other)]
    Other,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct MalformedCall {
    pub form: CallForm,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub function: Option<String>,
    #[serde(skip_serializing_if = "is_false")]
    pub undeclared: bool,
    pub chars: u64,
    #[serde(skip_serializing_if = "is_false")]
    pub balanced: bool,
    #[serde(skip_serializing_if = "is_false")]
    pub stop_string: bool,
    #[serde(skip_serializing_if = "is_false")]
    pub mail_tag: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ErrorDetail {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub limit_reached: Option<Box<plan_usage::UsageWindow>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model_limit: Option<Box<plan_usage::UsageWindow>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub stream: Option<Box<StreamDiagnostics>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub malformed_call: Option<Box<MalformedCall>>,
    pub phase: FailurePhase,
    pub source: FailureSource,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub status: Option<u16>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error_type: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub request_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
    pub retry: RetryClass,
    pub possibly_billed: bool,
}

impl ErrorDetail {
    pub fn new(phase: FailurePhase, source: FailureSource, retry: RetryClass) -> Self {
        Self {
            phase,
            source,
            limit_reached: None,
            model_limit: None,
            stream: None,
            malformed_call: None,
            status: None,
            error_type: None,
            request_id: None,
            model: None,
            retry,
            possibly_billed: false,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FailurePhase {
    Connect,
    Response,
    Stream,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FailureSource {
    Local,
    Endpoint,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum RetryClass {
    Never,
    Transient,
    After { ms: u64 },
}

fn one() -> u32 {
    1
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum StopReason {
    EndTurn,
    MaxTokens,
    StopSequence,
    ToolUse,
    PauseTurn,
    Refusal,
    Compaction,
    #[serde(rename = "model_context_window_exceeded")]
    ContextWindowExceeded,
    #[serde(untagged)]
    Other(String),
}

impl StopReason {
    pub fn as_str(&self) -> &str {
        match self {
            Self::EndTurn => "end_turn",
            Self::MaxTokens => "max_tokens",
            Self::StopSequence => "stop_sequence",
            Self::ToolUse => "tool_use",
            Self::PauseTurn => "pause_turn",
            Self::Refusal => "refusal",
            Self::Compaction => "compaction",
            Self::ContextWindowExceeded => "model_context_window_exceeded",
            Self::Other(s) => s,
        }
    }

    pub fn parse(s: &str) -> Self {
        match s {
            "end_turn" => Self::EndTurn,
            "max_tokens" => Self::MaxTokens,
            "stop_sequence" => Self::StopSequence,
            "tool_use" => Self::ToolUse,
            "pause_turn" => Self::PauseTurn,
            "refusal" => Self::Refusal,
            "compaction" => Self::Compaction,
            "model_context_window_exceeded" => Self::ContextWindowExceeded,
            other => Self::Other(other.to_owned()),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ErrorKind {
    Auth,
    RateLimit,
    Overloaded,
    InvalidRequest,
    Network,
    Refusal,
    InvalidResponse,
    ThinkingBinding,
    Server,
    Timeout,
    Billing,
    Interrupted,
    ContextOverflow,
    Other,
}

impl ErrorKind {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Auth => "authentication",
            Self::RateLimit => "rate limited",
            Self::Overloaded => "overloaded",
            Self::InvalidRequest => "invalid request",
            Self::Network => "network",
            Self::Refusal => "refusal",
            Self::InvalidResponse => "invalid response",
            Self::ThinkingBinding => "thinking binding",
            Self::Server => "server error",
            Self::Timeout => "timeout",
            Self::Billing => "billing",
            Self::Interrupted => "interrupted",
            Self::ContextOverflow => "context overflow",
            Self::Other => "error",
        }
    }

    pub fn sentence(self, provider: &str) -> String {
        match self {
            Self::Auth => format!("{provider} refused the credentials."),
            Self::RateLimit => format!("{provider} is rate limiting these requests."),
            Self::Overloaded => format!("{provider} is overloaded."),
            Self::InvalidRequest => format!("{provider} rejected the request."),
            Self::Network => format!("The connection to {provider} failed."),
            Self::Refusal => format!("{provider} declined to answer."),
            Self::InvalidResponse => format!("{provider} sent a reply Wisp couldn't use."),
            Self::ThinkingBinding => format!("{provider} stopped this turn."),
            Self::Server => format!("{provider} had a server error."),
            Self::Timeout => format!("{provider} stopped responding."),
            Self::Billing => format!("{provider} refused the request for billing reasons."),
            Self::Interrupted => "Wisp stopped during this turn.".to_owned(),
            Self::ContextOverflow => {
                format!("{provider} said the conversation is too long for the model.")
            }
            Self::Other => format!("{provider} reported an error."),
        }
    }
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Billing {
    #[default]
    Metered,
    ChatGptPlan,
    AnthropicPlan,
    AntigravityPlan,
    Mixed,
}

pub const MIXED_BILLING_NOTE: &str = "mixed billing · split unavailable";

impl Billing {
    pub fn combined(self, other: Self) -> Self {
        if self == other { self } else { Self::Mixed }
    }

    pub fn is_metered(&self) -> bool {
        *self == Self::Metered
    }
}

pub fn starts_charging(from: Option<Billing>, to: Billing) -> bool {
    to.is_metered() && from.is_none_or(|billing| !billing.is_metered())
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Usage {
    pub input_tokens: u64,
    pub output_tokens: u64,
    pub cache_read_input_tokens: u64,
    pub cache_creation_input_tokens: u64,
    pub cache_creation_5m_input_tokens: Option<u64>,
    pub cache_creation_1h_input_tokens: Option<u64>,
    #[serde(default, skip_serializing_if = "is_zero_u64")]
    pub web_search_requests: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cache_creation_30m_input_tokens: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cache_creation_12h_input_tokens: Option<u64>,
    #[serde(default, skip_serializing_if = "is_zero_u64")]
    pub reasoning_tokens: u64,
    #[serde(default, skip_serializing_if = "Billing::is_metered")]
    pub billing: Billing,
}

impl Usage {
    pub fn prompt_tokens(&self) -> u64 {
        self.input_tokens
            .saturating_add(self.cache_creation_input_tokens)
            .saturating_add(self.cache_read_input_tokens)
    }

    pub fn cache_percent(&self) -> Option<u8> {
        context::cache_percent(self.cache_read_input_tokens, self.prompt_tokens())
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct AccountSpend {
    pub usd: f64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub plan_usd: Option<f64>,
    #[serde(default, skip_serializing_if = "is_false")]
    pub lower_bound: bool,
}

impl Default for AccountSpend {
    fn default() -> Self {
        Self {
            usd: 0.0,
            plan_usd: Some(0.0),
            lower_bound: false,
        }
    }
}

impl AccountSpend {
    pub fn add(&mut self, other: Self) {
        self.lower_bound |= other.lower_bound;
        if other.usd == 0.0 {
            return;
        }
        self.usd += other.usd;
        self.plan_usd = self.plan_usd.zip(other.plan_usd).map(|(a, b)| a + b);
    }

    pub fn of(usd: f64, plan_usd: Option<f64>, billing: Billing, lower_bound: bool) -> Self {
        let usd = if usd.is_finite() { usd.max(0.0) } else { 0.0 };
        let plan_usd = match billing {
            Billing::Metered => Some(0.0),
            Billing::ChatGptPlan | Billing::AnthropicPlan | Billing::AntigravityPlan => Some(usd),
            Billing::Mixed => plan_usd
                .filter(|plan| plan.is_finite())
                .map(|plan| plan.clamp(0.0, usd)),
        };
        Self {
            usd,
            plan_usd,
            lower_bound,
        }
    }

    pub fn api_usd(&self) -> Option<f64> {
        self.plan_usd.map(|plan| (self.usd - plan).max(0.0))
    }

    pub fn charged(usage: Option<&Usage>, cost_usd: Option<f64>, lower_bound: bool) -> Self {
        let usd = cost_usd.unwrap_or(0.0);
        let billing = usage.map_or(Billing::Metered, |u| u.billing);
        let plan_usd = match billing {
            Billing::Metered => Some(0.0),
            Billing::Mixed if usd > 0.0 => None,
            Billing::ChatGptPlan
            | Billing::AnthropicPlan
            | Billing::AntigravityPlan
            | Billing::Mixed => Some(usd),
        };
        Self {
            usd,
            plan_usd,
            lower_bound,
        }
    }
}

pub const SPEND_WINDOW_DAYS: i64 = 30;

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct SpendDay {
    pub day: String,
    pub accounts: BTreeMap<String, AccountSpend>,
    pub total: AccountSpend,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct SpendWindow {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub since: Option<String>,
    pub accounts: BTreeMap<String, AccountSpend>,
    pub total: AccountSpend,
}

#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct Cost {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub turn_plan_usd: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub session_plan_usd: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub agents_plan_usd: Option<f64>,
    #[serde(default, skip_serializing_if = "Billing::is_metered")]
    pub agents_billing: Billing,
    #[serde(default, skip_serializing_if = "Billing::is_metered")]
    pub turn_billing: Billing,
    #[serde(default, skip_serializing_if = "Billing::is_metered")]
    pub session_billing: Billing,
    pub turn_usd: f64,
    pub session_usd: f64,
    #[serde(default)]
    pub turn_lower_bound: bool,
    #[serde(default)]
    pub session_lower_bound: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub agents_usd: Option<f64>,
    #[serde(default, skip_serializing_if = "is_false")]
    pub agents_lower_bound: bool,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn effort_levels_order_from_low_to_max() {
        assert!(Effort::Low < Effort::Medium);
        assert!(Effort::Medium < Effort::High);
        assert!(Effort::High < Effort::Xhigh);
        assert!(Effort::Xhigh < Effort::Max);
        let mut sorted = Effort::ALL;
        sorted.sort();
        assert_eq!(sorted, Effort::ALL);
        assert!(Effort::ALL.windows(2).all(|w| w[0] < w[1]));
    }

    #[test]
    fn thinking_block_round_trips() {
        let block = ContentBlock::Thinking {
            thinking: "hmm".into(),
            signature: "sig==".into(),
        };
        let json = serde_json::to_string(&block).unwrap();
        assert_eq!(
            json,
            r#"{"type":"thinking","thinking":"hmm","signature":"sig=="}"#
        );
        assert_eq!(serde_json::from_str::<ContentBlock>(&json).unwrap(), block);
    }

    #[test]
    fn stop_reason_other_round_trips() {
        let r = StopReason::parse("something_new");
        let json = serde_json::to_string(&r).unwrap();
        assert_eq!(json, r#""something_new""#);
        assert_eq!(serde_json::from_str::<StopReason>(&json).unwrap(), r);
        assert_eq!(
            serde_json::to_string(&StopReason::EndTurn).unwrap(),
            r#""end_turn""#
        );
    }

    #[test]
    fn hit_rate() {
        let u = Usage {
            input_tokens: 10,
            cache_read_input_tokens: 90,
            ..Usage::default()
        };
        assert_eq!(u.cache_percent(), Some(90));
        assert_eq!(Usage::default().cache_percent(), None);
    }

    #[test]
    fn parses_flags() {
        assert_eq!("xhigh".parse::<Effort>().unwrap(), Effort::Xhigh);
        assert_eq!("1h".parse::<CacheTtl>().unwrap(), CacheTtl::OneHour);
        assert_eq!("utc".parse::<MessageTime>().unwrap(), MessageTime::Utc);
        for e in Effort::ALL {
            assert_eq!(e.as_str().parse(), Ok(e));
            assert_eq!(serde_json::to_value(e).unwrap(), Value::from(e.as_str()));
        }
        for t in CacheTtl::ALL {
            assert_eq!(t.as_str().parse(), Ok(t));
        }
        for m in MessageTime::ALL {
            assert_eq!(m.as_str().parse(), Ok(m));
            assert_eq!(serde_json::to_value(m).unwrap(), Value::from(m.as_str()));
        }
        let error = |e: ParseError| e.0;
        assert_eq!(
            "ultra".parse::<Effort>().map_err(error),
            Err("unknown effort `ultra` (expected low, medium, high, xhigh or max)".to_owned())
        );
        assert_eq!(
            "2h".parse::<CacheTtl>().map_err(error),
            Err("unknown cache TTL `2h` (expected 5m or 1h)".to_owned())
        );
        assert_eq!(
            "zone".parse::<MessageTime>().map_err(error),
            Err("unknown time mode `zone` (expected local, utc or off)".to_owned())
        );
    }

    #[test]
    fn antigravity_plan_tracks_subscription_spend_with_zero_api_charge() {
        let json = serde_json::to_string(&Billing::AntigravityPlan).unwrap();
        assert_eq!(json, r#""antigravity_plan""#);
        assert_eq!(
            serde_json::from_str::<Billing>(&json).unwrap(),
            Billing::AntigravityPlan
        );
        let spend = AccountSpend::of(0.42, None, Billing::AntigravityPlan, false);
        assert_eq!(spend.usd, 0.42);
        assert_eq!(spend.plan_usd, Some(0.42));
        assert_eq!(spend.api_usd(), Some(0.0));
        let usage = Usage {
            billing: Billing::AntigravityPlan,
            ..Usage::default()
        };
        let charged = AccountSpend::charged(Some(&usage), Some(0.42), false);
        assert_eq!(charged.plan_usd, Some(0.42));
        assert_eq!(charged.api_usd(), Some(0.0));
        assert!(starts_charging(
            Some(Billing::AntigravityPlan),
            Billing::Metered
        ));
        assert!(!starts_charging(
            Some(Billing::AntigravityPlan),
            Billing::AntigravityPlan
        ));
    }
}
