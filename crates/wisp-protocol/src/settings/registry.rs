use std::collections::BTreeMap;
use std::sync::{Mutex, MutexGuard, PoisonError};

use super::names;
use super::value::{
    ClientVersion, SettingValue, TokensOrPercent, parse_bool, parse_tokens, parse_usd, thousands,
};
use super::wire::CredentialKind;
use crate::agents::config_relative;
use crate::filter::Query;
use crate::names::{
    ACCOUNT_FLAG, MODEL_ID_CHARS, MODEL_IDS_RULE, check_model_id, endpoint_name_rule, shown_model,
    shown_text, valid_endpoint_name,
};
use crate::plugin::{RowKind, SettingRow};
use crate::variable::{VAR_NAME_CHARS, shown_variable};
use crate::{CacheTtl, Effort, InstructionsSource, MessageTime};

pub const DEFAULT_MODEL: &str = "claude-opus-5-5";

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Site {
    Global,
    Endpoint,
    Model,
    EndpointModel,
    Agent,
    Shipped,
    PluginHook,
    PluginSlot,
    Plugin,
}

impl Site {
    pub fn words(self) -> &'static str {
        match self {
            Self::Global => "global",
            Self::Endpoint => "endpoint",
            Self::Model => "model",
            Self::EndpointModel => "endpoint\u{d7}model",
            Self::Agent => "agent (reserved)",
            Self::Shipped => "shipped",
            Self::PluginHook => "plugin hook",
            Self::PluginSlot => "sidebar section",
            Self::Plugin => "plugin",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    Bool,
    Choice(&'static [&'static str]),
    Int { min: i64, max: i64, none: bool },
    Tokens { min: u64, max: u64 },
    TokensOrPercent,
    Usd { none: bool },
    ModelId,
    ModelIds,
    EndpointName,
    AgentName,
    VarName,
    PluginId,
    List(&'static [&'static str]),
    ToolNames,
    Names,
    Preload,
    SectionNames,
    Secret,
    VoiceName,
    Language,
    Path,
    ClientVersion,
}

impl Kind {
    pub const fn choices(self) -> &'static [&'static str] {
        match self {
            Self::Choice(list) | Self::List(list) => list,
            Self::Preload => &[PRELOAD_CORE, PRELOAD_NONE],
            _ => &[],
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Dynamic {
    Endpoints,
    Models,
    Tools,
    Efforts,
    Sections,
    VoiceEndpoints,
    Voices,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Default {
    None,
    Const(&'static str),
    Rule(&'static str),
    Detected(&'static str),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Applies {
    Now,
    NextTurnKept,
    NextTurnRebuild,
    NextTurnMayRebuild,
    NextTurnEffort,
    NextSession,
    NextCall,
    Connection,
    Reserved,
}

macro_rules! cache_kept {
    ($prefix:literal, $suffix:literal) => {
        concat!($prefix, "cache kept", $suffix)
    };
}

pub const CACHE_KEPT: &str = cache_kept!("", "");
pub const CACHE_KEPT_EFFORT: &str = cache_kept!("", " if the gateway passes per-message effort on");
pub const CACHE_KEPT_TOOLS: &str = cache_kept!("", " if passed on");

macro_rules! new_chats {
    ($suffix:literal) => {
        concat!("new chats", $suffix)
    };
}

pub const NEW_CHATS: &str = new_chats!("");

impl Applies {
    pub const fn words(self) -> &'static str {
        match self {
            Self::Now => "now",
            Self::NextTurnKept => cache_kept!("next turn \u{b7} ", ""),
            Self::NextTurnRebuild => "next turn \u{b7} cache rebuild",
            Self::NextTurnMayRebuild => "next turn \u{b7} may rebuild the cache",
            Self::NextTurnEffort => {
                cache_kept!(
                    "next turn \u{b7} ",
                    " with per-message effort, else rebuilt"
                )
            }
            Self::NextSession => NEW_CHATS,
            Self::NextCall => "next call",
            Self::Connection => {
                new_chats!(" for the current account; at the next switch for another")
            }
            Self::Reserved => "reserved",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Owner {
    Client,
    Kernel,
    Host,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AppliesTo {
    All,
    FullScreen,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Secrecy {
    Plain,
    Secret,
    Hidden,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Section {
    Chat,
    Compaction,
    Agents,
    Voice,
    Display,
    Advanced,
    Endpoints,
    Models,
    Plugins,
}

impl Section {
    pub const ALL: [Self; 9] = [
        Self::Chat,
        Self::Compaction,
        Self::Agents,
        Self::Voice,
        Self::Display,
        Self::Advanced,
        Self::Endpoints,
        Self::Models,
        Self::Plugins,
    ];

    pub fn title(self) -> &'static str {
        match self {
            Self::Chat => "Chat",
            Self::Compaction => "Compaction",
            Self::Agents => "Agents",
            Self::Voice => "Voice",
            Self::Display => "Display",
            Self::Advanced => "Advanced",
            Self::Endpoints => "Accounts",
            Self::Models => "Models",
            Self::Plugins => "Plugins",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Status {
    Built,
    Reserved { note: &'static str },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SettingSpec {
    pub name: &'static str,
    pub sites: &'static [Site],
    pub mid_session: bool,
    pub flags: &'static [&'static str],
    pub env: &'static [&'static str],
    pub kind: Kind,
    pub dynamic: Option<Dynamic>,
    pub default: Default,
    pub applies: Applies,
    pub owner: Owner,
    pub applies_to: AppliesTo,
    pub secrecy: Secrecy,
    pub section: Section,
    pub label: &'static str,
    pub help: &'static str,
    pub status: Status,
    pub plugin: Option<&'static str>,
}

const GLOBAL: &[Site] = &[Site::Global];
const ENDPOINT: &[Site] = &[Site::Endpoint];
const GLOBAL_PER_MODEL: &[Site] = &[Site::Global, Site::Model, Site::EndpointModel];
const MODEL_FACT: &[Site] = &[Site::Shipped, Site::Model, Site::EndpointModel];
const SHIPPED_PRICE: &str = "shipped price or none";
const GLOBAL_AGENT: &[Site] = &[Site::Global, Site::Agent];
const PLUGIN_HOOK_SITE: &[Site] = &[Site::PluginHook];
const PLUGIN_SLOT_SITE: &[Site] = &[Site::PluginSlot];
const PLUGIN_SITE: &[Site] = &[Site::Plugin];

pub const PLUGIN_HOOK: &str = "plugins.<id>.hooks.<hook>";
pub const PLUGIN_SLOT: &str = "plugins.<id>.slots.<name>";
pub const PLUGIN_ENABLED: &str = "plugins.<id>.enabled";
pub const PLUGIN_INSTALLED: &str = "plugins.<id>.installed";
pub const ENABLED_KEY: &str = "enabled";
pub const INSTALLED_KEY: &str = "installed";
pub const PLUGINS_GROUP: &str = "plugins";
pub const HOOKS_TABLE: &str = "hooks";
pub const SLOTS_TABLE: &str = "slots";
const EVERY_TABLE: &[Site] = &[
    Site::Global,
    Site::Endpoint,
    Site::Model,
    Site::EndpointModel,
];

const fn spec(
    name: &'static str,
    section: Section,
    label: &'static str,
    help: &'static str,
    kind: Kind,
) -> SettingSpec {
    SettingSpec {
        name,
        sites: GLOBAL,
        mid_session: false,
        flags: &[],
        env: &[],
        kind,
        dynamic: None,
        default: Default::None,
        applies: Applies::NextSession,
        owner: Owner::Kernel,
        applies_to: AppliesTo::All,
        secrecy: Secrecy::Plain,
        section,
        label,
        help,
        status: Status::Built,
        plugin: None,
    }
}

impl SettingSpec {
    pub fn is_declared(&self) -> bool {
        self.section == Section::Plugins && self.plugin.is_some()
    }

    const fn sites(self, sites: &'static [Site]) -> Self {
        Self { sites, ..self }
    }

    const fn mid(self) -> Self {
        Self {
            mid_session: true,
            ..self
        }
    }

    const fn flags(self, flags: &'static [&'static str]) -> Self {
        Self { flags, ..self }
    }

    const fn env(self, env: &'static [&'static str]) -> Self {
        Self { env, ..self }
    }

    const fn dynamic(self, dynamic: Dynamic) -> Self {
        Self {
            dynamic: Some(dynamic),
            ..self
        }
    }

    const fn default(self, default: Default) -> Self {
        Self { default, ..self }
    }

    const fn applies(self, applies: Applies) -> Self {
        Self { applies, ..self }
    }

    const fn owner(self, owner: Owner) -> Self {
        Self { owner, ..self }
    }

    const fn full_screen(self) -> Self {
        Self {
            applies_to: AppliesTo::FullScreen,
            ..self
        }
    }

    const fn secrecy(self, secrecy: Secrecy) -> Self {
        Self { secrecy, ..self }
    }

    const fn reserved(self, note: &'static str) -> Self {
        Self {
            status: Status::Reserved { note },
            applies: Applies::Reserved,
            ..self
        }
    }

    const fn display(self, var: &'static [&'static str], default: Default) -> Self {
        self.mid()
            .env(var)
            .default(default)
            .applies(Applies::Now)
            .owner(Owner::Client)
            .full_screen()
    }

    const fn plugin(self, id: &'static str) -> Self {
        Self {
            plugin: Some(id),
            ..self
        }
    }

    const fn connection(self) -> Self {
        self.sites(ENDPOINT)
            .applies(Applies::Connection)
            .owner(Owner::Host)
    }
}

pub const AGENTS_NOTE: &str = "planned with agent types and can't be set yet";

pub const INSTRUCTION_EXTRAS: &[&str] = &["none", "autonomous"];
pub const PROVIDER_ANTHROPIC: &str = "anthropic";
pub const PROVIDER_OPENAI: &str = "openai";
pub const PROVIDER_GEMINI: &str = "gemini";
pub const AUTH_API_KEY: &str = "api_key";
pub const AUTH_BEARER: &str = "bearer";
pub const AUTH_ANTHROPIC_SUBSCRIPTION: &str = "anthropic_subscription";
pub const AUTH_CHATGPT_SUBSCRIPTION: &str = "chatgpt_subscription";
pub const AUTH_ANTIGRAVITY_SUBSCRIPTION: &str = "antigravity_subscription";
pub const KEY_STORE_FILE: &str = "file";
pub const KEY_STORE_KEYRING: &str = "keyring";
pub const PRICING_LIST: &str = "list";
pub const PRICING_NONE: &str = "none";
pub const PRELOAD_CORE: &str = "core";
pub const PRELOAD_NONE: &str = "none";
const PRELOAD_RULE: &str = "must be core, none, or a list of tool names; none must stand alone";

pub fn preload_names(items: &[String]) -> Result<Vec<String>, ValueError> {
    if items
        .iter()
        .any(|item| !is_tool_name(item) || crate::looks_like_key(item))
        || (items.len() > 1 && items.iter().any(|item| item == PRELOAD_NONE))
    {
        return Err(ValueError {
            rule: PRELOAD_RULE.to_owned(),
        });
    }
    let mut names = if items.is_empty() {
        vec![PRELOAD_NONE.to_owned()]
    } else {
        items.to_vec()
    };
    names.sort();
    names.dedup();
    Ok(names)
}

pub fn resolve_preload(preset: Option<&[String]>, setting: Option<&[String]>) -> Vec<String> {
    preset.or(setting).map_or_else(
        || match find(names::PRELOAD).and_then(SettingSpec::default_value) {
            Some(SettingValue::List(names)) => names,
            _ => Vec::new(),
        },
        <[String]>::to_vec,
    )
}
pub const PROVIDERS: &[&str] = &[PROVIDER_ANTHROPIC, PROVIDER_OPENAI, PROVIDER_GEMINI];
pub fn provider_plugin(id: &str) -> bool {
    PROVIDERS.contains(&id)
}

pub const AUTHS: &[&str] = &[
    AUTH_API_KEY,
    AUTH_BEARER,
    AUTH_ANTHROPIC_SUBSCRIPTION,
    AUTH_CHATGPT_SUBSCRIPTION,
    AUTH_ANTIGRAVITY_SUBSCRIPTION,
];
pub const KEY_STORES: &[&str] = &[KEY_STORE_FILE, KEY_STORE_KEYRING];
const KEYCHAINS: &[&str] = &["auto", "on", "off"];
pub const PRICINGS: &[&str] = &[PRICING_LIST, PRICING_NONE];
pub const CLAUDE_WEB_SEARCH: &str = "web_search_20250305";
pub const OPENAI_WEB_SEARCH: &str = "web_search";
pub const GEMINI_WEB_SEARCH: &str = "google_search";
pub const WEB_SEARCH_VERSIONS: &[&str] = &[CLAUDE_WEB_SEARCH, OPENAI_WEB_SEARCH, GEMINI_WEB_SEARCH];
pub const THEMES: &[&str] = &["auto", "dark", "light"];
pub const COLORS: &[&str] = &["auto", "truecolor", "256", "16", "none"];
pub const MOTIONS: &[&str] = &["auto", "full", "calm", "reduced"];
pub const AMBIGUOUS: &[&str] = &["auto", "wide", "narrow"];
pub const COPY_ON_SELECT: &[&str] = &["auto", "on", "off"];
pub const SIDEBAR_MODES: &[&str] = &["auto", "hidden"];

pub const SCROLL_SPEED_MAX: i64 = 20;

pub const MAX_RUNNING_MAX: i64 = 16;
pub const MAX_IN_FLIGHT_MAX: i64 = 64;
pub const MAX_AGENTS_MAX: i64 = 50;
pub const MAX_DEPTH_MAX: i64 = 5;
const WAKE_WHEN_STUCK_MAX: i64 = 1440;
const WARM_REFRESHES_MAX: i64 = 24;
const MAX_AGE_DAYS_MAX: i64 = 3650;

fn max_in_flight_default(credential: Option<&CredentialKind>) -> i64 {
    match credential {
        Some(CredentialKind::Other(kind)) if kind == AUTH_ANTHROPIC_SUBSCRIPTION => 12,
        _ => 8,
    }
}

pub const AGENTS_GROUP: &str = "agents";
pub const PRESETS_TABLE: &str = "presets";
pub const PRESETS_GROUP: &str = "agents.presets";

pub fn agents_group_keys() -> Vec<&'static str> {
    let mut keys: Vec<&'static str> = SETTINGS
        .iter()
        .filter(|s| s.is_built() && s.group() == Some(AGENTS_GROUP))
        .map(SettingSpec::leaf)
        .chain([PRESETS_TABLE])
        .collect();
    keys.sort_unstable();
    keys
}

pub static SETTINGS: &[SettingSpec] = &[
    spec(
        names::AGENT,
        Section::Advanced,
        "Agent type",
        "The agent type a session starts as.",
        Kind::AgentName,
    )
    .flags(&["--agent"])
    .owner(Owner::Host)
    .reserved(AGENTS_NOTE),
    spec(
        names::AGENTS_MAX_AGENTS,
        Section::Agents,
        "Agents per tree",
        "How many live (not ended) agents one session's tree may hold, the main agent included.",
        Kind::Int {
            min: 1,
            max: MAX_AGENTS_MAX,
            none: true,
        },
    )
    .default(Default::Const("50")),
    spec(
        names::AGENTS_MAX_DEPTH,
        Section::Agents,
        "Agent depth",
        "How deep agents may spawn: the main agent is depth 0, its children 1, theirs 2.",
        Kind::Int {
            min: 1,
            max: MAX_DEPTH_MAX,
            none: true,
        },
    )
    .default(Default::Const("2")),
    spec(
        names::AGENTS_MAX_RUNNING,
        Section::Agents,
        "Running children",
        "How many of one agent's children may run a turn at once, not counting children whose preset sets worker = false; the others wait their turn.",
        Kind::Int {
            min: 1,
            max: MAX_RUNNING_MAX,
            none: true,
        },
    )
    .default(Default::Const("4")),
    spec(
        names::AGENTS_MAX_TREE_RUNNING,
        Section::Agents,
        "Running per tree",
        "How many agents in one session's tree may run a turn at once, not counting the main agent or agents whose preset sets worker = false; the others wait their turn.",
        Kind::Int {
            min: 1,
            max: MAX_AGENTS_MAX,
            none: true,
        },
    )
    .default(Default::Const("none")),
    spec(
        names::AGENTS_WAKE_WHEN_STUCK,
        Section::Agents,
        "Wake when stuck",
        "Minutes before Wisp wakes the main session when no agent has worked and its checklist has open items, or an agent's parent when that agent has waited that long to retry; 0 never wakes.",
        Kind::Int {
            min: 0,
            max: WAKE_WHEN_STUCK_MAX,
            none: false,
        },
    )
    .default(Default::Const("0")),
    spec(
        names::AGENTS_WARM_REFRESHES,
        Section::Agents,
        "Keep-warm refreshes",
        "How many times per idle spell Wisp refreshes the prompt cache of an agent kept warm; 0 turns keep-warm off.",
        Kind::Int {
            min: 0,
            max: WARM_REFRESHES_MAX,
            none: false,
        },
    )
    .default(Default::Const("8")),
    spec(
        names::ALIASES,
        Section::Models,
        "Aliases",
        "Other model IDs that name this model's row. A save made mid-session reaches the window and the point at the next change of either.",
        Kind::ModelIds,
    )
    .sites(&[Site::Shipped, Site::Model])
    .owner(Owner::Host),
    spec(
        names::AUTH,
        Section::Endpoints,
        "Sign-in",
        "How the account takes its key: api_key, bearer or anthropic_subscription for Anthropic, api_key or chatgpt_subscription for OpenAI, api_key, bearer or antigravity_subscription for Gemini.",
        Kind::Choice(AUTHS),
    )
    .connection(),
    spec(
        names::CACHE_TTL,
        Section::Advanced,
        "Cache lifetime",
        "How long the conversation's prompt-cache entry lives: 1h for interactive use, 5m for continuous loops.",
        Kind::Choice(&CacheTtl::WORDS),
    )
    .mid()
    .flags(&["--cache-ttl"])
    .default(Default::Const("1h"))
    .applies(Applies::NextTurnMayRebuild),
    spec(
        names::CLAUDE_USAGE,
        Section::Advanced,
        "Claude usage lookup",
        "Read Claude subscription allowance usage when displayed. On by default; claude_usage = false turns it off.",
        Kind::Bool,
    ).mid().applies(Applies::Now).default(Default::Const("on")),
    spec(
        names::CLIENT_VERSION,
        Section::Endpoints,
        "Client version",
        "The version to report.",
        Kind::ClientVersion,
    )
    .sites(ENDPOINT)
    .default(Default::Rule("latest release"))
    .owner(Owner::Host),
    spec(
        names::COMPACT_AT,
        Section::Compaction,
        "Compact at",
        "When the chat is shortened.",
        Kind::TokensOrPercent,
    )
    .sites(GLOBAL_PER_MODEL)
    .mid()
    .flags(&["--compact-at"])
    .default(Default::Rule("window \u{2212} reserve"))
    .applies(Applies::NextTurnKept),
    spec(
        names::CONTEXT_AUTO_COMPACT,
        Section::Compaction,
        "Compact automatically",
        "Compact the conversation before a request that would reach the compaction point.",
        Kind::Bool,
    )
    .sites(GLOBAL_AGENT)
    .mid()
    .flags(&["--no-auto-compact"])
    .default(Default::Const("on"))
    .applies(Applies::NextTurnKept),
    spec(
        names::CONTEXT_BACKGROUND_COMPACTION,
        Section::Compaction,
        "Background compaction",
        "Prepare a native compaction in the background before the compaction point, and swap it in on the next request.",
        Kind::Bool,
    )
    .sites(EVERY_TABLE)
    .mid()
    .default(Default::Rule("on where the provider compacts"))
    .applies(Applies::Now),
    spec(
        names::CONTEXT_BACKGROUND_TOKENS,
        Section::Compaction,
        "Background start",
        "How far below the compaction point a background compaction starts.",
        Kind::Tokens {
            min: 1,
            max: u64::MAX,
        },
    )
    .sites(EVERY_TABLE)
    .mid()
    .default(Default::Const("40000"))
    .applies(Applies::Now),
    spec(
        names::CONTEXT_COMPACT_EFFORT,
        Section::Compaction,
        "Summary effort",
        "The highest effort a compaction's summary runs at, where the server takes a per-message effort.",
        Kind::Choice(&Effort::WORDS),
    )
    .mid()
    .default(Default::Const("high"))
    .applies(Applies::Now),
    spec(
        names::CONTEXT_CONTEXT_NOTE,
        Section::Chat,
        "Context line",
        "Tell the model how full its context is, with a Context: line on each message.",
        Kind::Bool,
    )
    .sites(GLOBAL_AGENT)
    .mid()
    .flags(&["--context-note"])
    .default(Default::Const("on"))
    .applies(Applies::NextTurnKept)
    .plugin("context"),
    spec(
        names::CONTEXT_COUNT_TOKENS,
        Section::Advanced,
        "Count with the server",
        "Let /context ask the server's token counter for the fixed part of the request, where it has one.",
        Kind::Bool,
    )
    .sites(GLOBAL_AGENT)
    .mid()
    .default(Default::Const("on"))
    .applies(Applies::Now),
    spec(
        names::CONTEXT_STRATEGY,
        Section::Compaction,
        "Compaction strategy",
        "Who lays out a compaction's summary and what it keeps: default for Wisp's own layout, or the id of an installed plugin that offers a strategy.",
        Kind::PluginId,
    )
    .mid()
    .default(Default::Const("default"))
    .applies(Applies::NextTurnKept),
    spec(
        names::CONTEXT_VERBATIM_USER_TOKENS,
        Section::Compaction,
        "Your messages kept",
        "How many tokens of your recent messages a compaction keeps word for word; a model table can set its own.",
        Kind::Tokens {
            min: 0,
            max: u64::MAX,
        },
    )
    .mid()
    .default(Default::Const("8000"))
    .applies(Applies::Now)
    .plugin("compaction"),
    spec(
        names::CONTEXT_WINDOW,
        Section::Models,
        "Context window",
        "Your limit on the model's context window; the documented window is the default.",
        Kind::Tokens {
            min: super::POINT_FLOOR,
            max: u64::MAX,
        },
    )
    .sites(MODEL_FACT)
    .mid()
    .default(Default::Rule("the documented window"))
    .applies(Applies::NextTurnKept),
    spec(
        names::DEBUG_CACHE,
        Section::Advanced,
        "Cache diagnostics",
        "Ask the API to diagnose prompt-cache misses.",
        Kind::Bool,
    )
    .mid()
    .flags(&["--debug-cache"])
    .default(Default::Const("off"))
    .applies(Applies::NextTurnMayRebuild),
    spec(
        names::DISPLAY_AMBIGUOUS,
        Section::Display,
        "Ambiguous width",
        "How wide the terminal draws ambiguous-width characters such as \u{b7}.",
        Kind::Choice(AMBIGUOUS),
    )
    .display(&["WISP_AMBIGUOUS"], Default::Detected("auto")),
    spec(
        names::DISPLAY_COLOR,
        Section::Display,
        "Colour",
        "How many colours to use.",
        Kind::Choice(COLORS),
    )
    .display(&["WISP_COLOR"], Default::Detected("auto")),
    spec(
        names::DISPLAY_COPY_ON_SELECT,
        Section::Display,
        "Copy on select",
        "Copy text to the clipboard as soon as it is selected.",
        Kind::Choice(COPY_ON_SELECT),
    )
    .display(&["WISP_COPY_ON_SELECT"], Default::Detected("auto")),
    spec(
        names::DISPLAY_FULL_REPAINT,
        Section::Display,
        "Full repaint",
        "Redraw the whole screen each frame instead of only the changed cells, and hold every animation still, as reduced motion does.",
        Kind::Bool,
    )
    .display(&["WISP_FULL_REPAINT"], Default::Const("off")),
    spec(
        names::DISPLAY_MOTION,
        Section::Display,
        "Motion",
        "How much the screen animates.",
        Kind::Choice(MOTIONS),
    )
    .display(&["WISP_MOTION"], Default::Const("auto")),
    spec(
        names::DISPLAY_MOUSE,
        Section::Display,
        "Mouse",
        "Use the mouse to scroll, select and see names under the pointer on `/agents`.",
        Kind::Bool,
    )
    .display(&["WISP_MOUSE"], Default::Const("on")),
    spec(
        names::DISPLAY_PICTURES,
        Section::Display,
        "Pictures",
        "Draw the agents and Context boxes as pictures where the terminal can show images.",
        Kind::Bool,
    )
    .display(&[], Default::Const("on")),
    spec(
        names::DISPLAY_SCROLL_SPEED,
        Section::Display,
        "Scroll speed",
        "Lines scrolled per mouse-wheel step.",
        Kind::Int {
            min: 1,
            max: SCROLL_SPEED_MAX,
            none: false,
        },
    )
    .display(&["WISP_SCROLL_SPEED"], Default::Const("1")),
    spec(
        names::DISPLAY_SIDEBAR,
        Section::Display,
        "Sidebar",
        "Auto docks the sidebar beside the transcript on a wide terminal once a section has something to show; hidden never docks it.",
        Kind::Choice(SIDEBAR_MODES),
    )
    .display(&["WISP_SIDEBAR"], Default::Const("auto")),
    spec(
        names::DISPLAY_SIDEBAR_FOLDED,
        Section::Display,
        "Folded sections",
        "Sidebar sections drawn as their title row only. A click on a section's title folds it or opens it again.",
        Kind::SectionNames,
    )
    .display(&[], Default::Const("none"))
    .dynamic(Dynamic::Sections),
    spec(
        names::DISPLAY_SIDEBAR_PINNED,
        Section::Display,
        "Pinned sections",
        "Sidebar sections kept at the top while the rest scroll under them.",
        Kind::SectionNames,
    )
    .display(&[], Default::Const("none"))
    .dynamic(Dynamic::Sections),
    spec(
        names::DISPLAY_THEME,
        Section::Display,
        "Theme",
        "Dark or light colours; auto follows the terminal's background.",
        Kind::Choice(THEMES),
    )
    .display(&["WISP_THEME"], Default::Detected("auto")),
    spec(
        names::DISPLAY_VERBOSE,
        Section::Display,
        "Verbose",
        "Show the full token and cache line after each turn, all of the thinking, and a line for every hook run.",
        Kind::Bool,
    )
    .display(&[], Default::Const("off"))
    .flags(&["--verbose"]),
    spec(
        names::EFFORT,
        Section::Chat,
        "Effort",
        "How hard the model thinks.",
        Kind::Choice(&Effort::WORDS),
    )
    .sites(GLOBAL_AGENT)
    .mid()
    .flags(&["--effort"])
    .dynamic(Dynamic::Efforts)
    .default(Default::Const("medium"))
    .applies(Applies::NextTurnEffort),
    spec(
        names::ENDPOINT,
        Section::Chat,
        "Account",
        "The account this chat uses.",
        Kind::EndpointName,
    )
    .mid()
    .flags(&[ACCOUNT_FLAG, "--endpoint"])
    .env(&["WISP_ACCOUNT", "WISP_ENDPOINT"])
    .dynamic(Dynamic::Endpoints)
    .default(Default::Rule("the start rules"))
    .applies(Applies::NextTurnRebuild),
    spec(
        names::INSTRUCTIONS_EXTRA,
        Section::Chat,
        "Extra instructions",
        "Built-in instructions that go before the instruction files: none or autonomous.",
        Kind::Choice(INSTRUCTION_EXTRAS),
    )
    .flags(&["--instructions-extra"])
    .default(Default::Const("none"))
    .owner(Owner::Host),
    spec(
        names::INSTRUCTIONS_SOURCE,
        Section::Chat,
        "Instructions",
        "Which instruction file goes in the system prompt: auto, default or none.",
        Kind::Choice(&InstructionsSource::WORDS),
    )
    .mid()
    .flags(&["--instructions"])
    .default(Default::Const("auto"))
    .applies(Applies::NextTurnKept)
    .owner(Owner::Host)
    .plugin("instructions"),
    spec(
        names::INSTRUCTIONS_WARN_TOKENS,
        Section::Chat,
        "Instructions warning",
        "Warn when the instruction files come to more than this many tokens.",
        Kind::Tokens {
            min: 0,
            max: u64::MAX,
        },
    )
    .mid()
    .flags(&["--instructions-warn-tokens"])
    .default(Default::Const("8000"))
    .applies(Applies::Now)
    .owner(Owner::Host)
    .plugin("instructions"),
    spec(
        names::KEEP_WARM,
        Section::Agents,
        "Keep main warm",
        "Refresh the main session's prompt cache while it idles, up to agents.warm_refreshes times per idle spell.",
        Kind::Bool,
    )
    .default(Default::Const("off")),
    spec(
        names::KEY,
        Section::Endpoints,
        "Key",
        concat!("The account's key, kept in ", crate::credentials_file!(), "; change it with wisp account edit."),
        Kind::Secret,
    )
    .connection()
    .secrecy(Secrecy::Secret),
    spec(
        names::KEY_ENV,
        Section::Endpoints,
        "Key variable",
        "The variable that holds the account's key, where no keychain is available.",
        Kind::VarName,
    )
    .connection(),
    spec(
        names::KEY_STORE,
        Section::Endpoints,
        "Key kept in",
        concat!("Where the account's key is kept: file (", crate::credentials_file!(), ") or keyring."),
        Kind::Choice(KEY_STORES),
    )
    .connection(),
    spec(
        names::KEYCHAIN,
        Section::Advanced,
        "Keychain",
        concat!("Whether Wisp uses the system keychain: auto (when this machine has one; not under WSL), on or off (variables and ", crate::credentials_file!(), " only)."),
        Kind::Choice(KEYCHAINS),
    )
    .flags(&["--keychain"])
    .env(&["WISP_KEYCHAIN"])
    .default(Default::Const("auto"))
    .owner(Owner::Host),
    spec(
        names::LOG,
        Section::Advanced,
        "Session log",
        "Write a session log.",
        Kind::Bool,
    )
    .flags(&["--no-log"])
    .default(Default::Const("on"))
    .owner(Owner::Host),
    spec(
        names::MAX_IN_FLIGHT,
        Section::Endpoints,
        "Requests in flight",
        "How many requests the whole agent tree may have open on this account at once; the rest wait their turn, the main agent first.",
        Kind::Int {
            min: 1,
            max: MAX_IN_FLIGHT_MAX,
            none: true,
        },
    )
    .sites(ENDPOINT)
    .default(Default::Rule("by account kind"))
    .owner(Owner::Host),
    spec(
        names::MAX_OUTPUT,
        Section::Models,
        "Max output",
        "The longest reply this model may write, in tokens; it can only lower what Wisp asks for.",
        Kind::Tokens {
            min: 1,
            max: u32::MAX as u64,
        },
    )
    .sites(MODEL_FACT)
    .default(Default::Rule("documented maximum"))
    .owner(Owner::Host),
    spec(
        names::MESSAGE_THREADS,
        Section::Advanced,
        "Message threads",
        "Send Claude only each request's new messages, on the API and a subscription. Anthropic then holds the conversation, system prompt and tools in memory for up to an hour; not eligible for ZDR or HIPAA.",
        Kind::Bool,
    )
    .default(Default::Const("on")),
    spec(
        names::MODEL,
        Section::Chat,
        "Model",
        "The model to chat with.",
        Kind::ModelId,
    )
    .sites(GLOBAL_AGENT)
    .mid()
    .flags(&["--model"])
    .dynamic(Dynamic::Models)
    .default(Default::Const(DEFAULT_MODEL))
    .applies(Applies::NextTurnRebuild),
    spec(
        names::NATIVE_COMPACTION,
        Section::Models,
        "Provider compaction",
        "Whether the provider can compact this model's conversations itself.",
        Kind::Bool,
    )
    .sites(MODEL_FACT)
    .default(Default::Rule("the shipped fact, else off"))
    .owner(Owner::Host),
    spec(
        names::NATIVE_WEB_SEARCH,
        Section::Endpoints,
        "Native web search",
        "Send the provider's own web search tool through this account.",
        Kind::Bool,
    )
    .sites(ENDPOINT)
    .default(Default::Const("on"))
    .owner(Owner::Host),
    spec(
        PLUGIN_ENABLED,
        Section::Plugins,
        "Plugin",
        "Turn a plugin on or off: off skips its hooks, sidebar sections and prompt sections and refuses its commands from the next turn. A provider's plugin goes off at once, and its accounts can't be used while it is off.",
        Kind::Bool,
    )
    .sites(PLUGIN_SITE)
    .mid()
    .default(Default::Const("on"))
    .applies(Applies::NextTurnMayRebuild),
    spec(
        PLUGIN_HOOK,
        Section::Plugins,
        "Plugin hook",
        "Turn one plugin hook on or off; a hook with its own switch setting follows that setting instead.",
        Kind::Bool,
    )
    .sites(PLUGIN_HOOK_SITE)
    .mid()
    .default(Default::Const("on"))
    .applies(Applies::NextTurnKept),
    spec(
        PLUGIN_INSTALLED,
        Section::Plugins,
        "Installed",
        "Remove a plugin, or add it back, from the next turn; a provider's plugin at once. Removing a provider deletes no account, key or sign-in.",
        Kind::Bool,
    )
    .sites(PLUGIN_SITE)
    .mid()
    .default(Default::Const("on"))
    .applies(Applies::NextTurnMayRebuild),
    spec(
        PLUGIN_SLOT,
        Section::Plugins,
        "Sidebar section",
        "Show one plugin's sidebar section, or leave it out of the sidebar.",
        Kind::Bool,
    )
    .sites(PLUGIN_SLOT_SITE)
    .display(&[], Default::Rule("the section's manifest default")),
    spec(
        names::PRELOAD,
        Section::Chat,
        "Start with",
        "core starts with all permitted Wisp tools; none starts with search and use. On resume, tools no longer held by this choice arrive at the first message.",
        Kind::Preload,
    )
    .default(Default::Const(PRELOAD_CORE))
    .applies(Applies::NextSession)
    .owner(Owner::Host)
    .full_screen(),
    spec(
        names::PRESET,
        Section::Agents,
        "Main preset",
        "The preset the main agent starts from: its instructions and tool limits, and its account, model and effort where no other setting chooses them.",
        Kind::AgentName,
    )
    .owner(Owner::Host),
    spec(
        names::PRICE_CACHE_READ,
        Section::Models,
        "Cache read price",
        "US dollars per million tokens read from the cache; set all three prices or none.",
        Kind::Usd { none: false },
    )
    .sites(MODEL_FACT)
    .default(Default::Rule(SHIPPED_PRICE))
    .owner(Owner::Host),
    spec(
        names::PRICE_INPUT,
        Section::Models,
        "Input price",
        "US dollars per million input tokens; set all three prices or none.",
        Kind::Usd { none: false },
    )
    .sites(MODEL_FACT)
    .default(Default::Rule(SHIPPED_PRICE))
    .owner(Owner::Host),
    spec(
        names::PRICE_OUTPUT,
        Section::Models,
        "Output price",
        "US dollars per million output tokens; set all three prices or none.",
        Kind::Usd { none: false },
    )
    .sites(MODEL_FACT)
    .default(Default::Rule(SHIPPED_PRICE))
    .owner(Owner::Host),
    spec(
        names::PRICING,
        Section::Endpoints,
        "Pricing",
        "Estimate costs at list prices, or show tokens only.",
        Kind::Choice(PRICINGS),
    )
    .sites(ENDPOINT)
    .mid()
    .default(Default::Const(PRICING_LIST))
    .applies(Applies::Now)
    .owner(Owner::Host),
    spec(
        names::PROVIDER,
        Section::Endpoints,
        "Provider",
        "Which provider the account speaks to.",
        Kind::Choice(PROVIDERS),
    )
    .connection()
    .default(Default::Const(PROVIDER_ANTHROPIC)),
    spec(
        names::REFUSAL_FALLBACK,
        Section::Advanced,
        "Refusal fallback",
        "Re-run refused requests on Anthropic's fallback model.",
        Kind::Bool,
    )
    .mid()
    .flags(&["--no-fallback"])
    .default(Default::Const("on"))
    .applies(Applies::NextTurnKept),
    spec(
        names::RETRY_BUDGET_USD,
        Section::Advanced,
        "Retry budget",
        "Cap what retries of failed requests may cost per turn, in US dollars.",
        Kind::Usd { none: true },
    )
    .mid()
    .flags(&["--retry-budget"])
    .default(Default::Const("none"))
    .applies(Applies::Now),
    spec(
        names::SELF_CHECK_BEFORE_END,
        Section::Chat,
        "Self-check",
        "Before a turn of the main agent that ran tools ends, ask the model once to check its work against the task.",
        Kind::Bool,
    )
    .sites(GLOBAL_AGENT)
    .mid()
    .flags(&["--self-check"])
    .default(Default::Const("off"))
    .applies(Applies::NextTurnKept)
    .plugin("self-check"),
    spec(
        names::STORE_ID,
        Section::Endpoints,
        "Store ID",
        concat!("Which records in ", crate::credentials_file!(), " belong to this config; written by Wisp."),
        Kind::Choice(&[]),
    )
    .owner(Owner::Host)
    .secrecy(Secrecy::Hidden),
    spec(
        names::TIME,
        Section::Chat,
        "Message time",
        "Stamp each message with the time it was sent: local (offset only), utc or off.",
        Kind::Choice(&MessageTime::WORDS),
    )
    .sites(GLOBAL_AGENT)
    .mid()
    .flags(&["--time"])
    .default(Default::Const("local"))
    .applies(Applies::NextTurnKept)
    .plugin("time"),
    spec(
        names::TOOL_OUTPUT_MAX_AGE_DAYS,
        Section::Advanced,
        "Tool output kept",
        "How many days other sessions' saved tool outputs are kept; Wisp removes older ones when it starts, and none keeps them.",
        Kind::Int {
            min: 1,
            max: MAX_AGE_DAYS_MAX,
            none: true,
        },
    )
    .default(Default::Const("7")),
    spec(
        names::TOOLS,
        Section::Chat,
        "Tools",
        "Which tools the model may use, or none. A saved list is exact: a tool added in a later \
version of Wisp isn't switched on for it.",
        Kind::ToolNames,
    )
    .sites(GLOBAL_AGENT)
    .mid()
    .flags(&["--tools"])
    .dynamic(Dynamic::Tools)
    .default(Default::Rule("tools on by default"))
    .applies(Applies::NextTurnMayRebuild)
    .owner(Owner::Host)
    .full_screen(),
    spec(
        names::TURNED_OFF,
        Section::Chat,
        "Turned off",
        "Tools, skills and servers that can't be found or used. Next message.",
        Kind::Names,
    )
    .mid()
    .applies(Applies::NextTurnMayRebuild)
    .owner(Owner::Host),
    spec(
        names::URL,
        Section::Endpoints,
        "URL",
        "The account's URL; change it with wisp account edit.",
        Kind::Secret,
    )
    .connection()
    .secrecy(Secrecy::Secret),
    spec(
        names::URL_ENV,
        Section::Endpoints,
        "URL variable",
        "The variable that holds the account's URL, where no keychain is available.",
        Kind::VarName,
    )
    .connection(),
    spec(
        names::VOICE_ENDPOINT,
        Section::Voice,
        "Voice account",
        "The account voice calls go through. Only accounts that offer voice are listed.",
        Kind::EndpointName,
    )
    .dynamic(Dynamic::VoiceEndpoints)
    .applies(Applies::NextCall),
    spec(
        names::VOICE_INSTRUCTIONS,
        Section::Voice,
        "Voice instructions",
        "The file of instructions the subscription's voice gets when a call opens; the API voice uses its own. A relative path starts at the config file's folder.",
        Kind::Path,
    )
    .applies(Applies::NextCall),
    spec(
        names::VOICE_LANGUAGE,
        Section::Voice,
        "Language",
        "The language spoken on calls, which helps the API voice hear it; unset, it is detected from speech.",
        Kind::Language,
    )
    .default(Default::Rule("detected from speech"))
    .applies(Applies::NextCall),
    spec(
        names::VOICE_VOICE,
        Section::Voice,
        "Voice",
        "The voice that speaks on calls, from the voices the voice account offers.",
        Kind::VoiceName,
    )
    .dynamic(Dynamic::Voices)
    .default(Default::Rule("the account's default voice"))
    .applies(Applies::NextCall),
    spec(
        names::WEB_SEARCH,
        Section::Models,
        "Web search",
        "Which versions of the provider's web search tool this model supports; none means no native search.",
        Kind::List(WEB_SEARCH_VERSIONS),
    )
    .sites(MODEL_FACT)
    .default(Default::Rule("the shipped fact, else none"))
    .owner(Owner::Host),
];

static DECLARED: Mutex<BTreeMap<&'static str, &'static SettingSpec>> = Mutex::new(BTreeMap::new());

fn declared() -> MutexGuard<'static, BTreeMap<&'static str, &'static SettingSpec>> {
    DECLARED.lock().unwrap_or_else(PoisonError::into_inner)
}

pub fn row_name(plugin: &str, name: &str) -> Result<String, String> {
    if !super::plugin_id(plugin) {
        return Err(format!("plugin `{plugin}` must be {PLUGIN_ID_RULE}"));
    }
    if name.len() > 32
        || !name.starts_with(|c: char| c.is_ascii_lowercase())
        || !super::name_segment(name)
    {
        return Err(format!(
            "setting `{name}` must be 1 to 32 of a-z, 0-9 and _, starting with a letter"
        ));
    }
    if [ENABLED_KEY, INSTALLED_KEY, HOOKS_TABLE, SLOTS_TABLE].contains(&name) {
        return Err(format!("setting `{name}` is a per-plugin key"));
    }
    if let Some(word) = super::path::secret_word(name) {
        return Err(format!(
            "setting `{name}` holds `{word}`, a word kept for secrets"
        ));
    }
    Ok(format!("plugins.{plugin}.{name}"))
}

fn leaked(text: String) -> &'static str {
    Box::leak(text.into_boxed_str())
}

pub fn declare(plugin: &str, row: &SettingRow) -> Option<&'static SettingSpec> {
    let name = row_name(plugin, &row.name).ok()?;
    let mut rows = declared();
    if let Some(spec) = rows.get(name.as_str()) {
        return Some(*spec);
    }
    let (kind, default) = match row.kind {
        RowKind::Bool { default } => (Kind::Bool, if default { "on" } else { "off" }.to_owned()),
        RowKind::Int { min, max, default } if min <= default && default <= max => (
            Kind::Int {
                min,
                max,
                none: false,
            },
            default.to_string(),
        ),
        _ => return None,
    };
    let spec = Box::leak(Box::new(
        spec(
            leaked(name),
            Section::Plugins,
            leaked(row.label.clone()),
            leaked(row.help.clone()),
            kind,
        )
        .mid()
        .plugin(leaked(plugin.to_owned()))
        .default(Default::Const(leaked(default)))
        .applies(Applies::NextTurnMayRebuild),
    ));
    rows.insert(spec.name, spec);
    Some(spec)
}

pub fn all() -> impl Iterator<Item = &'static SettingSpec> {
    SETTINGS
        .iter()
        .chain(declared().values().copied().collect::<Vec<_>>())
}

pub fn find(name: &str) -> Option<&'static SettingSpec> {
    let name = if name == ENDPOINT_FILE_KEY {
        names::ENDPOINT
    } else {
        name
    };
    SETTINGS
        .iter()
        .find(|s| s.name == name)
        .or_else(|| declared().get(name).copied())
}

pub const ENDPOINT_FILE_KEY: &str = "default";

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ValueError {
    pub rule: String,
}

pub const A_STRING: &str = "a string";
pub const TOKEN_COUNT: &str = "a token count, such as 8000 or \"8k\"";
pub const TOKENS_OR_SHARE: &str = "a number of tokens (400000) or a share of the window (\"85%\")";
pub const TRUE_OR_FALSE: &str = "true or false";
pub const USD_WORDS: &str = "a number of US dollars, such as 0.5";
const LANGUAGE_RULE: &str = "a two-letter language code (ISO 639-1), such as en";
macro_rules! tool_names {
    () => {
        "a list of tool names"
    };
}
pub(crate) use tool_names;

pub const TOOL_NAMES: &str = tool_names!();
pub const PLUGIN_ID_RULE: &str = "2 to 32 characters of a-z, 0-9 and -, starting with a letter";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Form {
    File,
    Docs,
    Rule,
    Unit,
}

pub fn one_of<S: AsRef<str>>(values: &[S]) -> String {
    let words: Vec<&str> = values.iter().map(AsRef::as_ref).collect();
    match words.as_slice() {
        [one] => (*one).to_owned(),
        [a, b] => format!("{a} or {b}"),
        many => format!("one of {}", many.join(", ")),
    }
}

pub fn quoted_or(choices: &[&str]) -> String {
    if choices.is_empty() {
        return "a value Wisp writes".to_owned();
    }
    let quoted: Vec<String> = choices.iter().map(|c| format!("\"{c}\"")).collect();
    listed(&quoted, "or")
}

pub fn listed<S: AsRef<str>>(items: &[S], word: &str) -> String {
    let items = items.iter().map(|s| s.as_ref().to_owned()).collect();
    joined(items, word, str::to_owned).concat()
}

pub fn joined<T>(mut items: Vec<T>, last_word: &str, word: impl Fn(&str) -> T) -> Vec<T> {
    let last = items.pop();
    let mut out = Vec::new();
    for item in items {
        if !out.is_empty() {
            out.push(word(", "));
        }
        out.push(item);
    }
    if let Some(last) = last {
        if !out.is_empty() {
            out.push(word(&format!(" {last_word} ")));
        }
        out.push(last);
    }
    out
}

pub fn named<T: Clone>(words: &[&str], values: &[T], word: &str) -> Option<T> {
    words
        .iter()
        .zip(values)
        .find(|(w, _)| **w == word)
        .map(|(_, v)| v.clone())
}

pub fn word_of<T: PartialEq>(
    words: &[&'static str],
    values: &[T],
    value: &T,
) -> Option<&'static str> {
    words
        .iter()
        .zip(values)
        .find(|(_, v)| *v == value)
        .map(|(w, _)| *w)
}

pub fn color_word(raw: &str) -> &str {
    match raw {
        "24bit" => "truecolor",
        other => other,
    }
}

pub(super) fn ticked(choices: &[&str]) -> Vec<String> {
    choices.iter().map(|c| format!("`{c}`")).collect()
}

impl SettingSpec {
    pub fn is_built(&self) -> bool {
        self.status == Status::Built
    }

    pub fn has_site(&self, site: Site) -> bool {
        self.sites.contains(&site)
    }

    pub fn defers(&self, value: Option<&SettingValue>) -> bool {
        self.section == Section::Display
            && matches!(value, Some(SettingValue::Text(text)) if text == "auto")
    }

    pub fn settles_on_save(&self) -> bool {
        self.mid_session && self.owner == Owner::Kernel && self.has_site(Site::Model)
    }

    pub fn per_plugin(&self) -> bool {
        self.sites
            .iter()
            .any(|s| matches!(s, Site::PluginHook | Site::PluginSlot | Site::Plugin))
    }

    pub fn file_key(&self) -> &'static str {
        if self.name == names::ENDPOINT {
            ENDPOINT_FILE_KEY
        } else {
            self.name
        }
    }

    pub fn leaf(&self) -> &'static str {
        self.name
            .split_once('.')
            .map_or(self.name, |(_, leaf)| leaf)
    }

    pub fn group(&self) -> Option<&'static str> {
        self.name.split_once('.').map(|(group, _)| group)
    }

    pub fn matches(&self, query: &Query) -> bool {
        query.matches(&[self.label, self.name, self.help])
    }

    pub fn describe(&self, form: Form) -> String {
        match (self.kind, form) {
            (Kind::Bool, Form::File) => TRUE_OR_FALSE.to_owned(),
            (Kind::Bool, Form::Docs) => {
                let words = concat!(
                    "`true` or `false` in `",
                    crate::config_file!(),
                    "`, `on` or `off` on the command line"
                );
                if self.env.is_empty() {
                    words.to_owned()
                } else {
                    format!("{words} and in variables")
                }
            }
            (Kind::Bool, Form::Rule | Form::Unit) => "must be on or off".to_owned(),
            (Kind::Choice(choices), Form::File) => quoted_or(choices),
            (Kind::Choice([]), Form::Docs) => "written by Wisp".to_owned(),
            (Kind::Choice(choices), Form::Docs) => one_of(&ticked(choices)),
            (Kind::Choice(choices), Form::Rule | Form::Unit) => {
                format!("must be {}", one_of(choices))
            }
            (Kind::Int { min, max, none }, Form::File) if none => {
                format!("a whole number from {min} to {max}, or \"none\"")
            }
            (Kind::Int { min, max, .. }, Form::File) => {
                format!("a whole number from {min} to {max}")
            }
            (Kind::Int { min, max, none }, Form::Docs) if none => {
                format!("{min} to {max}, or `none`")
            }
            (Kind::Int { min, max, .. }, Form::Docs) => format!("{min} to {max}"),
            (Kind::Int { min, max, none }, Form::Rule) if none => {
                format!("must be a whole number from {min} to {max}, or none")
            }
            (Kind::Int { min, max, .. }, Form::Rule) => {
                format!("must be a whole number from {min} to {max}")
            }
            (Kind::Int { min, max, none }, Form::Unit) if none => {
                format!("a whole number, {min} to {max}, or none")
            }
            (Kind::Int { min, max, .. }, Form::Unit) => format!("a whole number, {min} to {max}"),
            (Kind::Tokens { .. }, Form::File) => TOKEN_COUNT.to_owned(),
            (Kind::Tokens { .. }, Form::Docs) => "tokens (`40000`, `40k`, `1M`)".to_owned(),
            (Kind::Tokens { .. }, Form::Rule) => {
                format!("needs {}", TOKEN_COUNT.replace('"', ""))
            }
            (Kind::Tokens { min, max }, Form::Unit) if max == u64::MAX => {
                format!("tokens, {} or more", thousands(min))
            }
            (Kind::Tokens { min, max }, Form::Unit) => {
                format!("tokens, {} to {}", thousands(min), thousands(max))
            }
            (Kind::TokensOrPercent, Form::File) => TOKENS_OR_SHARE.to_owned(),
            (Kind::TokensOrPercent, Form::Docs) => {
                "tokens (`400k`) or a share of the window (`85%`)".to_owned()
            }
            (Kind::TokensOrPercent, Form::Rule) => {
                "needs a token count (400000 or 400k) or a share of the window (85%)".to_owned()
            }
            (Kind::TokensOrPercent, Form::Unit) => "tokens, or a share such as 60%".to_owned(),
            (Kind::Usd { none: true }, Form::File) => format!("{USD_WORDS}, or \"none\""),
            (Kind::Usd { none: false }, Form::File) => USD_WORDS.to_owned(),
            (Kind::Usd { none: true }, Form::Docs) => "US dollars (`0.50`) or `none`".to_owned(),
            (Kind::Usd { none: false }, Form::Docs) => "US dollars (`0.50`)".to_owned(),
            (Kind::Usd { none: true }, Form::Rule) => {
                "needs a number of dollars, such as 0.50, or none".to_owned()
            }
            (Kind::Usd { none: false }, Form::Rule) => {
                "needs a number of dollars, such as 0.50".to_owned()
            }
            (Kind::Usd { none: true }, Form::Unit) => "US dollars, or none".to_owned(),
            (Kind::Usd { none: false }, Form::Unit) => "US dollars".to_owned(),
            (Kind::ModelId, Form::File) => format!("a model ID ({MODEL_ID_CHARS})"),
            (Kind::ModelId, Form::Docs) => "a model ID".to_owned(),
            (Kind::ModelId, Form::Rule | Form::Unit) => {
                format!("needs a model ID of {MODEL_ID_CHARS}")
            }
            (Kind::ModelIds, Form::File | Form::Docs) => MODEL_IDS_RULE.to_owned(),
            (Kind::ModelIds, Form::Rule | Form::Unit) => format!("needs {MODEL_IDS_RULE}"),
            (Kind::EndpointName | Kind::AgentName, Form::File) => {
                format!("a name of {}", endpoint_name_rule())
            }
            (Kind::EndpointName, Form::Docs) => "an account name".to_owned(),
            (Kind::AgentName, Form::Docs) => "an agent type's name".to_owned(),
            (Kind::EndpointName | Kind::AgentName, Form::Rule | Form::Unit) => {
                format!("needs a name of {}", endpoint_name_rule())
            }
            (Kind::VarName, Form::File) => format!("a variable name ({VAR_NAME_CHARS})"),
            (Kind::VarName, Form::Docs) => "a variable name".to_owned(),
            (Kind::VarName, Form::Rule) => format!("needs a variable name of {VAR_NAME_CHARS}"),
            (Kind::VarName, Form::Unit) => format!("a variable name of {VAR_NAME_CHARS}"),
            (Kind::PluginId, Form::File) => "\"default\" or a plugin's id".to_owned(),
            (Kind::PluginId, Form::Docs) => "`default` or a plugin's id".to_owned(),
            (Kind::PluginId, Form::Rule | Form::Unit) => {
                format!("needs default or a plugin id of {PLUGIN_ID_RULE}")
            }
            (Kind::List(choices), Form::File) => {
                format!("a list of {}, or [] for none", quoted_or(choices))
            }
            (Kind::List(choices), Form::Docs) => {
                format!("a list of {}, or `none`", ticked(choices).join(", "))
            }
            (Kind::List(choices), Form::Rule | Form::Unit) => {
                format!("must be none or a list of {}", one_of(choices))
            }
            (Kind::Preload, Form::File | Form::Docs) => {
                "a list of tool names, core for Wisp's tools, or none for search and use".to_owned()
            }
            (Kind::Preload, Form::Rule | Form::Unit) => PRELOAD_RULE.to_owned(),
            (Kind::ToolNames, Form::File) => format!("{TOOL_NAMES}, or [] for none"),
            (Kind::ToolNames, Form::Docs) => {
                format!("{TOOL_NAMES} from the plugin catalog (the `--tools` values), or `none`")
            }
            (Kind::ToolNames, Form::Rule | Form::Unit) => format!("must be none or {TOOL_NAMES}"),
            (Kind::Names, Form::File) => "a list of names, or [] for none".to_owned(),
            (Kind::Names, Form::Docs) => "names of tools, skills and servers, or `none`".to_owned(),
            (Kind::Names, Form::Rule | Form::Unit) => "must be none or a list of names".to_owned(),
            (Kind::SectionNames, Form::File) => {
                "a list of sidebar sections (\"<plugin>/<name>\"), or [] for none".to_owned()
            }
            (Kind::SectionNames, Form::Docs) => {
                "a list of sidebar sections (`<plugin>/<name>`), or `none`".to_owned()
            }
            (Kind::SectionNames, Form::Rule | Form::Unit) => {
                "must be none or a list of sidebar sections, each <plugin>/<name>".to_owned()
            }
            (Kind::Secret, Form::File) => concat!("kept out of ", crate::config_file!()).to_owned(),
            (Kind::Secret, Form::Docs) => "a secret, never typed as a value".to_owned(),
            (Kind::Secret, Form::Rule | Form::Unit) => "is a secret".to_owned(),
            (Kind::VoiceName, Form::File) => format!("a voice's name of {}", endpoint_name_rule()),
            (Kind::VoiceName, Form::Docs) => "a voice's name".to_owned(),
            (Kind::VoiceName, Form::Rule | Form::Unit) => {
                format!("needs a voice's name of {}", endpoint_name_rule())
            }
            (Kind::Language, Form::File | Form::Docs) => LANGUAGE_RULE.to_owned(),
            (Kind::Language, Form::Rule | Form::Unit) => format!("needs {LANGUAGE_RULE}"),
            (Kind::ClientVersion, Form::File) => {
                r#"three dot-separated numbers, such as "1.2.3""#.to_owned()
            }
            (Kind::ClientVersion, Form::Docs | Form::Unit) => "a version such as 1.2.3".to_owned(),
            (Kind::ClientVersion, Form::Rule) => "needs the form 1.2.3".to_owned(),
            (Kind::Path, Form::File) => {
                concat!("a file path, ", config_relative!()).replace('`', "")
            }
            (Kind::Path, Form::Docs) => concat!("a file, ", config_relative!()).to_owned(),
            (Kind::Path, Form::Rule | Form::Unit) => format!("needs {}", self.describe(Form::File)),
        }
    }

    pub fn parse(&self, text: &str) -> Result<SettingValue, ValueError> {
        let bad = || ValueError {
            rule: self.describe(Form::Rule),
        };
        let trimmed = text.trim();
        match self.kind {
            Kind::Bool => parse_bool(trimmed).map(SettingValue::Bool).ok_or_else(bad),
            Kind::Choice(choices) => choices
                .iter()
                .find(|c| **c == trimmed)
                .map(|c| SettingValue::Text((*c).to_owned()))
                .ok_or_else(bad),
            Kind::Int { none: true, .. } if trimmed == "none" => {
                Ok(SettingValue::Text("none".to_owned()))
            }
            Kind::Int { min, max, .. } => trimmed
                .parse::<i64>()
                .ok()
                .filter(|n| (min..=max).contains(n))
                .map(SettingValue::Int)
                .ok_or_else(bad),
            Kind::Tokens { min, max } => parse_tokens(trimmed)
                .filter(|n| (min..=max).contains(n))
                .map(SettingValue::Tokens)
                .ok_or_else(bad),
            Kind::TokensOrPercent => TokensOrPercent::parse(trimmed)
                .map(SettingValue::from)
                .ok_or_else(bad),
            Kind::Usd { none } => {
                if none && trimmed == "none" {
                    return Ok(SettingValue::Text("none".to_owned()));
                }
                parse_usd(trimmed)
                    .map(|micros| SettingValue::Usd { micros })
                    .ok_or_else(bad)
            }
            Kind::ModelId => check_model_id(trimmed)
                .map(|()| SettingValue::Text(trimmed.to_owned()))
                .map_err(|_| bad()),
            Kind::ModelIds => {
                let items = split_list(trimmed);
                if items.iter().all(|i| check_model_id(i).is_ok()) {
                    Ok(SettingValue::List(items))
                } else {
                    Err(bad())
                }
            }
            Kind::EndpointName | Kind::AgentName | Kind::VoiceName => {
                if valid_endpoint_name(trimmed) {
                    Ok(SettingValue::Text(trimmed.to_owned()))
                } else {
                    Err(bad())
                }
            }
            Kind::Language => {
                if trimmed.len() == 2 && trimmed.bytes().all(|b| b.is_ascii_lowercase()) {
                    Ok(SettingValue::Text(trimmed.to_owned()))
                } else {
                    Err(bad())
                }
            }
            Kind::ClientVersion => ClientVersion::parse(trimmed)
                .map(|version| SettingValue::Text(version.to_string()))
                .ok_or_else(bad),
            Kind::Path => {
                if !trimmed.is_empty() && !trimmed.chars().any(char::is_control) {
                    Ok(SettingValue::Text(trimmed.to_owned()))
                } else {
                    Err(bad())
                }
            }
            Kind::VarName => {
                if crate::plain_variable(trimmed) {
                    Ok(SettingValue::Text(trimmed.to_owned()))
                } else {
                    Err(bad())
                }
            }
            Kind::PluginId => {
                if super::path::plugin_id(trimmed) {
                    Ok(SettingValue::Text(trimmed.to_owned()))
                } else {
                    Err(bad())
                }
            }
            Kind::List(choices) => {
                if trimmed == "none" {
                    return Ok(SettingValue::List(Vec::new()));
                }
                let mut items = split_list(trimmed);
                if items.is_empty() || !items.iter().all(|i| choices.contains(&i.as_str())) {
                    return Err(bad());
                }
                items.sort();
                items.dedup();
                Ok(SettingValue::List(items))
            }
            Kind::Preload if trimmed.is_empty() => Err(bad()),
            Kind::Preload => preload_names(&split_list(trimmed)).map(SettingValue::List),
            Kind::ToolNames | Kind::Names => {
                if trimmed == "none" {
                    return Ok(SettingValue::List(Vec::new()));
                }
                let mut items = split_list(trimmed);
                let named =
                    |i: &String| i != "none" && is_tool_name(i) && !crate::looks_like_key(i);
                if items.is_empty() || !items.iter().all(named) {
                    return Err(bad());
                }
                items.sort();
                items.dedup();
                Ok(SettingValue::List(items))
            }
            Kind::SectionNames => {
                if trimmed == "none" {
                    return Ok(SettingValue::List(Vec::new()));
                }
                let mut items = split_list(trimmed);
                if items.is_empty() || !items.iter().all(|i| super::path::section_ref(i)) {
                    return Err(bad());
                }
                for item in &mut items {
                    if let Some((plugin, section)) = item.split_once('/') {
                        *item = format!("{plugin}/{}", super::path::current_slot(plugin, section));
                    }
                }
                items.sort();
                items.dedup();
                Ok(SettingValue::List(items))
            }
            Kind::Secret => Err(bad()),
        }
    }

    pub fn parse_list(&self, items: &[String]) -> Result<SettingValue, ValueError> {
        match self.kind {
            Kind::Preload => preload_names(items).map(SettingValue::List),
            Kind::List(_) | Kind::ModelIds | Kind::ToolNames | Kind::Names | Kind::SectionNames
                if items.is_empty() =>
            {
                Ok(SettingValue::List(vec![]))
            }
            Kind::List(_) | Kind::ModelIds | Kind::ToolNames | Kind::Names | Kind::SectionNames => {
                self.parse(&items.join(","))
            }
            _ => Err(ValueError {
                rule: self.describe(Form::Rule),
            }),
        }
    }

    pub fn default_text(&self) -> Option<&'static str> {
        match self.default {
            Default::Const(text) | Default::Detected(text) => Some(text),
            Default::None | Default::Rule(_) => None,
        }
    }

    pub fn default_value(&self) -> Option<SettingValue> {
        match self.default {
            Default::Const(text) => self.parse(text).ok(),
            _ => None,
        }
    }

    pub fn default_for(&self, credential: Option<&CredentialKind>) -> Option<SettingValue> {
        (self.name == names::MAX_IN_FLIGHT)
            .then(|| SettingValue::Int(max_in_flight_default(credential)))
    }

    pub fn default_words(&self) -> String {
        match self.default {
            Default::None => "none".to_owned(),
            Default::Const(text) => text.to_owned(),
            Default::Rule(rule) => rule.to_owned(),
            Default::Detected(text) => format!("{text} (detected)"),
        }
    }

    pub fn shown(&self, value: &SettingValue) -> String {
        match (self.kind, self.secrecy, value) {
            (_, Secrecy::Secret | Secrecy::Hidden, _) => "set".to_owned(),
            (Kind::VarName, _, SettingValue::Text(name)) => shown_variable(name).to_owned(),
            (Kind::ModelId, _, SettingValue::Text(id)) => shown_model(id).to_owned(),
            (Kind::Path, _, SettingValue::Text(path)) => shown_text(path).to_owned(),
            (Kind::ModelIds, _, SettingValue::List(ids)) => {
                SettingValue::List(ids.iter().map(|id| shown_model(id).to_owned()).collect())
                    .shown()
            }
            (Kind::ToolNames | Kind::Names | Kind::Preload, _, SettingValue::List(names)) => {
                SettingValue::List(
                    names
                        .iter()
                        .map(|name| {
                            if crate::plugin::shows_tool_name(name) {
                                name.clone()
                            } else {
                                crate::NOT_SHOWN.to_owned()
                            }
                        })
                        .collect(),
                )
                .shown()
            }
            (_, _, value) => value.shown(),
        }
    }
}

pub fn is_tool_name(name: &str) -> bool {
    (1..=64).contains(&name.len())
        && name
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-')
}

fn split_list(text: &str) -> Vec<String> {
    text.split(',')
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_owned)
        .collect()
}
