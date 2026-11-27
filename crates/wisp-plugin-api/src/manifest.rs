use std::fmt;

use serde::Deserialize;
use serde::de::{self, Deserializer, MapAccess, SeqAccess, Visitor};

pub const WORLDS: [(&str, Option<&str>); 4] = [
    (
        "0.1.0",
        Some("c79748ef806e8be97a856fce2820c80a8fcb620d1c9303aae369813b1556aa19"),
    ),
    (
        "0.2.0",
        Some("c3d9b698c818df922135fc72bd0beae2ae6a4ba1678fa3dbfb8a915ff1e4e20a"),
    ),
    (
        "0.3.0",
        Some("7fbd345b7c1b5d79e95328767e1739b294537e738018a9ba4f94af1bf7dd7b63"),
    ),
    ("0.4.0", None),
];

pub fn spoken(world: &str) -> bool {
    WORLDS.iter().any(|(spoken, _)| *spoken == world)
}

pub fn frozen(world: &str) -> bool {
    WORLDS
        .iter()
        .any(|(spoken, frozen)| *spoken == world && frozen.is_some())
}

pub const SLOT_WORLD: &str = "0.2.0";

pub const ROW_WORLD: &str = "0.3.0";

pub const USAGE_WORLD: &str = "0.4.0";

pub const DYNAMIC_TOOLS_WORLD: &str = "0.4.0";

pub const USAGE_CHANGED: &str = "usage-changed";

pub const BUNDLED_IDS: [&str; 26] = [
    "anthropic",
    "gemini",
    "openai",
    "shell",
    "show-diff",
    "view-file",
    "ask-user",
    "chat",
    "secret",
    "web-search",
    "time",
    "context",
    "todos",
    "agents",
    "self-check",
    "workspace",
    "instructions",
    "compaction",
    "menus",
    "mcp",
    "memory",
    "page",
    "idle",
    "agent-viz",
    "search",
    "skills",
];

pub const SESSION_START: &str = "session-start";
pub const SESSION_END: &str = "session-end";
pub const USER_INPUT: &str = "user-input";
pub const BEFORE_TURN: &str = "before-turn";
pub const AFTER_TURN: &str = "after-turn";
pub const BEFORE_REQUEST: &str = "before-request";
pub const AFTER_RESPONSE: &str = "after-response";
pub const BEFORE_END: &str = "before-end";
pub const BEFORE_TOOL: &str = "before-tool";
pub const AFTER_TOOL: &str = "after-tool";
pub const CONTEXT_THRESHOLD: &str = "context-threshold";
pub const BEFORE_COMPACT: &str = "before-compact";
pub const COMPACTION_HEAD: &str = "compaction-head";
pub const AFTER_COMPACT: &str = "after-compact";
pub const REWOUND: &str = "rewound";
pub const COMMAND: &str = "command";
pub const TOOLS_CHANGED: &str = "tools-changed";
pub const SETTINGS_CHANGED: &str = "settings-changed";
pub const MODEL_SWITCHED: &str = "model-switched";
pub const AGENT_SPAWNED: &str = "agent-spawned";
pub const AGENT_STATE: &str = "agent-state";
pub const AGENT_MAIL: &str = "agent-mail";
pub const AGENT_REPORTED: &str = "agent-reported";
pub const AGENT_STOPPED: &str = "agent-stopped";
pub const AGENT_ENDED: &str = "agent-ended";

pub const CORE_STATE: &str = "core/state";
pub const CORE_NOTES: &str = "core/notes";
pub const CORE_NEWS: &str = "core/news";
pub const CORE_ENVIRONMENT: &str = "core/environment";
pub const CORE_PROCESSES: &str = "core/processes";

pub const CORE_ID: &str = "core";

pub const RESERVED_IDS: [&str; 2] = [CORE_ID, "wisp"];

pub const CORE_SETS: [&str; 3] = [CORE_STATE, CORE_NOTES, CORE_NEWS];

pub const CONTINUABLE: [&str; 2] = [CORE_ENVIRONMENT, CORE_PROCESSES];

pub const SET_POINT: &str = BEFORE_TURN;

pub const TYPED_NODE: &str = "core/typed";

pub const CORE_POINTS: [&str; 19] = [
    SESSION_START,
    SESSION_END,
    USER_INPUT,
    BEFORE_TURN,
    AFTER_TURN,
    BEFORE_REQUEST,
    AFTER_RESPONSE,
    BEFORE_END,
    BEFORE_TOOL,
    AFTER_TOOL,
    CONTEXT_THRESHOLD,
    BEFORE_COMPACT,
    COMPACTION_HEAD,
    AFTER_COMPACT,
    REWOUND,
    COMMAND,
    TOOLS_CHANGED,
    SETTINGS_CHANGED,
    MODEL_SWITCHED,
];

pub const OBSERVE_POINTS: [&str; 11] = [
    SESSION_START,
    SESSION_END,
    AFTER_TURN,
    BEFORE_REQUEST,
    AFTER_RESPONSE,
    AFTER_COMPACT,
    REWOUND,
    COMMAND,
    TOOLS_CHANGED,
    SETTINGS_CHANGED,
    MODEL_SWITCHED,
];

pub const AGENT_POINTS: [&str; 6] = [
    AGENT_SPAWNED,
    AGENT_STATE,
    AGENT_MAIL,
    AGENT_REPORTED,
    AGENT_STOPPED,
    AGENT_ENDED,
];

pub const RESERVE_POINTS: [&str; 2] = [BEFORE_TURN, COMPACTION_HEAD];

pub const FROM_POINTS: [&str; 1] = [CONTEXT_THRESHOLD];

pub const SUMMARY_CHARS: usize = 60;

pub const COMMAND_SUMMARY_CHARS: usize = 32;

pub const COMMAND_TIMEOUT_MS: u64 = 30_000;

pub const SLOT_TITLE_CHARS: usize = 24;

pub const HELP_CHARS: usize = 160;

pub const REFRESH_S: std::ops::RangeInclusive<u64> = 1..=3600;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Origin {
    Bundled,
    External,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Problem {
    pub line: Option<usize>,
    pub column: Option<usize>,
    pub message: String,
}

impl Problem {
    fn plain(message: String) -> Self {
        Problem {
            line: None,
            column: None,
            message,
        }
    }

    fn at(text: &str, offset: Option<usize>, message: String) -> Self {
        let Some(offset) = offset else {
            return Problem::plain(message);
        };
        let before = text.get(..offset).unwrap_or(text);
        let line = before.matches('\n').count() + 1;
        let start = before.rfind('\n').map_or(0, |i| i + 1);
        let column = before.get(start..).map_or(0, |s| s.chars().count()) + 1;
        Problem {
            line: Some(line),
            column: Some(column),
            message,
        }
    }

    fn from_toml(text: &str, error: &toml::de::Error) -> Self {
        Problem::at(
            text,
            error.span().map(|span| span.start),
            error.message().to_owned(),
        )
    }
}

impl fmt::Display for Problem {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match (self.line, self.column) {
            (Some(line), Some(column)) => {
                write!(f, "line {line}, column {column}: {}", self.message)
            }
            _ => f.write_str(&self.message),
        }
    }
}

impl std::error::Error for Problem {}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct World020 {
    id: String,
    version: String,
    world: String,
    summary: String,
    #[serde(default)]
    settings: Vec<String>,
    #[serde(default)]
    system: Option<System>,
    #[serde(default)]
    tools: Vec<FrozenTool>,
    #[serde(default)]
    commands: Vec<Command>,
    #[serde(default)]
    hooks: Vec<Hook>,
    #[serde(default)]
    points: Vec<PointDecl>,
    #[serde(default)]
    capabilities: Capabilities,
    #[serde(default)]
    slots: Vec<Slot>,
    #[serde(default)]
    prompt_sections: Vec<PromptSection>,
    #[serde(default)]
    strategy: Option<Strategy>,
}

impl From<World020> for Manifest {
    fn from(old: World020) -> Self {
        Self {
            id: old.id,
            version: old.version,
            world: old.world,
            summary: old.summary,
            settings: old.settings.into_iter().map(Setting::Name).collect(),
            system: old.system,
            tools: old.tools.into_iter().map(Tool::from).collect(),
            commands: old.commands,
            hooks: old.hooks,
            points: old.points,
            capabilities: old.capabilities,
            slots: old.slots,
            prompt_sections: old.prompt_sections,
            strategy: old.strategy,
            index: false,
            live_documents: false,
            dynamic_tools: None,
        }
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct World030 {
    id: String,
    version: String,
    world: String,
    summary: String,
    #[serde(default)]
    settings: Vec<Setting>,
    #[serde(default)]
    system: Option<System>,
    #[serde(default)]
    tools: Vec<Tool030>,
    #[serde(default)]
    commands: Vec<Command>,
    #[serde(default)]
    hooks: Vec<Hook>,
    #[serde(default)]
    points: Vec<PointDecl>,
    #[serde(default)]
    capabilities: Capabilities,
    #[serde(default)]
    slots: Vec<Slot>,
    #[serde(default)]
    prompt_sections: Vec<PromptSection>,
    #[serde(default)]
    strategy: Option<Strategy>,
    #[serde(default)]
    index: bool,
}

impl From<World030> for Manifest {
    fn from(old: World030) -> Self {
        Self {
            id: old.id,
            version: old.version,
            world: old.world,
            summary: old.summary,
            settings: old.settings,
            system: old.system,
            tools: old.tools.into_iter().map(Tool::from).collect(),
            commands: old.commands,
            hooks: old.hooks,
            points: old.points,
            capabilities: old.capabilities,
            slots: old.slots,
            prompt_sections: old.prompt_sections,
            strategy: old.strategy,
            index: old.index,
            live_documents: false,
            dynamic_tools: None,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Manifest {
    pub id: String,
    pub version: String,
    pub world: String,
    pub summary: String,
    #[serde(default)]
    pub settings: Vec<Setting>,
    #[serde(default)]
    pub system: Option<System>,
    #[serde(default)]
    pub tools: Vec<Tool>,
    #[serde(default)]
    pub commands: Vec<Command>,
    #[serde(default)]
    pub hooks: Vec<Hook>,
    #[serde(default)]
    pub points: Vec<PointDecl>,
    #[serde(default)]
    pub capabilities: Capabilities,
    #[serde(default)]
    pub slots: Vec<Slot>,
    #[serde(default)]
    pub prompt_sections: Vec<PromptSection>,
    #[serde(default)]
    pub strategy: Option<Strategy>,
    #[serde(default)]
    pub index: bool,
    #[serde(default)]
    pub live_documents: bool,
    #[serde(default)]
    pub dynamic_tools: Option<String>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct World010 {
    id: String,
    version: String,
    world: String,
    summary: String,
    #[serde(default)]
    settings: Vec<String>,
    #[serde(default)]
    system: Option<System>,
    #[serde(default)]
    tools: Vec<FrozenTool>,
    #[serde(default)]
    commands: Vec<Command>,
    #[serde(default)]
    hooks: Vec<Hook010>,
    #[serde(default)]
    points: Vec<PointDecl>,
    #[serde(default)]
    capabilities: Capabilities,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Hook010 {
    name: String,
    point: String,
    #[serde(default)]
    after: Vec<String>,
    #[serde(default)]
    before: Vec<String>,
    #[serde(default)]
    requires: Vec<String>,
    #[serde(default)]
    timeout_ms: Option<u64>,
    #[serde(default)]
    on_failure: OnFailure,
    #[serde(default)]
    background: bool,
    #[serde(default)]
    switch: Option<String>,
    #[serde(default)]
    reserve: Option<String>,
    #[serde(default)]
    from: Option<Threshold>,
}

impl From<Hook010> for Hook {
    fn from(old: Hook010) -> Self {
        Hook {
            name: old.name,
            point: old.point,
            sets: Vec::new(),
            after: old.after,
            before: old.before,
            requires: old.requires,
            timeout_ms: old.timeout_ms,
            on_failure: old.on_failure,
            background: old.background,
            switch: old.switch,
            reserve: old.reserve,
            from: old.from,
            retry: Retry::Rerun,
            job_news: false,
            continues: None,
        }
    }
}

impl From<World010> for Manifest {
    fn from(old: World010) -> Self {
        Manifest {
            id: old.id,
            version: old.version,
            world: old.world,
            summary: old.summary,
            settings: old.settings.into_iter().map(Setting::Name).collect(),
            system: old.system,
            tools: old.tools.into_iter().map(Tool::from).collect(),
            commands: old.commands,
            hooks: old.hooks.into_iter().map(Hook::from).collect(),
            points: old.points,
            capabilities: old.capabilities,
            slots: Vec::new(),
            prompt_sections: Vec::new(),
            strategy: None,
            index: false,
            live_documents: false,
            dynamic_tools: None,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Setting {
    Name(String),
    Row(SettingRow),
}

impl<'de> Deserialize<'de> for Setting {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        struct Entry;

        impl<'de> Visitor<'de> for Entry {
            type Value = Setting;

            fn expecting(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                f.write_str("a setting name or a row")
            }

            fn visit_str<E: de::Error>(self, value: &str) -> Result<Setting, E> {
                Ok(Setting::Name(value.to_owned()))
            }

            fn visit_map<A: MapAccess<'de>>(self, map: A) -> Result<Setting, A::Error> {
                SettingRow::deserialize(de::value::MapAccessDeserializer::new(map))
                    .map(Setting::Row)
            }
        }

        deserializer.deserialize_any(Entry)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SettingRow {
    pub name: String,
    pub label: String,
    pub help: String,
    pub kind: RowKind,
    pub min: Option<i64>,
    pub max: Option<i64>,
    pub default: RowDefault,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RowKind {
    Bool,
    Int,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(untagged, expecting = "a whole number, or \"on\" or \"off\"")]
pub enum RowDefault {
    Int(i64),
    Bool(Switch),
}

#[derive(Deserialize)]
struct ListedSettings {
    #[serde(default, rename = "settings")]
    _settings: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Strategy {
    pub summary: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct System {
    pub text: String,
    #[serde(default)]
    pub after: Vec<String>,
    #[serde(default)]
    pub before: Vec<String>,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Switch {
    #[default]
    On,
    Off,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Platform {
    Unix,
    Windows,
}

fn all_platforms() -> Vec<Platform> {
    vec![Platform::Unix, Platform::Windows]
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct FrozenTool {
    name: String,
    #[serde(default)]
    default: Switch,
    #[serde(default)]
    provides: Vec<String>,
    #[serde(default)]
    follows: Option<String>,
    #[serde(default = "all_platforms")]
    platforms: Vec<Platform>,
    #[serde(default)]
    native: Option<String>,
}

impl From<FrozenTool> for Tool {
    fn from(old: FrozenTool) -> Self {
        Self {
            name: old.name,
            default: old.default,
            provides: old.provides,
            follows: old.follows,
            platforms: old.platforms,
            native: old.native,
            forwards: false,
            held: false,
        }
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Tool030 {
    name: String,
    #[serde(default)]
    default: Switch,
    #[serde(default)]
    provides: Vec<String>,
    #[serde(default)]
    follows: Option<String>,
    #[serde(default = "all_platforms")]
    platforms: Vec<Platform>,
    #[serde(default)]
    native: Option<String>,
    #[serde(default)]
    forwards: bool,
}

impl From<Tool030> for Tool {
    fn from(old: Tool030) -> Self {
        Self {
            name: old.name,
            default: old.default,
            provides: old.provides,
            follows: old.follows,
            platforms: old.platforms,
            native: old.native,
            forwards: old.forwards,
            held: false,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Tool {
    pub name: String,
    #[serde(default)]
    pub default: Switch,
    #[serde(default)]
    pub provides: Vec<String>,
    #[serde(default)]
    pub follows: Option<String>,
    #[serde(default = "all_platforms")]
    pub platforms: Vec<Platform>,
    #[serde(default)]
    pub native: Option<String>,
    #[serde(default)]
    pub forwards: bool,
    #[serde(default)]
    pub held: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Command {
    pub name: String,
    #[serde(default)]
    pub aliases: Vec<String>,
    pub summary: String,
    #[serde(default)]
    pub args: Vec<CommandArg>,
    #[serde(default)]
    pub busy: Busy,
    #[serde(default = "command_timeout")]
    pub timeout_ms: u64,
}

fn command_timeout() -> u64 {
    COMMAND_TIMEOUT_MS
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CommandArg {
    pub name: String,
    pub values: ArgValues,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ArgValues {
    Free,
    Words(Vec<String>),
}

impl<'de> Deserialize<'de> for ArgValues {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        struct Values;

        impl<'de> Visitor<'de> for Values {
            type Value = ArgValues;

            fn expecting(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                f.write_str("\"free\" or a list of words")
            }

            fn visit_str<E: de::Error>(self, value: &str) -> Result<ArgValues, E> {
                if value == "free" {
                    Ok(ArgValues::Free)
                } else {
                    Err(E::invalid_value(de::Unexpected::Str(value), &self))
                }
            }

            fn visit_seq<A: SeqAccess<'de>>(self, mut seq: A) -> Result<ArgValues, A::Error> {
                let mut words = Vec::new();
                while let Some(word) = seq.next_element::<String>()? {
                    words.push(word);
                }
                Ok(ArgValues::Words(words))
            }
        }

        deserializer.deserialize_any(Values)
    }
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Busy {
    #[default]
    Runs,
    BetweenTurns(String),
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum OnFailure {
    #[default]
    Ignore,
    Block,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Threshold {
    Tokens(u64),
    Percent(u8),
}

impl<'de> Deserialize<'de> for Threshold {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        struct From;

        impl Visitor<'_> for From {
            type Value = Threshold;

            fn expecting(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                f.write_str("a token count, or a share of the window such as \"80%\"")
            }

            fn visit_i64<E: de::Error>(self, value: i64) -> Result<Threshold, E> {
                u64::try_from(value)
                    .map(Threshold::Tokens)
                    .map_err(|_| E::invalid_value(de::Unexpected::Signed(value), &self))
            }

            fn visit_u64<E: de::Error>(self, value: u64) -> Result<Threshold, E> {
                Ok(Threshold::Tokens(value))
            }

            fn visit_str<E: de::Error>(self, value: &str) -> Result<Threshold, E> {
                value
                    .strip_suffix('%')
                    .filter(|n| !n.is_empty() && n.bytes().all(|b| b.is_ascii_digit()))
                    .and_then(|n| n.parse::<u8>().ok())
                    .filter(|n| (1..=100).contains(n))
                    .map(Threshold::Percent)
                    .ok_or_else(|| E::invalid_value(de::Unexpected::Str(value), &self))
            }
        }

        deserializer.deserialize_any(From)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Hook {
    pub name: String,
    pub point: String,
    #[serde(default, rename = "in")]
    pub sets: Vec<String>,
    #[serde(default)]
    pub after: Vec<String>,
    #[serde(default)]
    pub before: Vec<String>,
    #[serde(default)]
    pub requires: Vec<String>,
    #[serde(default)]
    pub timeout_ms: Option<u64>,
    #[serde(default)]
    pub on_failure: OnFailure,
    #[serde(default)]
    pub background: bool,
    #[serde(default)]
    pub switch: Option<String>,
    #[serde(default)]
    pub reserve: Option<String>,
    #[serde(default)]
    pub from: Option<Threshold>,
    #[serde(default)]
    pub retry: Retry,
    #[serde(default)]
    pub job_news: bool,
    #[serde(default)]
    pub continues: Option<String>,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Retry {
    #[default]
    Rerun,
    Replay,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum SectionPlace {
    System,
    Lead,
}

impl<'de> Deserialize<'de> for SectionPlace {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let name = String::deserialize(deserializer)?;
        match name.as_str() {
            "system" => Ok(SectionPlace::System),
            "lead" => Ok(SectionPlace::Lead),
            _ => Err(de::Error::custom(format!(
                "a prompt section's place is `system` or `lead`, not `{name}`"
            ))),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PromptSection {
    pub name: String,
    pub place: SectionPlace,
    #[serde(default, rename = "in")]
    pub sets: Vec<String>,
    #[serde(default)]
    pub after: Vec<String>,
    #[serde(default)]
    pub before: Vec<String>,
    #[serde(default)]
    pub continues: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PointKind {
    Observe,
    Contribute,
    Transform,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PointDecl {
    pub name: String,
    pub kind: PointKind,
    pub version: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Capability {
    Exec,
    Fs,
    Net,
    Ask,
    Model,
    Usage,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Capabilities {
    #[serde(default)]
    pub requested: Vec<Capability>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Place {
    Sidebar,
}

impl<'de> Deserialize<'de> for Place {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let name = String::deserialize(deserializer)?;
        match name.as_str() {
            "sidebar" => Ok(Place::Sidebar),
            _ => Err(de::Error::custom(format!(
                "this Wisp has no slot named `{name}`"
            ))),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Slot {
    pub slot: Place,
    pub name: String,
    pub title: String,
    #[serde(default)]
    pub after: Vec<String>,
    #[serde(default)]
    pub before: Vec<String>,
    #[serde(default)]
    pub default: Switch,
    #[serde(default)]
    pub marks: Vec<String>,
    #[serde(default)]
    pub refresh_s: Option<u64>,
}

#[derive(Deserialize)]
struct SlotsAt {
    #[serde(default)]
    slots: Vec<SlotAt>,
}

#[derive(Deserialize)]
struct SlotAt {
    name: toml::Spanned<String>,
    title: toml::Spanned<String>,
    #[serde(default)]
    after: Vec<toml::Spanned<String>>,
    #[serde(default)]
    before: Vec<toml::Spanned<String>>,
    #[serde(default)]
    marks: Vec<toml::Spanned<String>>,
    #[serde(default)]
    refresh_s: Option<toml::Spanned<u64>>,
}

#[derive(Deserialize)]
struct NodesAt {
    #[serde(default)]
    hooks: Vec<HookAt>,
    #[serde(default)]
    prompt_sections: Vec<SectionAt>,
}

#[derive(Deserialize)]
struct HookAt {
    #[serde(default, rename = "in")]
    sets: Vec<toml::Spanned<String>>,
    #[serde(default)]
    retry: Option<toml::Spanned<Retry>>,
    #[serde(default)]
    job_news: Option<toml::Spanned<bool>>,
    #[serde(default)]
    continues: Option<toml::Spanned<String>>,
}

#[derive(Deserialize)]
struct SectionAt {
    name: toml::Spanned<String>,
    #[serde(default, rename = "in")]
    sets: Vec<toml::Spanned<String>>,
    #[serde(default)]
    continues: Option<toml::Spanned<String>>,
}

type Fault = (usize, String);

fn at(spanned: &toml::Spanned<impl Sized>, message: String) -> Fault {
    (spanned.span().start, message)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum PointClass {
    Observe,
    Other,
    Foreign,
    Unknown,
}

#[derive(Deserialize)]
struct WorldOnly {
    world: toml::Spanned<String>,
}

impl Manifest {
    pub fn parse(text: &str, origin: Origin) -> Result<Manifest, Problem> {
        let world: WorldOnly = toml::from_str(text).map_err(|e| Problem::from_toml(text, &e))?;
        if !spoken(world.world.get_ref()) {
            return Err(Problem::at(
                text,
                Some(world.world.span().start),
                format!(
                    "world `{}` isn't spoken by this Wisp, which speaks {}",
                    world.world.get_ref(),
                    worlds()
                ),
            ));
        }
        if !at_least(world.world.get_ref(), ROW_WORLD) {
            let _: ListedSettings =
                toml::from_str(text).map_err(|e| Problem::from_toml(text, &e))?;
        }
        let manifest: Manifest = if at_least(world.world.get_ref(), DYNAMIC_TOOLS_WORLD) {
            toml::from_str(text).map_err(|e| Problem::from_toml(text, &e))?
        } else if at_least(world.world.get_ref(), ROW_WORLD) {
            toml::from_str::<World030>(text)
                .map_err(|e| Problem::from_toml(text, &e))?
                .into()
        } else if at_least(world.world.get_ref(), SLOT_WORLD) {
            toml::from_str::<World020>(text)
                .map_err(|e| Problem::from_toml(text, &e))?
                .into()
        } else {
            toml::from_str::<World010>(text)
                .map_err(|e| Problem::from_toml(text, &e))?
                .into()
        };
        manifest.validate(origin).map_err(Problem::plain)?;
        if !manifest.slots.is_empty() {
            let at: SlotsAt = toml::from_str(text).map_err(|e| Problem::from_toml(text, &e))?;
            manifest
                .check_slots(&at.slots)
                .map_err(|(offset, message)| Problem::at(text, Some(offset), message))?;
        }
        if at_least(world.world.get_ref(), SLOT_WORLD) {
            let at: NodesAt = toml::from_str(text).map_err(|e| Problem::from_toml(text, &e))?;
            manifest
                .check_nodes(&at, origin)
                .map_err(|(offset, message)| Problem::at(text, Some(offset), message))?;
        }
        Ok(manifest)
    }

    pub fn names(&self) -> impl Iterator<Item = &str> {
        self.settings.iter().filter_map(|setting| match setting {
            Setting::Name(name) => Some(name.as_str()),
            Setting::Row(_) => None,
        })
    }

    pub fn rows(&self) -> impl Iterator<Item = &SettingRow> {
        self.settings.iter().filter_map(|setting| match setting {
            Setting::Row(row) => Some(row),
            Setting::Name(_) => None,
        })
    }

    pub fn notes(&self) -> Vec<String> {
        let mut notes = Vec::new();
        for slot in &self.slots {
            for mark in &slot.marks {
                match self.classify(mark) {
                    Ok(PointClass::Unknown) => notes.push(format!(
                        "section `{}/{}` is marked at `{mark}`, a point this Wisp doesn't fire; that mark is ignored",
                        self.id, slot.name
                    )),
                    Ok(PointClass::Foreign) => notes.push(format!(
                        "section `{}/{}` is marked at `{mark}`, another plugin's point; only Wisp's points and the plugin's own mark a section, so that mark is ignored",
                        self.id, slot.name
                    )),
                    _ => {}
                }
            }
        }
        notes
    }

    fn check_slots(&self, at: &[SlotAt]) -> Result<(), (usize, String)> {
        let mut seen: Vec<&str> = Vec::new();
        for (slot, at) in self.slots.iter().zip(at) {
            let name = slot.name.as_str();
            let here =
                |spanned: &toml::Spanned<String>, message: String| (spanned.span().start, message);
            if !is_id(name) {
                return Err(here(
                    &at.name,
                    format!("section name `{name}` must be {ID_RULE}"),
                ));
            }
            if seen.contains(&name) {
                return Err(here(
                    &at.name,
                    format!("section `{name}` is declared twice"),
                ));
            }
            seen.push(name);
            check_line(
                &format!("section `{name}`'s title"),
                &slot.title,
                SLOT_TITLE_CHARS,
            )
            .map_err(|m| here(&at.title, m))?;
            if let (Some(seconds), Some(spanned)) = (slot.refresh_s, &at.refresh_s)
                && !REFRESH_S.contains(&seconds)
            {
                return Err((
                    spanned.span().start,
                    format!(
                        "section `{name}` refreshes every {seconds} s; refresh_s must be {} to {}",
                        REFRESH_S.start(),
                        REFRESH_S.end()
                    ),
                ));
            }
            for (word, entries) in [("after", &at.after), ("before", &at.before)] {
                for entry in entries {
                    if !is_section(entry.get_ref()) {
                        return Err(here(
                            entry,
                            format!(
                                "section `{name}` is ordered {word} `{}`, which isn't a section name; one is written <plugin>/<section>",
                                entry.get_ref()
                            ),
                        ));
                    }
                }
            }
            for mark in &at.marks {
                let point = mark.get_ref();
                if self.points.iter().any(|p| &p.name == point) {
                    return Err(here(
                        mark,
                        format!(
                            "section `{name}` is marked at `{point}`, this plugin's own point; write it in full: `{}/{point}`",
                            self.id
                        ),
                    ));
                }
                if self.classify(point).map_err(|m| here(mark, m))? == PointClass::Other {
                    return Err(here(
                        mark,
                        format!(
                            "section `{name}` is marked at `{point}`, which isn't an observe point"
                        ),
                    ));
                }
            }
        }
        Ok(())
    }

    fn check_nodes(&self, nodes: &NodesAt, origin: Origin) -> Result<(), Fault> {
        let mut continued: Vec<(usize, &str)> = Vec::new();
        for (hook, spans) in self.hooks.iter().zip(&nodes.hooks) {
            let what = format!("hook `{}`", hook.name);
            let point = hook.point.as_str();
            if point != SET_POINT {
                let keys = [
                    ("in", spans.sets.first().map(|s| s.span().start)),
                    ("retry", spans.retry.as_ref().map(|s| s.span().start)),
                    ("job_news", spans.job_news.as_ref().map(|s| s.span().start)),
                    (
                        "continues",
                        spans.continues.as_ref().map(|s| s.span().start),
                    ),
                ];
                if let Some((key, Some(offset))) = keys.into_iter().find(|(_, o)| o.is_some()) {
                    return Err((
                        offset,
                        format!("{what} declares {key} at `{point}`; only {SET_POINT} takes it"),
                    ));
                }
            }
            check_sets(&what, &spans.sets)?;
            check_continues(&what, spans.continues.as_ref(), origin, &mut continued)?;
            if let Some(news) = &spans.job_news
                && *news.get_ref()
                && origin != Origin::Bundled
            {
                return Err(at(
                    news,
                    format!("{what} declares job_news, which only a bundled plugin may do"),
                ));
            }
        }
        let mut seen: Vec<&str> = Vec::new();
        for (section, spans) in self.prompt_sections.iter().zip(&nodes.prompt_sections) {
            let name = section.name.as_str();
            if !is_name(name) {
                return Err(at(
                    &spans.name,
                    format!("prompt section name `{name}` must be {NAME_RULE}"),
                ));
            }
            if seen.contains(&name) {
                return Err(at(
                    &spans.name,
                    format!("prompt section `{name}` is declared twice"),
                ));
            }
            seen.push(name);
            if self.hooks.iter().any(|hook| hook.name == name) {
                return Err(at(
                    &spans.name,
                    format!(
                        "prompt section `{name}` has the name of a hook of this plugin; sections and hooks share one set of names"
                    ),
                ));
            }
            let what = format!("prompt section `{name}`");
            check_sets(&what, &spans.sets)?;
            check_continues(&what, spans.continues.as_ref(), origin, &mut continued)?;
        }
        continued.sort_unstable();
        for (i, (offset, name)) in continued.iter().enumerate() {
            if continued[..i].iter().any(|(_, earlier)| earlier == name) {
                return Err((*offset, format!("`{name}` is continued twice")));
            }
        }
        Ok(())
    }

    fn validate(&self, origin: Origin) -> Result<(), String> {
        check_id(&self.id, origin)?;
        if self.live_documents && !self.index {
            return Err("live_documents requires index = true".into());
        }
        if self
            .dynamic_tools
            .as_ref()
            .is_some_and(|prefix| !is_tool_name(prefix))
        {
            return Err("dynamic_tools must be a nonempty tool-name prefix".into());
        }
        check_length("summary", &self.summary, SUMMARY_CHARS)?;
        if let Some(system) = &self.system {
            check_sentence(&system.text)?;
        }
        if let Some(strategy) = &self.strategy {
            check_line("the strategy's summary", &strategy.summary, SUMMARY_CHARS)?;
        }
        for (capability, name) in [(Capability::Model, "model"), (Capability::Usage, "usage")] {
            if self.capabilities.requested.contains(&capability)
                && !at_least(&self.world, USAGE_WORLD)
            {
                return Err(format!(
                    "the {name} capability needs world {USAGE_WORLD} or later"
                ));
            }
        }
        self.check_settings()?;
        self.check_tools()?;
        self.check_commands()?;
        self.check_points()?;
        self.check_hooks()
    }

    fn check_settings(&self) -> Result<(), String> {
        let mut seen: Vec<&str> = Vec::new();
        for row in self.rows() {
            let name = row.name.as_str();
            if seen.contains(&name) {
                return Err(format!("setting `{name}` is declared twice"));
            }
            seen.push(name);
            check_line(
                &format!("setting `{name}`'s label"),
                &row.label,
                SLOT_TITLE_CHARS,
            )?;
            check_line(&format!("setting `{name}`'s help"), &row.help, HELP_CHARS)?;
            match row.kind {
                RowKind::Int => {
                    let (Some(min), Some(max)) = (row.min, row.max) else {
                        return Err(format!("setting `{name}` needs min and max"));
                    };
                    if min > max {
                        return Err(format!("setting `{name}` has min above max"));
                    }
                    match row.default {
                        RowDefault::Int(value) if (min..=max).contains(&value) => {}
                        _ => {
                            return Err(format!(
                                "setting `{name}` needs a whole-number default from {min} to {max}"
                            ));
                        }
                    }
                }
                RowKind::Bool => {
                    if row.min.is_some() || row.max.is_some() {
                        return Err(format!("setting `{name}` is bool and takes no min or max"));
                    }
                    if !matches!(row.default, RowDefault::Bool(_)) {
                        return Err(format!("setting `{name}` needs an on or off default"));
                    }
                }
            }
        }
        Ok(())
    }

    fn check_tools(&self) -> Result<(), String> {
        let mut seen: Vec<&str> = Vec::new();
        for tool in &self.tools {
            if tool.forwards && tool.native.is_some() {
                return Err(format!(
                    "tool `{}` is native and cannot forward a call",
                    tool.name
                ));
            }
            if !is_tool_name(&tool.name) {
                return Err(format!(
                    "tool name `{}` must be 1 to 64 characters of A-Z, a-z, 0-9, _ and -",
                    tool.name
                ));
            }
            if seen.contains(&tool.name.as_str()) {
                return Err(format!("tool `{}` is declared twice", tool.name));
            }
            if let Some(family) = &tool.native
                && !is_tool_name(family)
            {
                return Err(format!(
                    "tool `{}` asks for the native tool `{family}`, which must be 1 to 64 characters of A-Z, a-z, 0-9, _ and -",
                    tool.name
                ));
            }
            seen.push(&tool.name);
        }
        for tool in &self.tools {
            let Some(leader) = &tool.follows else {
                continue;
            };
            let Some(target) = self.tools.iter().find(|t| &t.name == leader) else {
                return Err(format!(
                    "tool `{}` follows `{leader}`, which isn't a tool of this plugin",
                    tool.name
                ));
            };
            if let Some(next) = &target.follows {
                return Err(format!(
                    "tool `{}` follows `{leader}`, which itself follows `{next}`; follows can't chain",
                    tool.name
                ));
            }
        }
        Ok(())
    }

    fn check_commands(&self) -> Result<(), String> {
        let mut seen: Vec<&str> = Vec::new();
        for command in &self.commands {
            for name in std::iter::once(&command.name).chain(&command.aliases) {
                if !is_name(name) {
                    return Err(format!("command name `{name}` must be {NAME_RULE}"));
                }
                if seen.contains(&name.as_str()) {
                    return Err(format!("command name `{name}` is declared twice"));
                }
                seen.push(name);
            }
            check_length(
                &format!("command `{}`'s summary", command.name),
                &command.summary,
                COMMAND_SUMMARY_CHARS,
            )?;
        }
        Ok(())
    }

    fn check_points(&self) -> Result<(), String> {
        let mut seen: Vec<&str> = Vec::new();
        for point in &self.points {
            if !is_name(&point.name) {
                return Err(format!("point name `{}` must be {NAME_RULE}", point.name));
            }
            if seen.contains(&point.name.as_str()) {
                return Err(format!("point `{}` is declared twice", point.name));
            }
            seen.push(&point.name);
        }
        Ok(())
    }

    fn check_hooks(&self) -> Result<(), String> {
        let mut seen: Vec<&str> = Vec::new();
        for hook in &self.hooks {
            let name = &hook.name;
            if !is_name(name) {
                return Err(format!("hook name `{name}` must be {NAME_RULE}"));
            }
            if seen.contains(&name.as_str()) {
                return Err(format!("hook `{name}` is declared twice"));
            }
            seen.push(name);
            let point = hook.point.as_str();
            let observe = self.point_is_observe(point)?;
            if let Some(switch) = &hook.switch
                && !self.names().any(|name| name == switch)
            {
                return Err(format!(
                    "hook `{name}` is switched by `{switch}`, a setting this plugin doesn't list"
                ));
            }
            if hook.background && observe == Some(false) {
                return Err(format!(
                    "hook `{name}` runs in the background at `{point}`, which isn't an observe point"
                ));
            }
            if hook.reserve.is_some() && !RESERVE_POINTS.contains(&point) {
                return Err(format!(
                    "hook `{name}` declares reserve at `{point}`; only before-turn and compaction-head take it"
                ));
            }
            if hook.from.is_some() && !FROM_POINTS.contains(&point) {
                return Err(format!(
                    "hook `{name}` declares from at `{point}`; only context-threshold takes it"
                ));
            }
            if point == SET_POINT && hook.after.iter().any(|a| a == TYPED_NODE) {
                return Err(format!(
                    "hook `{name}` comes after `{TYPED_NODE}`; the typed text ends the turn message"
                ));
            }
        }
        Ok(())
    }

    fn point_is_observe(&self, point: &str) -> Result<Option<bool>, String> {
        match self.classify(point)? {
            PointClass::Observe => Ok(Some(true)),
            PointClass::Other => Ok(Some(false)),
            PointClass::Foreign => Ok(None),
            PointClass::Unknown => Err(self.not_a_point(point)),
        }
    }

    fn not_a_point(&self, point: &str) -> String {
        format!(
            "`{point}` isn't a point of world {}; a plugin point is written <plugin>/<point>",
            self.world
        )
    }

    fn classify(&self, point: &str) -> Result<PointClass, String> {
        if point == USAGE_CHANGED {
            if !at_least(&self.world, USAGE_WORLD) {
                return Err(format!("`{point}` needs world {USAGE_WORLD} or later"));
            }
            return Ok(PointClass::Observe);
        }
        if AGENT_POINTS.contains(&point) {
            if !at_least(&self.world, SLOT_WORLD) {
                return Err(format!(
                    "`{point}` is an agent point, which needs world {SLOT_WORLD} or later"
                ));
            }
            return Ok(PointClass::Observe);
        }
        if CORE_POINTS.contains(&point) {
            return Ok(if OBSERVE_POINTS.contains(&point) {
                PointClass::Observe
            } else {
                PointClass::Other
            });
        }
        let Some((plugin, name)) = point.split_once('/') else {
            return if is_name(point) {
                Ok(PointClass::Unknown)
            } else {
                Err(self.not_a_point(point))
            };
        };
        if !is_id(plugin) || !is_name(name) {
            return Err(self.not_a_point(point));
        }
        if plugin != self.id {
            return Ok(PointClass::Foreign);
        }
        match self.points.iter().find(|p| p.name == name) {
            Some(own) if own.kind == PointKind::Observe => Ok(PointClass::Observe),
            Some(_) => Ok(PointClass::Other),
            None => Err(format!(
                "`{point}` names this plugin, which declares no point `{name}`"
            )),
        }
    }
}

const NAME_RULE: &str = "1 to 32 characters of a-z, 0-9 and -, starting with a letter";

const ID_RULE: &str = "2 to 32 characters of a-z, 0-9 and -, starting with a letter";

fn check_sets(what: &str, sets: &[toml::Spanned<String>]) -> Result<(), Fault> {
    let mut joined: Option<&str> = None;
    for set in sets {
        let name = set.get_ref().as_str();
        if !CORE_SETS.contains(&name) {
            return Err(at(
                set,
                format!(
                    "{what} joins `{name}`, which isn't a set; the sets are `core/state`, `core/notes` and `core/news`"
                ),
            ));
        }
        match joined {
            Some(first) if first != name => {
                return Err(at(
                    set,
                    format!(
                        "{what} joins `{name}` as well as `{first}`; a node joins one set, since the core's sets run in a chain"
                    ),
                ));
            }
            _ => joined = Some(name),
        }
    }
    Ok(())
}

fn check_continues<'a>(
    what: &str,
    continues: Option<&'a toml::Spanned<String>>,
    origin: Origin,
    continued: &mut Vec<(usize, &'a str)>,
) -> Result<(), Fault> {
    let Some(spanned) = continues else {
        return Ok(());
    };
    let name = spanned.get_ref().as_str();
    if !CONTINUABLE.contains(&name) {
        return Err(at(
            spanned,
            format!(
                "{what} continues `{name}`; only `core/environment` and `core/processes` can be continued"
            ),
        ));
    }
    if origin != Origin::Bundled {
        return Err(at(
            spanned,
            format!("{what} continues `{name}`, which only a bundled plugin may do"),
        ));
    }
    continued.push((spanned.span().start, name));
    Ok(())
}

pub fn at_least(world: &str, since: &str) -> bool {
    let at = |w: &str| WORLDS.iter().position(|(o, _)| *o == w);
    at(since).is_some_and(|since| at(world).is_some_and(|world| since <= world))
}

fn worlds() -> String {
    WORLDS
        .iter()
        .map(|(w, _)| format!("`{w}`"))
        .collect::<Vec<_>>()
        .join(", ")
}

fn kebab(text: &str, min: usize) -> bool {
    let len = text.len();
    (min..=32).contains(&len)
        && text.starts_with(|c: char| c.is_ascii_lowercase())
        && text
            .bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-')
}

pub fn is_id(text: &str) -> bool {
    kebab(text, 2)
}

pub fn is_name(text: &str) -> bool {
    kebab(text, 1)
}

pub fn is_tool_name(text: &str) -> bool {
    (1..=64).contains(&text.len())
        && text
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-')
}

pub fn is_section(text: &str) -> bool {
    text.split_once('/')
        .is_some_and(|(plugin, name)| is_id(plugin) && is_id(name))
}

fn check_id(id: &str, origin: Origin) -> Result<(), String> {
    if !is_id(id) {
        return Err(format!("id `{id}` must be {ID_RULE}"));
    }
    if RESERVED_IDS.contains(&id) {
        return Err(format!("id `{id}` is reserved"));
    }
    if origin == Origin::External && BUNDLED_IDS.contains(&id) {
        return Err(format!("id `{id}` is reserved for a bundled plugin"));
    }
    Ok(())
}

fn check_length(what: &str, text: &str, limit: usize) -> Result<(), String> {
    let length = text.chars().count();
    if length > limit {
        return Err(format!(
            "{what} is {length} characters long; the limit is {limit}"
        ));
    }
    Ok(())
}

fn is_format(c: char) -> bool {
    matches!(
        c,
        '\u{00AD}'
            | '\u{061C}'
            | '\u{180E}'
            | '\u{200B}'..='\u{200F}'
            | '\u{202A}'..='\u{202E}'
            | '\u{2060}'..='\u{206F}'
            | '\u{FEFF}'
    )
}

fn check_line(what: &str, text: &str, limit: usize) -> Result<(), String> {
    if text.chars().all(|c| c.is_whitespace() || is_format(c)) {
        return Err(format!("{what} is blank"));
    }
    if text.chars().any(char::is_control) {
        return Err(format!(
            "{what} holds a control character; it must be one line of text"
        ));
    }
    check_length(what, text, limit)
}

pub fn is_static_sentence(text: &str) -> bool {
    !text
        .chars()
        .any(|c| c.is_ascii_digit() || c == '/' || c == '\\')
}

fn check_sentence(text: &str) -> Result<(), String> {
    if is_static_sentence(text) {
        Ok(())
    } else {
        Err(
            "the system sentence holds a digit, `/` or `\\`, but it may hold no dates, paths or ids"
                .to_owned(),
        )
    }
}
