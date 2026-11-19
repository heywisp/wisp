use std::fmt;

use serde::{Deserialize, Serialize};

mod docs;
mod listed;
pub mod names;
mod path;
mod registry;
mod value;
mod wire;

pub use docs::render_docs;
pub use listed::{
    Chat, EffortVariant, Landed, Landing, Offer, Remembered, display_effort, effort_on, every,
    fallback_effort, lands_on, offered, offers, reachable, runs_on, serves, siblings,
    takes_no_effort,
};
pub(crate) use path::name_segment;
pub use path::{
    Endpoints, NOT_A_NAME, Place, SitePath, SpecRef, Target, TargetError, current_slot,
    former_slots, holds_secret_word, hook_name, nearest, nearest_in, nearest_name, parse_target,
    parse_target_among, plugin_id, quotable, secret_word, section_name, section_ref,
};
pub(crate) use registry::tool_names;
pub use registry::{
    A_STRING, AGENTS_GROUP, AGENTS_NOTE, AMBIGUOUS, AUTH_ANTHROPIC_SUBSCRIPTION,
    AUTH_ANTIGRAVITY_SUBSCRIPTION, AUTH_API_KEY, AUTH_BEARER, AUTH_CHATGPT_SUBSCRIPTION, AUTHS,
    Applies, AppliesTo, CACHE_KEPT, CACHE_KEPT_EFFORT, CACHE_KEPT_TOOLS, CLAUDE_WEB_SEARCH, COLORS,
    COPY_ON_SELECT, DEFAULT_MODEL, Default, Dynamic, ENABLED_KEY, ENDPOINT_FILE_KEY, Form,
    GEMINI_WEB_SEARCH, HOOKS_TABLE, INSTALLED_KEY, INSTRUCTION_EXTRAS, KEY_STORE_FILE,
    KEY_STORE_KEYRING, KEY_STORES, Kind, MAX_AGENTS_MAX, MAX_DEPTH_MAX, MAX_IN_FLIGHT_MAX,
    MAX_RUNNING_MAX, MOTIONS, NEW_CHATS, OPENAI_WEB_SEARCH, Owner, PLUGIN_ENABLED, PLUGIN_HOOK,
    PLUGIN_ID_RULE, PLUGIN_INSTALLED, PLUGIN_SLOT, PLUGINS_GROUP, PRELOAD_CORE, PRELOAD_NONE,
    PRESETS_GROUP, PRESETS_TABLE, PRICING_LIST, PRICING_NONE, PRICINGS, PROVIDER_ANTHROPIC,
    PROVIDER_GEMINI, PROVIDER_OPENAI, PROVIDERS, SCROLL_SPEED_MAX, SETTINGS, SIDEBAR_MODES,
    SLOTS_TABLE, Secrecy, Section, SettingSpec, Site, Status, THEMES, TOKEN_COUNT, TOKENS_OR_SHARE,
    TOOL_NAMES, TRUE_OR_FALSE, USD_WORDS, ValueError, WEB_SEARCH_VERSIONS, agents_group_keys, all,
    color_word, declare, find, is_tool_name, joined, listed, named, one_of, preload_names,
    provider_plugin, quoted_or, resolve_preload, row_name, word_of,
};
pub use value::{
    ClientVersion, MICROS_PER_USD, SettingValue, TokensOrPercent, parse_bool, parse_tokens,
    parse_usd, thousands, usd_shown, usd_text,
};
pub use wire::{
    AccountModel, AppliedChange, By, CompactionQuote, Consequence, CredentialKind, EndpointSummary,
    FileState, KeyVariable, Layer, LoggedChange, LoggedTarget, ModelEntry, Overridden,
    OverrideScope, PlacedValue, ResolvedSetting, Restored, Saved, SavedEntry, SavedValue,
    SecretState, SettingChange, SettingName, SettingTarget, SettingsSnapshot, Shadowed,
    SnapshotContext, SwitchQuote, SwitchTarget, TargetKind, VoiceUnset, When, WindowRoom,
    Withdrawal, site_path, sorted_changes,
};

pub fn shipped_origin(model: &str) -> String {
    format!("models.toml {model}")
}

pub fn effort_refusal(
    model: &str,
    effort: crate::Effort,
    asked: bool,
    takes: &[crate::Effort],
) -> String {
    let how = if asked {
        "doesn't take effort"
    } else {
        "doesn't take the current effort"
    };
    if takes.is_empty() {
        return format!("{model} {how} {}", effort.as_str());
    }
    let takes: Vec<&str> = takes.iter().map(|e| e.as_str()).collect();
    format!(
        "{model} {how} {}; it takes {}",
        effort.as_str(),
        takes.join(", ")
    )
}

pub const RESERVE_SHARE_PERCENT: u64 = 15;

pub const RESERVE_MIN: u64 = 40_000;

pub const POINT_FLOOR: u64 = 40_000;

pub fn reserve_share(window: u64) -> u64 {
    window / 100 * RESERVE_SHARE_PERCENT + (window % 100 * RESERVE_SHARE_PERCENT).div_ceil(100)
}

pub fn reserve(window: u64, max_tokens: u32) -> u64 {
    reserve_share(window)
        .max(RESERVE_MIN)
        .max(u64::from(max_tokens))
}

pub fn default_point(window: u64, max_tokens: u32) -> u64 {
    window.saturating_sub(reserve(window, max_tokens))
}

pub fn smallest_window(point: u64, max_tokens: u32) -> u64 {
    if point == 0 {
        return 0;
    }
    let ratio = (u128::from(point) * 100).div_ceil(u128::from(100 - RESERVE_SHARE_PERCENT));
    u64::try_from(ratio)
        .unwrap_or(u64::MAX)
        .max(point.saturating_add(RESERVE_MIN.max(u64::from(max_tokens))))
}

pub fn share_point(window: u64, percent: Percent) -> u64 {
    let p = u64::from(percent.get());
    window / 100 * p + window % 100 * p / 100
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(try_from = "u8", into = "u8")]
pub struct Percent(u8);

impl Percent {
    pub const MAX: u8 = 100;

    pub fn new(value: u8) -> Result<Self, PercentTooLarge> {
        if value > Self::MAX {
            Err(PercentTooLarge(value))
        } else {
            Ok(Self(value))
        }
    }

    pub fn get(self) -> u8 {
        self.0
    }
}

impl TryFrom<u8> for Percent {
    type Error = PercentTooLarge;

    fn try_from(value: u8) -> Result<Self, Self::Error> {
        Self::new(value)
    }
}

impl From<Percent> for u8 {
    fn from(percent: Percent) -> u8 {
        percent.0
    }
}

impl fmt::Display for Percent {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}%", self.0)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PercentTooLarge(pub u8);

impl fmt::Display for PercentTooLarge {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}% is over 100%", self.0)
    }
}

impl std::error::Error for PercentTooLarge {}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct CompactionCost {
    pub estimate_micros: u64,
    pub head_write_micros: u64,
}

#[cfg(test)]
mod tests;
