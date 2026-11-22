use std::fmt;

use wisp_protocol::agents::{AgentPath, ROOT_NAME, shown_agent};
use wisp_protocol::context::format_tokens;
use wisp_protocol::plugin::{CommandWait, PluginNote, PluginRecord, PluginSource};
use wisp_protocol::settings::{CACHE_KEPT, CACHE_KEPT_TOOLS, thousands};
use wisp_protocol::{
    AccountSpend, BackgroundReason, BusyWait, CacheEffect, CompactionFailure, Cost, ErrorDetail,
    ErrorKind, ExitStatus, Fact as ToolFact, FailurePhase, FailureSource, GaveUp,
    MIXED_BILLING_NOTE, Marked, NotRunReason, ProcessEnd, RebuildReason, SearchAction, SignalKind,
    StopReason, StoppedBy, TermSize, ToolAction, ToolChangeVia, ToolOutcome, TurnOutcome,
    TurnStats, UnfinishedReason, Usage, marked_amount,
};

use crate::chrome::{KeyHint, approx, model_line};
use crate::theme::Token;
use crate::theme::glyphs::{GlyphSet, Glyphs};

mod resume;
mod retry;
mod rewind;
pub use resume::*;
pub use retry::*;
pub use rewind::*;
pub use wisp_protocol::COMPACT_BETWEEN_TURNS;
pub use wisp_protocol::format_usd as usd;

pub const STOPPED: &str = "Stopped.";
pub const SAVING_SETTINGS: &str = "saving settings";
pub const CHECKING_COMPACTION: &str = "checking for a compaction";
pub const APPLYING_SETTINGS: &str = "applying settings";

pub fn ongoing(phrase: &str, glyphs: &Glyphs) -> String {
    format!("{phrase}{}", glyphs.more)
}

pub const NOTHING_KEPT: &str = "Nothing from this turn was kept.";
pub const NOT_KEPT: &str = "not kept in history";
pub const CACHE_MISS: &str = "cache miss";
pub const COST_UNKNOWN: &str = "cost unknown";
pub const POSSIBLY_BILLED: &str = "cost unknown, possibly billed";
pub const THINKING: &str = "Thinking";
pub const THOUGHT: &str = "Thought";
pub const READY: &str = "Ready";
pub const ESC_KEY: &str = "esc";
pub const SEP: &str = " \u{b7} ";
pub const ENTER_KEY: &str = "enter";
pub const TIME_UNAVAILABLE: &str = "time unavailable";
pub const TAB_KEY: &str = "tab";
pub const SPACE_KEY: &str = "space";
pub const CTRL_C_KEY: &str = "ctrl-c";
pub const NEW_LINE_KEY: &str = "ctrl-j";
pub const NEW_LINE_KEY_KITTY: &str = "shift-enter";
pub const TO_STOP: &str = "to stop";
pub const OPEN_WORD: &str = "open";
pub const COPY_WORD: &str = "copy";
pub const COPYING_WORD: &str = "copying";
pub const COPIED_WORD: &str = "copied";
pub const SENT_WORD: &str = "sent";
pub const NOT_COPIED_WORD: &str = "not copied";
pub const NEXT_WORD: &str = "next";
pub const CHOOSE_WORD: &str = "choose";
pub const FINISH_WORD: &str = "finish";
pub const CANCEL_WORD: &str = "cancel";
pub const STOP_WORD: &str = "stop";
pub const MOVE_WORD: &str = "move";
pub const TOGGLE_WORD: &str = "toggle";
pub const SAVE_WORD: &str = "save";
pub const YES: &str = "Yes";
pub const NO: &str = "No";
pub const YES_NO: [&str; 2] = [YES, NO];
pub const KEEP: &str = "Keep";
pub const REMOVE: &str = "Remove";
pub const REMOVE_KEEP: [&str; 2] = [REMOVE, KEEP];

pub fn esc(word: &str) -> KeyHint {
    KeyHint::new(ESC_KEY, word).as_exit()
}

pub fn enter_choose() -> KeyHint {
    KeyHint::new(ENTER_KEY, CHOOSE_WORD)
}

pub fn to_stop(key: &str) -> KeyHint {
    KeyHint::new(key, TO_STOP)
}

pub fn esc_to_stop() -> KeyHint {
    to_stop(ESC_KEY)
}

pub fn arrow_keys(set: GlyphSet) -> [&'static str; 2] {
    match set {
        GlyphSet::Unicode => ["\u{2191}", "\u{2193}"],
        GlyphSet::AmbiguousWide | GlyphSet::Ascii => ["up", "down"],
    }
}

pub fn arrows(set: GlyphSet) -> KeyHint {
    KeyHint::new(crate::chrome::up_down(&Glyphs::of(set)), MOVE_WORD)
}

pub fn pick_keys(
    held: Option<bool>,
    action: KeyHint,
    take: KeyHint,
    way_out: Option<KeyHint>,
    set: GlyphSet,
) -> Vec<KeyHint> {
    let keys = match held {
        None => vec![arrows(set)],
        Some(true) => vec![action, arrows(set)],
        Some(false) => vec![take],
    };
    keys.into_iter().chain(way_out).collect()
}

pub fn page_scroll() -> KeyHint {
    KeyHint::new("pgup/pgdn", "scroll")
}
pub const WORKING: &str = "Working";
pub const DONE: &str = "Done";
pub const PLACEHOLDER: &str = "Ask anything, or type / for commands";

pub const PLACEHOLDER_SHORT: &str = "Ask anything";
pub const NO_API_CALLS: &str = "replay, no API calls";
pub const NOT_LOGGING: &str = "not logging this session";
pub const TAGLINE_OWN: &str = "Your own AI.";
pub const TAGLINE_COMPUTER: &str = "With a computer of its own.";
pub const EFFORT: &str = "effort";
pub const KEY_SEND: &str = "send";
pub const KEY_NEW_LINE: &str = "new line";

pub fn search_off_hint(sep: &str, chat_only: bool) -> String {
    if chat_only {
        format!("web search off{sep}{CHAT_ONLY}{sep}{KEY_CTRL_O} for why")
    } else {
        format!("web search off{sep}{KEY_CTRL_O} for why")
    }
}

pub fn ago(secs: u64) -> String {
    let minutes = secs / 60;
    if minutes < 60 {
        format!("{} min ago", minutes.max(1))
    } else {
        format!("{} h ago", minutes / 60)
    }
}

pub fn clock(secs: u64) -> String {
    duration(secs.saturating_mul(1000))
}

pub const VERBS: [&str; 20] = [
    "Kindling",
    "Drifting",
    "Glimmering",
    "Flickering",
    "Hovering",
    "Gleaming",
    "Wandering",
    "Weaving",
    "Gathering",
    "Musing",
    "Mulling",
    "Glowing",
    "Shimmering",
    "Floating",
    "Tracing",
    "Sifting",
    "Lantern-lighting",
    "Unfurling",
    "Brightening",
    "Settling",
];

pub fn verb(seed: u64, turn: u64) -> &'static str {
    let len = VERBS.len() as u64;
    let at = (seed % len + turn % len) % len;
    VERBS[usize::try_from(at).unwrap_or(0)]
}

pub fn tokens_down(arrow: &str, approx: &str, n: u64) -> String {
    format!("{arrow} {approx}{}", plural(n, "token", "tokens"))
}

pub const THINKING_PHASE: &str = "thinking";

pub fn thinking_with(effort: &str) -> String {
    format!("thinking with {effort} effort")
}

pub fn retrying_in(secs: u64, attempt: u32, max: u32, cause: &str) -> String {
    let count = retry_count(attempt, max);
    format!("{}{count}: {cause}", retrying_head(secs))
}

fn retry_count(attempt: u32, max: u32) -> String {
    match (attempt, max) {
        (0, _) => String::new(),
        (_, max) if max > 0 => format!(" ({attempt} of {max})"),
        _ => format!(" (attempt {attempt})"),
    }
}

fn retry_wait(secs: u64) -> String {
    if secs == 0 {
        "0 s".to_owned()
    } else {
        finished_duration(secs.saturating_mul(1000))
    }
}

pub fn retrying_head(secs: u64) -> String {
    format!("Retrying in {}", retry_wait(secs))
}

pub fn busy_waited(busy: BusyWait) -> String {
    match busy.waited_ms {
        ms if ms < 60_000 => format!("waited {} s", ms / 1_000),
        ms => format!("waited {} min", ms / 60_000),
    }
}

pub fn retry_cause(kind: ErrorKind, phase: Option<FailurePhase>) -> &'static str {
    match (kind, phase) {
        (ErrorKind::Network, Some(FailurePhase::Connect)) => "couldn't connect",
        (ErrorKind::Network, _) => "connection dropped",
        (ErrorKind::Timeout, Some(FailurePhase::Stream)) => "the stream stalled",
        (ErrorKind::Timeout, _) => "no response",
        (ErrorKind::Server, _) => "server error",
        (ErrorKind::Overloaded, _) => "overloaded",
        (ErrorKind::RateLimit, _) => "rate limited",
        (ErrorKind::InvalidResponse, _) => "an unreadable reply",
        (other, _) => other.as_str(),
    }
}

pub struct RetryNotice<'a> {
    pub child: bool,
    pub provider: &'a str,
    pub kind: ErrorKind,
    pub detail: Option<&'a ErrorDetail>,
    pub reason: &'a str,
    pub after_secs: u64,
    pub step: u32,
    pub attempt: u32,
    pub max: u32,
    pub delay_ms: u64,
    pub discarded_blocks: u32,
    pub discarded_chars: u64,
}

pub fn retry_notice(n: &RetryNotice<'_>) -> Vec<String> {
    let phase = n.detail.map(|d| d.phase);
    let cause = retry_cause(n.kind, phase);
    let reason = n.reason.trim().trim_end_matches('.');
    let first = if n.kind == ErrorKind::InvalidResponse {
        if reason.is_empty() {
            format!("{} sent a reply Wisp could not read.", n.provider)
        } else if reason
            .strip_prefix(n.provider)
            .and_then(|rest| rest.strip_prefix(" returned "))
            .is_some_and(|what| !what.trim().is_empty())
        {
            format!("{reason}.")
        } else {
            format!("{} sent a reply Wisp could not read: {reason}.", n.provider)
        }
    } else if phase == Some(FailurePhase::Stream) {
        let why = if reason.is_empty() || reason.eq_ignore_ascii_case(cause) {
            cause.to_owned()
        } else {
            format!("{cause}: {reason}")
        };
        format!(
            "The stream from {} broke after {} s ({why}).",
            n.provider, n.after_secs
        )
    } else {
        let sentence = error_sentence(n.provider, n.kind);
        let sentence = sentence.trim_end_matches('.');
        if names_the_cause(sentence, cause, own_message(reason)) {
            format!("{sentence}.")
        } else {
            format!("{sentence} ({reason}).")
        }
    };
    let count = retry_count(n.attempt, n.max);
    let step = if n.step > 0 {
        format!("Retrying step {}{count}", n.step)
    } else {
        format!("Retrying{count}")
    };
    let retry = format!("{step} in {}.", retry_wait(seconds_up(n.delay_ms)));
    let second = match discarded(n.step, n.discarded_blocks, n.discarded_chars) {
        Some(discarded) => format!("{discarded} {retry}"),
        None => retry,
    };
    let mut notice = vec![first, second];
    if !n.child
        && n.step > 0
        && n.detail
            .is_some_and(|detail| detail.limit_reached.is_some())
    {
        notice.push(format!(
            "{ESC_KEY} stops waiting; then {RETRY_WORD} or {}.",
            SWITCH_ACCOUNT.to_ascii_lowercase()
        ));
    }
    notice
}

fn discarded(step: u32, blocks: u32, chars: u64) -> Option<String> {
    if blocks == 0 {
        return None;
    }
    let kept = match step.saturating_sub(1) {
        0 => String::new(),
        1 => "; 1 earlier step is kept".to_owned(),
        k => format!("; {k} earlier steps are kept"),
    };
    Some(format!(
        "Discarded {} of an unfinished reply{kept}.",
        plural(chars, "character", "characters")
    ))
}

pub const RESENT: &str = "too many cache breakpoints; sent again without one";

pub fn resend_notice(step: u32, blocks: u32, chars: u64) -> Vec<String> {
    let mut out = vec![format!("{}.", sentence_case(RESENT))];
    out.extend(discarded(step, blocks, chars));
    out
}

pub fn resend_status() -> String {
    sentence_case(RESENT)
}

pub fn resend_detail(step: u32) -> String {
    format!("re-send of step {step}: {RESENT}")
}

pub const SEARCHING: &str = "Searching";
pub const SEARCHED: &str = "Searched";
pub const SEARCH_FAILED: &str = "Search failed";
pub const NO_RESULT: &str = "No result";
pub const SOURCES: &str = "Sources";

pub fn search_verb(action: SearchAction) -> &'static str {
    match action {
        SearchAction::Search | SearchAction::Other => SEARCHED,
        SearchAction::OpenPage => "Opened",
        SearchAction::FindInPage => "Searched page for",
    }
}

pub fn quoted(query: &str) -> String {
    format!("\"{query}\"")
}

pub fn search_results(n: usize) -> String {
    match n {
        0 => "no results".to_owned(),
        n => plural(u64::try_from(n).unwrap_or(u64::MAX), "result", "results"),
    }
}

pub fn unfinished(reason: UnfinishedReason, possibly_billed: bool) -> &'static str {
    if possibly_billed {
        return "stopped while it may have been running";
    }
    match reason {
        UnfinishedReason::Stopped => "the turn ended first",
        UnfinishedReason::Failed => "the turn failed first",
        UnfinishedReason::Paused => "paused by the provider",
        UnfinishedReason::Refused => "web search is switched off for this account",
        UnfinishedReason::Compacted => "the context window ran out",
    }
}

pub fn cite(n: usize) -> String {
    format!("[{n}]")
}

pub fn inline_cite(n: usize) -> String {
    format!("\\[{n}\\]")
}

fn own_message(reason: &str) -> &str {
    reason
        .strip_prefix("HTTP ")
        .and_then(|rest| rest.split_once(": "))
        .filter(|(status, _)| status.bytes().all(|b| b.is_ascii_digit()))
        .map_or(reason, |(_, message)| message)
        .trim()
}

fn names_the_cause(sentence: &str, cause: &str, own: &str) -> bool {
    own.is_empty()
        || own.eq_ignore_ascii_case(cause)
        || sentence
            .to_ascii_lowercase()
            .contains(&own.to_ascii_lowercase())
}

pub fn gave_up(attempts: u32, reason: GaveUp) -> Option<String> {
    let tries = plural(u64::from(attempts), "attempt", "attempts");
    Some(match reason {
        GaveUp::NotRetryable if attempts > 1 => format!("Stopped after {tries}."),
        GaveUp::Budget => {
            format!("Gave up after {tries}: another attempt would go over the retry budget.")
        }
        _ => return None,
    })
}

pub fn retries(n: u32) -> String {
    plural(u64::from(n), "retry", "retries")
}

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub enum Fact {
    Plain(String),
    Amount(Marked),
}

impl Fact {
    pub fn plain(text: impl Into<String>) -> Fact {
        Fact::Plain(text.into())
    }

    pub fn cost(usd: f64, lower_bound: bool) -> Fact {
        Fact::Amount(marked_amount(&self::usd(usd), false, lower_bound))
    }

    pub fn billed_cost(usd: f64, lower_bound: bool, billing: wisp_protocol::Billing) -> Vec<Fact> {
        if billing == wisp_protocol::Billing::Mixed {
            return mixed_note_parts().into_iter().map(Fact::plain).collect();
        }
        vec![Fact::Amount(marked_amount(
            &self::usd(usd),
            !billing.is_metered(),
            lower_bound,
        ))]
    }

    pub fn joined(facts: &[Fact], sep: &str) -> String {
        facts.iter().map(Fact::text).collect::<Vec<_>>().join(sep)
    }

    pub fn text(&self) -> String {
        match self {
            Fact::Plain(t) => t.clone(),
            Fact::Amount(m) => m.to_string(),
        }
    }
}

pub fn logging_to(path: &str) -> String {
    format!("logging to {path}")
}

pub fn effort_named(effort: &str) -> String {
    format!("{effort} effort")
}

pub fn cache_named(ttl: &str) -> String {
    format!("cache {ttl}")
}

pub const NEXT_MESSAGE: &str = "next message";

pub fn next_turn(arrow: &str, what: &str) -> String {
    format!("{arrow} {what} next turn")
}

pub const DEFAULT: &str = "default";
pub const WITH_THE_SWITCH: &str = "with the model switch";
pub const NO_ADAPTIVE_THINKING: &str = "not supported yet (no adaptive thinking)";
pub fn no_capability_data(priced: bool) -> String {
    let cost = if priced { "" } else { ", cost shows tokens" };
    format!(
        "No capability data for this ID: effort changes rebuild the cache, images are off{cost}."
    )
}
pub const NO_DATA_FOR_MODEL: &str = "No data for this model yet: effort changes rebuild the cache.";
pub const NO_CAPABILITY_DATA_FOR_MODEL: &str =
    "No data for this model yet: thinking, effort and images are off.";
pub const ANY_MODEL_ID: &str = "(type any model ID, then enter)";
pub const FREE_TEXT: &str = "(any model ID, enter to use)";

pub const KEY_SHAPED_COMMAND: &str = "this looks like a key, so it can't be used in a command";

pub const KEY_SHAPED_MODEL: &str = "this looks like a key, not a model ID, so it can't be used";
pub const MODEL_TITLE: &str = "Model";
pub const EFFORT_TITLE: &str = "Effort";
pub const PRICE_TITLE: &str = "$/MTok in";
pub const NEXT_TURN_TITLE: &str = "next turn";
pub const USE_EFFORT: &str = "Use /effort to choose.";
pub const WITH_THE_ENDPOINT_SWITCH: &str = "with the account switch";
pub const UNAVAILABLE: &str = "unavailable";
pub const NO_PRICES: &str = "no prices";
pub const UP_TO_WHOLE_PROMPT: &str = "up to the whole prompt";

pub fn up_to_unpriced() -> String {
    format!("{UP_TO_WHOLE_PROMPT}, {NO_PRICES}")
}
pub const LIST_PRICES: &str = "list prices";
pub const CREDENTIAL_TITLE: &str = "credential";
pub const ADD_WORD: &str = "add";
pub const EDIT_WITH_CLI: &str = "edit or remove with wisp account";

pub const NAME_COLUMN: &str = "name";
pub const LEVEL_COLUMN: &str = "level";
pub const KEY_TYPE: &str = "type";
pub const USE_WORD: &str = "use";
pub const TO_FILTER: &str = "to filter";

pub fn type_to_filter() -> String {
    format!("{KEY_TYPE} {TO_FILTER}")
}

pub const OPEN_NAME: &str = "Open";
pub const FULL_TRANSCRIPT: &str = "Full transcript";
pub const EVERY_CELL: &str = "every cell in full";
pub const NOTHING_MATCHES: &str = "nothing matches";
pub const KEY_CTRL_O: &str = "ctrl-o";
pub const TRANSCRIPT_WORD: &str = "transcript";

pub fn no_level_offered(model: &str, level: &str) -> String {
    format!("{model} doesn't offer {level}")
}

pub fn endpoint_changed(arrow: &str, endpoint: &str) -> String {
    format!("Account {arrow} {endpoint}")
}

pub fn model_also(arrow: &str, model: &str) -> String {
    format!("model {arrow} {}", model_line(model))
}

pub fn effort_also(arrow: &str, effort: &str) -> String {
    format!("effort {arrow} {effort}")
}

pub fn listed(names: &[String]) -> String {
    wisp_protocol::settings::listed(names, "and")
}

pub fn listed_or(names: &[&str]) -> String {
    wisp_protocol::settings::listed(names, "or")
}

pub fn reasoning_may_not_carry(models: &[String]) -> String {
    let shown: Vec<String> = models.iter().map(|m| model_line(m)).collect();
    format!("earlier {} reasoning may not carry over", listed(&shown))
}

pub fn unavailable_because(reason: &str) -> String {
    format!("{UNAVAILABLE}: {reason}")
}

pub fn endpoint_prices(priced: bool, endpoint: &str) -> String {
    let prices = if priced { LIST_PRICES } else { NO_PRICES };
    format!("{prices} on {endpoint}")
}

pub fn no_endpoint(name: Option<&str>, available: &[String]) -> String {
    let available = available.join(", ");
    match name {
        Some(name) => format!("No account \"{name}\". Available: {available}"),
        None => format!("No account by that name. Available: {available}"),
    }
}

pub fn endpoint_unavailable(name: &str, reason: &str) -> String {
    format!("Account \"{name}\" is unavailable: {reason}")
}

pub const CACHE_REBUILD: &str = "cache rebuild";

pub fn cache_rebuild(amount: &str) -> String {
    format!("{CACHE_REBUILD} {amount}")
}

pub fn rebuild_why(reason: &RebuildReason) -> String {
    match reason {
        RebuildReason::ModelSwitch => "model switched".to_owned(),
        RebuildReason::EffortChange => "effort changed".to_owned(),
        RebuildReason::PerMessageEffortRejected => "per-message effort rejected".to_owned(),
        RebuildReason::EndpointSwitch => "account switched".to_owned(),
        RebuildReason::NativeSearchRefused => {
            "web search isn't enabled for this account".to_owned()
        }
        RebuildReason::CacheTtlChange => "cache lifetime changed".to_owned(),
        RebuildReason::RequestOptionChange => "request options changed".to_owned(),
        RebuildReason::Compaction => "context compacted".to_owned(),
        RebuildReason::Rewind => "conversation rewound".to_owned(),
        RebuildReason::ToolsChange => "tools changed".to_owned(),
        RebuildReason::Other(text) => text.replace('_', " "),
    }
}

pub const MAY_REBUILD: &str = "may rebuild the cache";

pub fn cache_rebuilt(reason: &RebuildReason) -> String {
    match reason {
        RebuildReason::CacheTtlChange | RebuildReason::RequestOptionChange => {
            MAY_REBUILD.to_owned()
        }
        _ => format!("cache rebuilt: {}", rebuild_why(reason)),
    }
}

pub const REASONING_DROPPED: &str = "earlier reasoning dropped";
pub const SEARCH_CHANGE_DROPS_REASONING: &str =
    "switching web search on or off drops earlier reasoning";
pub const CHAT_ONLY: &str = "chat only";

pub fn search_line(form: &str) -> String {
    format!("web search: {form}")
}

pub fn search_shift(arrow: &str, from: &str, to: &str) -> String {
    format!("web search: {from} {arrow} {to}")
}

pub fn native_search_refused(endpoint: &str) -> String {
    format!(
        "web search isn't enabled for this account on {endpoint}; continuing without web search"
    )
}

pub const SEARCH_NOT_RUN: &str = "web search didn't run: the server passed the search tool to a model that can't use it (it may have routed this request to another model)";

pub fn gateway_search_refused(endpoint: &str) -> String {
    format!(
        "{endpoint} refused the web search tool; continuing without web search. To stop sending it: wisp config set endpoints.{endpoint}.native_web_search off"
    )
}

pub fn gateway_rejected(model: &str) -> String {
    format!(
        "cache rebuilt: this gateway doesn't pass per-message effort on for {}",
        model_line(model)
    )
}

pub fn model_changed(arrow: &str, model: &str) -> String {
    format!("Model {arrow} {}", model_line(model))
}

pub fn effort_changed(arrow: &str, effort: &str) -> String {
    format!("Effort {arrow} {effort}")
}

pub fn effort_with(effort: &str) -> String {
    format!("effort {effort}")
}

pub fn unchanged(model: &str, effort: Option<&str>, sep: &str) -> String {
    let model = model_line(model);
    match effort {
        Some(effort) => format!("Settings unchanged: {model}{sep}{effort}"),
        None => format!("Settings unchanged: {model}"),
    }
}

pub const APPLIES_NOW: &str = "applies now";
pub const APPLIES_NEXT_CALL: &str = "applies next call";
pub const APPLIES_WHEN_CURRENT: &str = "applies when it is the current model";
pub const LAYER_FLAG: &str = "the command line";
pub const LAYER_ENV: &str = "your environment";
pub const LAYER_FORCED: &str = "a forced value";
pub const LAYER_SESSION: &str = "this session's value";
pub const LAYER_MODE: &str = "the mode";
pub const LAYER_AGENT: &str = "the agent type";
pub const LAYER_ENDPOINT_MODEL: &str = "this account and model";
pub const LAYER_MODEL: &str = "this model";
pub const LAYER_ENDPOINT: &str = "this account";
pub const LAYER_SHIPPED: &str = "Wisp's shipped value";
pub const LAYER_DEFAULT: &str = "the default";

pub fn label_for(label: &str, owner: &str) -> String {
    format!("{label} for {owner}")
}

pub fn label_on(label: &str, endpoint: &str) -> String {
    format!("{label} on {endpoint}")
}

pub fn setting_reset(label: &str) -> String {
    format!("{label} reset")
}

pub fn setting_saved_to(path: &str) -> String {
    format!("saved to {path}")
}

pub fn still_wins_now(by: &str) -> String {
    format!("{by} still wins this session")
}

pub fn still_wins_later(by: &str) -> String {
    format!("{by} still wins in new sessions")
}

pub fn still_wins(by: &str) -> String {
    format!("{by} still wins")
}

pub fn still_wins_other(by: &str, scope: &str) -> String {
    format!("{by} still wins ({scope})")
}

pub fn now_wins_now(by: &str) -> String {
    format!("{by} now wins this session")
}

pub fn now_wins_later(by: &str) -> String {
    format!("{by} wins in new sessions")
}

pub fn was(value: &str) -> String {
    format!("(was {value})")
}

pub fn was_saved(value: &str, saved: &str) -> String {
    format!("(was {value} this session; {saved} saved)")
}

pub const SAVED_VALUE_REMOVED: &str = "saved value removed";
pub const SESSION_VALUE_REMOVED: &str = "session value removed";

pub const COMPACTION_CANCELLED: &str = "the compaction it needed was cancelled";
pub const CONVERTS_BEFORE_SWITCHING: &str = "converts the summary before switching";
pub const CAUSE_CONVERT: &str = "converts the summary";
pub const CAUSE_SMALLER_WINDOW: &str = "the new window is smaller";
pub const CAUSE_KEPT_TURNS: &str = "kept turns can't go to another model";
pub const CAUSE_OTHER: &str = "the new pair needs it";
pub const NOT_QUOTED: &str = "(not quoted when you switched)";

pub fn compacts_then_switches(detail: &str) -> String {
    format!("compacts before your next message, then switches ({detail})")
}

pub fn compacting_before_switch(target: &str, not_quoted: bool) -> String {
    if not_quoted {
        format!("Compacting before switching to {target} {NOT_QUOTED}")
    } else {
        format!("Compacting before switching to {target}")
    }
}

pub fn switch_converted(target: &str) -> String {
    format!("Couldn't compact before switching to {target}; converted the summary instead")
}

pub fn turns_kept_as_messages(first: u64, last: u64, dash: &str) -> String {
    if first == last {
        format!("turn {first} kept only as your messages and the ledger")
    } else {
        format!("turns {first}{dash}{last} kept only as your messages and the ledger")
    }
}

pub const COMPACTION_FAILED: &str = "the compaction it needed failed";
pub const STILL_SAVED_MODEL: &str = "still saved as the model Wisp starts with";
pub const STILL_SAVED_ENDPOINT: &str = "still saved as the account Wisp starts on";

pub fn switch_withdrawn(target: &str, cause: &str) -> String {
    format!("Switch to {target} withdrawn: {cause}")
}

pub fn model_on(model: &str, endpoint: &str) -> String {
    format!("{} on {endpoint}", model_line(model))
}

pub fn choose_again(commands: &[&str]) -> String {
    match commands {
        [one] => format!("choose it again with {one}"),
        many => format!("choose them again with {}", many.join(" and ")),
    }
}

pub fn nameless_withdrawn(failed: bool) -> String {
    let which = if failed { "failed" } else { "cancelled" };
    format!("The switch the {which} compaction was for is withdrawn.")
}

pub fn effort_back(effort: &str, model: &str, asked: &str) -> String {
    format!(
        "effort back to {effort}: {} doesn't take {asked}",
        model_line(model)
    )
}

pub fn settings_refused(reason: &str) -> String {
    format!("Settings not changed: {}.", reason.trim_end_matches('.'))
}

pub fn matching(query: &str) -> String {
    format!("{} (backspace edit, {ESC_KEY} close)", typed_filter(query))
}

pub fn typed_filter(query: &str) -> String {
    format!("matching \"{query}\"")
}

pub fn effort_cycled(arrow: &str, from: &str, to: &str, sep: &str, effect: &str) -> String {
    format!("effort {from} {arrow} {to}{sep}next turn{sep}{effect}")
}

pub fn effort_rebuilds(model: &str, cost: &str) -> String {
    format!(
        "Changing effort on {} rebuilds the cache ({cost}). {USE_EFFORT}",
        model_line(model)
    )
}

pub fn gateway_unproven(cost: &str) -> String {
    format!(
        "This gateway hasn't shown it passes per-message effort on, so a change may rebuild the cache ({cost}). {USE_EFFORT}"
    )
}

pub fn gateway_fell_back(model: &str, cost: &str) -> String {
    format!(
        "This gateway doesn't pass per-message effort on for {}, so a change rebuilds the cache ({cost}). {USE_EFFORT}",
        model_line(model)
    )
}

pub fn no_effort_levels(model: &str) -> String {
    format!("{} takes no effort levels.", model_line(model))
}

pub fn unknown_command(name: &str) -> String {
    format!("/{name} isn't a command. Start with a space to send it.")
}

pub const HELP_LEAD: &str = "Help:";
pub const HELP_TITLE: &str = "commands and keys";
pub const HELP_COMMANDS: &str = "Commands";
pub const HELP_PLUGINS: &str = "Plugins";
pub const KEYS_LEAD: &str = "Keys:";
pub const KEYS_WRITING: &str = "writing";
pub const KEYS_WORKING: &str = "while Wisp works";
pub const KEYS_WAITING: &str = "waiting messages";
pub const KEYS_READING: &str = "reading";
pub const KEYS_TRANSCRIPT: &str = "transcript";
pub const KEYS_ANYWHERE: &str = "anywhere";

pub const BETWEEN_TURNS: &str = "between turns";
pub const RUN_WORD: &str = "run";

pub fn command_queued(name: &str, until: CommandWait) -> String {
    let until = match until {
        CommandWait::Call => "the call ends",
        CommandWait::Hook => "the hook finishes",
        CommandWait::Command => "the command before it ends",
        CommandWait::Unknown => "Wisp is free",
    };
    format!("/{name} queued until {until}")
}

pub fn command_failed(name: &str, message: &str) -> String {
    format!("/{name} failed: {message}")
}

pub fn command_timed_out(name: &str) -> String {
    format!("/{name} timed out")
}

pub fn command_cancelled(name: &str) -> String {
    format!("/{name} cancelled")
}

pub fn command_refused(name: &str, hint: &str) -> String {
    format!("/{name}: {hint}")
}

pub fn no_command_matches(word: &str) -> String {
    format!("no command matches /{word}")
}

pub fn suggest_keys() -> Vec<KeyHint> {
    vec![
        KeyHint::new(ESC_KEY, CLOSE_WORD),
        KeyHint::new(TAB_KEY, "complete"),
        KeyHint::new(ENTER_KEY, RUN_WORD),
    ]
}

pub fn argument_keys() -> Vec<KeyHint> {
    vec![
        KeyHint::new(ESC_KEY, CLOSE_WORD),
        KeyHint::new(ENTER_KEY, RUN_WORD),
    ]
}

pub fn optional_argument(name: &str, separator: &str) -> String {
    format!("{name} (optional) {separator} {ENTER_KEY} {RUN_WORD}")
}

pub fn unknown_effort(level: &str) -> String {
    format!("Use low, medium, high, xhigh or max, not `{level}`.")
}

pub const TOKENS: &str = "tokens";
pub const NOTHING_CACHED_SHORT: &str = "nothing cached yet";
pub const CACHE_EXPIRED_SHORT: &str = "cache expired";

pub fn todo_progress(progress: wisp_protocol::TodoProgress) -> String {
    format!("todo {}/{}", progress.completed, progress.total)
}

pub fn cache_rate(pct: u8) -> String {
    format!("cached {pct}%")
}

pub fn tokens_per_second(approx: &str, rate: f64) -> String {
    let value = if rate > 0.0 && rate < 1.0 {
        "<1".to_owned()
    } else {
        crate::num(rate.round() as u64)
    };
    format!("{approx}{value} tok/s")
}

pub const ALL_CHATS: &str = "All chats";
pub const TODAY: &str = "Today";
pub const YESTERDAY: &str = "Yesterday";
pub const SINCE: &str = "since";

pub fn all_chats_label(part: &str) -> String {
    format!("{ALL_CHATS}, {part}")
}

pub fn account_chats_label(part: &str) -> String {
    format!("{}, {}", sentence_case(part), ALL_CHATS.to_lowercase())
}

pub fn sentence_case(text: &str) -> String {
    let mut chars = text.chars();
    match chars.next() {
        Some(first) => first.to_uppercase().chain(chars).collect(),
        None => String::new(),
    }
}

pub fn finished_duration(ms: u64) -> String {
    let secs = ms.saturating_add(500).div_euclid(1000).max(1);
    match secs {
        0..60 => format!("{secs} s"),
        60..3600 => match secs % 60 {
            0 => format!("{} min", secs / 60),
            s => format!("{} min {s} s", secs / 60),
        },
        _ => match (secs % 3600) / 60 {
            0 => format!("{} h", secs / 3600),
            m => format!("{} h {m} min", secs / 3600),
        },
    }
}

pub fn thought_for(ms: u64) -> String {
    format!("{THOUGHT} for {}", finished_duration(ms))
}

pub fn duration(ms: u64) -> String {
    if ms < 10_000 {
        let tenths = ms.saturating_add(50) / 100;
        return if tenths.is_multiple_of(10) {
            format!("{}s", tenths / 10)
        } else {
            format!("{}.{}s", tenths / 10, tenths % 10)
        };
    }
    let secs = ms.saturating_add(500) / 1000;
    match secs {
        0..60 => format!("{secs}s"),
        60..3600 => format!("{}m {:02}s", secs / 60, secs % 60),
        _ => format!("{}h {:02}m", secs / 3600, (secs % 3600) / 60),
    }
}

pub fn seconds_up(ms: u64) -> u64 {
    ms.div_ceil(1000).max(1)
}

pub fn stop_words(reason: &StopReason) -> Option<String> {
    let words = match reason {
        StopReason::EndTurn => return None,
        StopReason::MaxTokens => "cut off at the output limit".to_owned(),
        StopReason::Refusal => "the model declined".to_owned(),
        StopReason::ToolUse => "stopped for a tool call".to_owned(),
        StopReason::PauseTurn => "paused by the provider".to_owned(),
        StopReason::StopSequence => "stopped at a stop sequence".to_owned(),
        StopReason::Compaction => "stopped to compact the context".to_owned(),
        StopReason::ContextWindowExceeded => "cut off: the context window ran out".to_owned(),
        StopReason::Other(r) => format!("stopped: {r}"),
    };
    Some(words)
}

pub fn error_sentence(provider: &str, kind: ErrorKind) -> String {
    match error_headline(provider, kind, None) {
        (sentence, Some(rest)) => format!("{sentence} {rest}"),
        (sentence, None) => sentence,
    }
}

pub fn error_headline(
    provider: &str,
    kind: ErrorKind,
    detail: Option<&ErrorDetail>,
) -> (String, Option<&'static str>) {
    if kind == ErrorKind::InvalidRequest
        && let Some(detail) = detail
        && let Some(model) = &detail.model
    {
        let model = model_line(model);
        let not_found = detail.source == FailureSource::Endpoint
            && detail.status == Some(404)
            && detail.error_type.is_some();
        return if not_found {
            (
                format!("{provider} could not find the model {model}."),
                Some(MODEL_LISTS),
            )
        } else {
            let rejected = kind.sentence(provider);
            (
                format!("{} for {model}.", rejected.trim_end_matches('.')),
                None,
            )
        };
    }
    let hint = (kind == ErrorKind::ContextOverflow).then_some(COMPACT_TO_FIT);
    (kind.sentence(provider), hint)
}

pub fn window_lowered(model: &str, endpoint: &str, tokens: u64) -> String {
    format!(
        "The server says {model}'s window on {endpoint} is {}; Wisp won't use more than that for the rest of this session. Your saved settings are unchanged.",
        wisp_protocol::context::format_tokens(tokens)
    )
}

pub const REACHED_USAGE_LIMIT: &str = "reached its usage limit";

pub const NOTICE_COLUMNS: usize = 60;

pub fn past_compaction_point(model: &str) -> String {
    let named = format!("Run /compact: past {model}'s compaction point.");
    if named.chars().count() <= NOTICE_COLUMNS {
        named
    } else {
        "Run /compact: past the new model's compaction point.".to_owned()
    }
}

fn tenths(tokens: u64) -> String {
    if !(1_000..99_950).contains(&tokens) {
        return format_tokens(tokens);
    }
    match ((tokens + 50) / 1_000, (tokens + 50) / 100 % 10) {
        (whole, 0) => format!("{whole}k"),
        (whole, tenth) => format!("{whole}.{tenth}k"),
    }
}

pub fn auto_compaction_held(tokens: u64, point: u64) -> String {
    format!(
        "Automatic compaction is off: after compacting, the conversation is still {}, over the {} point. It turns back on when the point, the model, the system prompt or the tools change; /compact and a plugin's compaction requests still work.",
        tenths(tokens),
        tenths(point)
    )
}

pub const MODEL_LISTS: &str = "/model lists what this account offers.";

pub const COMPACT_TO_FIT: &str = "/compact summarises it so your next message fits.";

pub fn phase_words(phase: FailurePhase) -> &'static str {
    match phase {
        FailurePhase::Connect => "before sending",
        FailurePhase::Response => "before the reply",
        FailurePhase::Stream => "mid-stream",
    }
}

pub fn limit_reached_at(
    window: &wisp_protocol::plan_usage::UsageWindow,
    endpoint: Option<&str>,
    guard: bool,
    time: impl Fn(i64) -> String,
) -> String {
    if guard && let Some(at) = window.resets_at {
        let headline = endpoint.map_or_else(
            || "Request rate limited".into(),
            |name| format!("{} is rate limited", crate::chrome::one_line(name)),
        );
        return format!("{headline}{SEP}retry after {}", time(at));
    }
    let reset = crate::plan_usage::reset(window, time);
    match endpoint {
        Some(endpoint) => format!(
            "{} {}{SEP}{reset}",
            crate::chrome::one_line(endpoint),
            REACHED_USAGE_LIMIT
        ),
        None => format!("Usage limit reached{SEP}{reset}"),
    }
}

pub(super) fn model_limit_line(
    window: &wisp_protocol::plan_usage::UsageWindow,
    time: impl Fn(i64) -> String,
) -> String {
    let used = crate::plan_usage::mini::used(window);
    let reset = crate::plan_usage::reset(window, time);
    format!(
        "{} {used}{SEP}{reset}",
        crate::chrome::one_line(&window.label)
    )
}

pub fn error_detail(detail: &ErrorDetail, sep: &str) -> String {
    error_detail_parts(detail).join(sep)
}

pub fn error_detail_parts(detail: &ErrorDetail) -> Vec<String> {
    let mut parts: Vec<String> = Vec::new();
    if let Some(status) = detail.status {
        parts.push(format!("HTTP {status}"));
    }
    if let Some(error_type) = &detail.error_type {
        parts.push(error_type.clone());
    }
    if let Some(id) = &detail.request_id {
        parts.push(format!("request {id}"));
    }
    parts.push(phase_words(detail.phase).to_owned());
    parts
}

pub fn history_adapted(endpoint: Option<&str>, change: &wisp_protocol::HistoryChange) -> String {
    use wisp_protocol::HistoryChange;
    match change {
        HistoryChange::SearchesAsText { thinking_cut } => {
            let on = endpoint.unwrap_or("the server");
            let line = format!(
                "Earlier web search results can't be read on {on}; they're sent as titles and links from now on (the cache is rebuilt once)."
            );
            if *thinking_cut {
                format!("{line} The thinking after them is left out.")
            } else {
                line
            }
        }
        HistoryChange::ThinkingRefused => {
            let who = endpoint.unwrap_or("The server");
            format!(
                "{who} refused earlier thinking; it's left out from now on (the cache is rebuilt once)."
            )
        }
        HistoryChange::ThinkingDropped { count, reason } => {
            let blocks = if *count == 1 {
                "earlier thinking block"
            } else {
                "earlier thinking blocks"
            };
            let why = match reason.as_deref() {
                Some("model_binding_mismatch") => "came from another model",
                Some("organization_binding_mismatch") => "came from another account",
                _ if *count == 1 => "no longer matches the conversation",
                _ => "no longer match the conversation",
            };
            format!(
                "The API left out {count} {blocks} that {why}; the model no longer sees that reasoning."
            )
        }
        HistoryChange::AttachmentsAsNotes {
            model,
            images,
            files,
        } => {
            let count =
                |n: usize, one, many| plural(u64::try_from(n).unwrap_or(u64::MAX), one, many);
            let listed: Vec<String> = [(*images, "image", "images"), (*files, "file", "files")]
                .into_iter()
                .filter(|(n, ..)| *n > 0)
                .map(|(n, one, many)| count(n, one, many))
                .collect();
            let notes = if images + files == 1 {
                "a note"
            } else {
                "notes"
            };
            format!(
                "{} can't take attachments, so it gets {notes} in place of {}.",
                crate::chrome::model_name(&crate::chrome::one_line(model)),
                listed.join(" and ")
            )
        }
    }
}

pub const INJECTED_THINKING: &str = "The server re-inserted earlier reasoning Wisp didn't send (a gateway). Each change can cost a cache rebuild.";

pub const REMOTE_LOSS: &str = "provider- or gateway-side cache loss, nothing changed locally";

pub const TRANSFORMATIONS_UNCHANGED: &str = "input transformations: same as the previous request";

pub fn miss_cause(cause: &wisp_protocol::MissCause) -> String {
    use wisp_protocol::MissCause;
    match cause {
        MissCause::MessageChanged { index } => format!("message_changed at messages.{index}"),
        MissCause::InjectedThinking { first_path, count }
        | MissCause::ProviderDroppedThinking { first_path, count } => {
            format!("{} ({count}, first at {first_path})", cause.as_str())
        }
        MissCause::TtlExpired { gap_s } => format!("ttl_expired after {gap_s}s"),
        MissCause::EndpointChanged => "account_changed".to_owned(),
        MissCause::Expected { reason } if reason == "endpoint_switch" => {
            "expected (account_switch)".to_owned()
        }
        MissCause::Expected { reason } => format!("expected ({reason})"),
        MissCause::RemoteLoss => REMOTE_LOSS.to_owned(),
        other => other.as_str().to_owned(),
    }
}

pub fn miss_repeated(
    cause: &wisp_protocol::MissCause,
    count: u32,
    usd: Option<f64>,
    billing: wisp_protocol::Billing,
) -> String {
    let cost = usd.map_or_else(String::new, |usd| {
        format!(
            ", {}",
            Fact::joined(&Fact::billed_cost(usd, false, billing), SEP)
        )
    });
    format!(
        "The cache was rebuilt {count} times in a row (cause: {}){cost}.",
        miss_cause(cause)
    )
}

pub fn fallback(provider: &str, to: &str) -> String {
    format!("{provider} declined, so this answer came from {to}.")
}

pub fn not_rewound(reason: &str) -> String {
    let reason = reason.trim_end().trim_end_matches('.');
    format!("Not rewound: {reason}.")
}

pub fn rejected(reason: &str) -> String {
    let reason = reason.trim_end().trim_end_matches('.');
    format!("This message wasn't sent: {reason}.")
}

pub const SENT_WHILE_WORKING: &str = "sent while working";
pub const QUEUED_TAG: &str = "queued";
pub const NOT_KEPT_TAG: &str = "not kept";

pub fn rewritten_by(hook: &str) -> String {
    format!("rewritten by {hook}")
}

pub const REMOVED_FIELD: &str = "removed";

pub fn hook_run(hook: &str, point: &str, words: &str) -> String {
    format!("hook {hook} at {point} {words}")
}

pub fn hook_ms(ms: u64) -> String {
    format!("{ms} ms")
}

pub fn changed_fields(names: &[String]) -> String {
    format!("changed {}", names.join(", "))
}

pub fn blocked_by(hook: &str, reason: &str) -> String {
    format!("blocked by {hook}: {reason}")
}

pub fn continued_by(hook: &str) -> String {
    format!("continued by {hook}")
}
pub const NEXT_STEP: &str = "next step";
pub const NEXT_TURN: &str = "next turn";
pub const HELD: &str = "held";
pub const NEXT_STEP_KIND: &str = "next-step";
pub const NEXT_TURN_KIND: &str = "next-turn";
pub const NOTHING_TO_STEER_SHORT: &str = "nothing is running to steer";
pub const IMAGES_ELSEWHERE: &str = "Can't change this message: its images aren't held here.";
pub const ALREADY_READ: &str = "Wisp already read that message, so it stays sent.";
pub const ALREADY_SENT: &str = "Already sent";
pub const EDIT_AHEAD: &str = "your edit is back, ahead of your draft.";
pub const EDIT_BACK: &str = "your edit is back in the composer.";

pub const EDIT_WAITING: &str = "edit waiting";

pub fn tab_queues() -> KeyHint {
    KeyHint::new(TAB_KEY, format!("{QUEUE_WORD} for the next turn"))
}

pub fn send_held() -> KeyHint {
    KeyHint::new(ENTER_KEY, format!("{KEY_SEND} {HELD}"))
}

pub fn edit_waiting(up: &str) -> Vec<KeyHint> {
    vec![KeyHint::new(up, EDIT_WAITING), esc_to_stop()]
}

pub fn sends_held(up: &str) -> Vec<KeyHint> {
    vec![send_held(), KeyHint::new(up, EDIT_WAITING)]
}

pub fn editing(kind: &str) -> Vec<KeyHint> {
    vec![
        KeyHint::note(format!("editing a {kind} message")),
        KeyHint::new(ESC_KEY, CANCEL_WORD),
        KeyHint::new(ENTER_KEY, SAVE_WORD),
    ]
}

pub fn row_keys() -> Vec<KeyHint> {
    vec![
        KeyHint::new(ENTER_KEY, "edit"),
        KeyHint::new("del", "remove"),
        KeyHint::new(TAB_KEY, "switch"),
    ]
}

pub fn too_late_edit(ahead_of_draft: bool) -> String {
    let edit = if ahead_of_draft {
        EDIT_AHEAD
    } else {
        EDIT_BACK
    };
    format!("{ALREADY_SENT}: {edit}")
}

pub fn dock_count(at: usize, of: usize) -> String {
    format!("{at} of {of}")
}

pub fn waiting_count(n: usize) -> String {
    format!("{n} waiting")
}

pub fn more_waiting(n: usize) -> String {
    format!("+ {n} more")
}

fn messages(n: usize, one: &str, many: &str) -> String {
    if n == 1 {
        format!("1 {one}")
    } else {
        format!("{n} {many}")
    }
}

pub fn inputs_after(failed: bool, held: usize, returned: usize) -> Option<String> {
    let mut parts: Vec<String> = vec![];
    if held > 0 {
        let count = messages(held, "queued message is", "queued messages are");
        let them = if held == 1 { "it" } else { "them" };
        parts.push(if failed {
            format!("The turn failed. {count} held; {ENTER_KEY} sends {them} when you're ready.")
        } else {
            format!("Stopped. {count} held; {ENTER_KEY} sends {them}.")
        });
    }
    if returned == 1 {
        parts.push("Your unsent steer is back in the composer.".to_owned());
    } else if returned > 1 {
        parts.push(format!(
            "Your {returned} unsent steers are back in the composer."
        ));
    }
    (!parts.is_empty()).then(|| parts.join(" "))
}

pub fn returned_after_resume(returned: usize, missing: usize) -> Option<String> {
    let mut parts: Vec<String> = vec![];
    match returned {
        0 => {}
        1 => {
            parts.push("Your message from before Wisp stopped is back in the composer.".to_owned())
        }
        n => parts.push(format!(
            "Your {n} messages from before Wisp stopped are back in the composer."
        )),
    }
    match missing {
        0 => {}
        1 => parts.push("1 of their images couldn't be read from the log.".to_owned()),
        n => parts.push(format!(
            "{n} of their images couldn't be read from the log."
        )),
    }
    (!parts.is_empty()).then(|| parts.join(" "))
}

pub fn unknown_event(provider: &str, logging: bool) -> String {
    let lead = format!("{provider} sent an event this version doesn't know.");
    if logging {
        format!("{lead} It's kept in the log.")
    } else {
        lead
    }
}

pub fn log_disabled(reason: &str) -> String {
    format!("Session logging stopped: {reason}. The chat carries on.")
}

pub fn attachment_withdrawn(noun: &str, label: &str, reason: &str) -> String {
    format!("{noun} {label} couldn't be processed: {reason}")
}

pub fn tool_attachment_withdrawn(noun: &str, label: &str, call_id: &str, reason: &str) -> String {
    format!("{noun} {label} from call {call_id} couldn't be processed: {reason}")
}

pub fn resent_without(step: u32) -> String {
    format!("re-send of step {step} without the attachments")
}

pub const COULD_NOT_ATTACH: &str = "couldn't attach";

pub fn withdrawn_with(others: usize) -> String {
    let noun = if others == 1 {
        "attachment"
    } else {
        "attachments"
    };
    format!("withdrawn with {others} other {noun}")
}

pub fn attachment_not_kept(reason: &str, others: usize) -> String {
    if others == 0 {
        format!("Couldn't attach: {reason}")
    } else {
        format!(
            "{} when the API refused the step: {reason}",
            sentence_case(&withdrawn_with(others))
        )
    }
}

pub fn saved_copy(path: &str) -> String {
    format!("Saved copy: {path}")
}

pub const NOT_RUN: &str = "Not run";
pub const STOPPED_FACT: &str = "stopped";
pub const COULD_NOT_START: &str = "couldn't start";
pub const FAILED_FACT: &str = "failed";
pub const TIMED_OUT_FACT: &str = "timed out";
pub const INTERRUPTED_FACT: &str = "interrupted";
pub const STOPPED_WHILE_IT_RAN: &str = "Wisp stopped while it ran";
pub fn woke(ended: &[(u32, ProcessEnd)]) -> String {
    match ended {
        [] => "background processes ended".to_owned(),
        [(id, end)] => match end {
            ProcessEnd::Failed { .. } => format!("process {id} failed"),
            ProcessEnd::Stopped { .. } => format!("process {id} reached its time limit"),
            ProcessEnd::Exited { .. } | ProcessEnd::Lost { .. } => format!("process {id} exited"),
        },
        many => {
            let ids: Vec<String> = many.iter().map(|(id, _)| id.to_string()).collect();
            format!("processes {} ended", listed(&ids))
        }
    }
}

pub const PROCESSES_TITLE: &str = "Processes";
pub const PS_RUNNING: &str = "Running";
pub const PS_HISTORY: &str = "History";
pub const PS_HINT: &str = "/ps";
pub fn background_key() -> KeyHint {
    KeyHint::new("ctrl-b", "background")
}
pub const SCREEN_ENDED: &str = "ended";

pub const ATTACH_WARNING: &str =
    "The agent that started it can read the program's output. Password prompts hide what you type.";

pub fn attach_note() -> String {
    format!("{ATTACH_WARNING} If {F12_KEY} does nothing here, {BRACKET_KEY} goes back too.")
}

pub const LINE_MODE_PASTE: &str = "a terminal in line mode keeps only 4,095 bytes per line";
pub const PASTE_TOO_LARGE: &str = "a paste over 1 MiB isn't sent to the program";
pub const PS_WAITING: &str = "waiting for a password";
pub const WAITING_TO_TAKE_OVER: &str = "Waiting for you to take over";
pub const YOU_TYPED_INTO: &str = "You typed into";
pub const YOU_LOOKED_AT: &str = "You looked at";
pub const WITHOUT_TYPING: &str = "without typing";
pub const DECLINED_TAKE_OVER: &str = "Declined to take over";
pub const ANSWER_ENDED_FIRST: &str = "ended first";

pub fn screen_keys(attachable: bool) -> Vec<KeyHint> {
    let mut keys = Vec::new();
    if attachable {
        keys.push(a_take_over());
    }
    keys.push(esc(CLOSE_WORD));
    keys
}

pub const F12_KEY: &str = "f12";
pub const BRACKET_KEY: &str = "ctrl-]";
pub const TAKE_OVER_WORD: &str = "take over";

pub fn go_back_hint() -> KeyHint {
    KeyHint::new(F12_KEY, format!("to go {BACK_WORD}")).as_exit()
}

pub fn take_over_hint() -> KeyHint {
    KeyHint::new(F12_KEY, format!("to {TAKE_OVER_WORD}"))
}

pub fn a_take_over() -> KeyHint {
    KeyHint::new("a", TAKE_OVER_WORD)
}

pub fn step_in_hint() -> String {
    format!("1 {TAKE_OVER_WORD}, then type")
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ProcessName {
    id: u32,
    agent: Option<String>,
}

impl ProcessName {
    pub fn of(id: u32, agent: &AgentPath, here: &AgentPath) -> Self {
        let agent = (agent != here).then(|| shown_agent(agent).to_owned());
        Self { id, agent }
    }
}

impl fmt::Display for ProcessName {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match &self.agent {
            Some(agent) => write!(f, "{} of {agent}", self.id),
            None => write!(f, "{}", self.id),
        }
    }
}

pub fn typing_into(id: impl fmt::Display) -> String {
    format!("typing into {id}")
}

pub fn waiting_for_password(id: impl fmt::Display) -> String {
    format!("{id} is waiting for a password")
}

pub fn ps_waiting(sep: &str) -> String {
    format!("{PS_WAITING} {sep} {}", a_take_over().text())
}

pub fn nothing_waiting(sep: &str) -> String {
    format!("nothing is waiting for you {sep} /ps to choose a process")
}

pub fn no_terminal_code(key: &str) -> String {
    format!("{key} has no terminal code; not sent")
}

pub fn typed_into(id: u32) -> String {
    format!("You typed into process {id}")
}

pub fn typed_into_about(id: u32, about: &str, sep: &str) -> String {
    format!("{}{sep}{about}", typed_into(id))
}

pub fn process_ended_first(id: u32) -> String {
    format!("{} {ANSWER_ENDED_FIRST}", screen_title(id))
}

pub fn step_in_title(id: u32) -> String {
    format!("Take over process {id}?")
}

pub fn ps_keys_with(
    set: GlyphSet,
    open: bool,
    stop: bool,
    several: bool,
    attach: bool,
) -> Vec<KeyHint> {
    let mut keys = Vec::new();
    if open {
        keys.push(KeyHint::new(ENTER_KEY, OPEN_WORD));
    }
    if attach {
        keys.push(a_take_over());
    }
    if stop {
        keys.push(KeyHint::new("x", STOP_WORD));
    }
    if several {
        keys.push(arrows(set));
    }
    keys.push(esc(CLOSE_WORD));
    keys
}

pub fn press_x_again(id: impl fmt::Display) -> String {
    format!("press x again to stop process {id}")
}

pub fn running_count(n: usize) -> String {
    format!("{n} running")
}

pub const AGENT_CONTINUES_WHEN: &str = "agent continues when";

pub fn ready_with_processes(ids: &[u32], separator: &str) -> String {
    let ready = format!("{READY} {separator} {AGENT_CONTINUES_WHEN}");
    match ids {
        [] => READY.to_owned(),
        [id] => format!("{ready} process {id} ends"),
        ids => format!("{ready} 1 of {} processes ends", ids.len()),
    }
}

pub fn screen_title(id: impl fmt::Display) -> String {
    format!("Process {id}")
}

pub fn stopping_processes(n: usize) -> String {
    if n == 1 {
        "stopping 1 process".to_owned()
    } else {
        format!("stopping {n} processes")
    }
}

pub fn process_state(end: Option<&ProcessEnd>, terminal: Option<TermSize>) -> String {
    match (end, terminal) {
        (None, Some(size)) => format!("terminal {}x{}", size.cols, size.rows),
        (None, None) => "running".to_owned(),
        (Some(end), _) => end_words(end),
    }
}

fn end_words(end: &ProcessEnd) -> String {
    match end {
        ProcessEnd::Exited {
            exit: ExitStatus::Code { code },
        } => format!("exit {code}"),
        ProcessEnd::Exited {
            exit: ExitStatus::Signal { signal: n },
        } => signal(*n),
        ProcessEnd::Stopped {
            by: StoppedBy::User,
        } => "stopped by you".to_owned(),
        ProcessEnd::Stopped {
            by: StoppedBy::TimeLimit { .. },
        } => "time limit".to_owned(),
        ProcessEnd::Stopped { .. } => "stopped".to_owned(),
        ProcessEnd::Lost { .. } => "lost".to_owned(),
        ProcessEnd::Failed { .. } => "failed".to_owned(),
    }
}

pub fn process_ended(id: impl fmt::Display, end: &ProcessEnd) -> String {
    match end {
        ProcessEnd::Exited {
            exit: ExitStatus::Code { code },
        } => format!("process {id} exited ({code})"),
        ProcessEnd::Exited {
            exit: ExitStatus::Signal { signal },
        } => format!("process {id} ended by signal {signal}"),
        ProcessEnd::Stopped {
            by: StoppedBy::TimeLimit { .. },
        } => format!("process {id} reached its time limit"),
        ProcessEnd::Stopped { .. } => format!("process {id} stopped"),
        ProcessEnd::Lost { .. } => format!("process {id} was lost"),
        ProcessEnd::Failed { .. } => format!("process {id} failed"),
    }
}

pub const TURN_STOPPED: &str = "the turn was stopped";
pub const BEFORE_RESTART: &str = "before the restart";
pub const AFTER_QUESTION: &str = "came after a question";
pub const PROMPT: &str = "$ ";
pub const TAIL_ONLY: &str = "Only the last part of the output is shown here.";
pub const LOADING_FILE: &str = "Loading the output file";

pub const WRITING: &str = "Writing";

pub fn writing(phrase: &str) -> String {
    format!("{WRITING} {phrase}")
}

pub fn writing_call(name: &str) -> String {
    format!("a {name} call")
}

pub const QUEUED_FACT: &str = "queued";
pub const NOT_RUN_FACT: &str = "not run";
pub const NOT_SENT_FACT: &str = "not sent";
pub const RUNNING_FACT: &str = "running";
pub const PS_STOPS_IT: &str = "/ps stops it";
pub const KEEPS_RUNNING_FACT: &str = "keeps running after Wisp exits";
pub const MAY_STILL_RUN_FACT: &str = "may still be running";
pub const KEPT_STATE: &str = "running, keeps running after Wisp exits";
pub const KEPT_RESTORED_STATE: &str =
    "running since before the restart, keeps running after Wisp exits";

pub fn may_still_run(pid: Option<u32>) -> String {
    match pid {
        Some(pid) => format!("{MAY_STILL_RUN_FACT} as pid {pid}"),
        None => MAY_STILL_RUN_FACT.to_owned(),
    }
}

pub const STOP_IT_LABEL: &str = "stop it";
pub const CHECK_IT_LABEL: &str = "check it";
pub const OUTPUT_LABEL: &str = "output";

pub fn kept_heading(id: u32, kept: super::Keeps) -> String {
    let state = match kept {
        super::Keeps::Unchecked => MAY_STILL_RUN_FACT,
        super::Keeps::Running | super::Keeps::No => "keeps running",
    };
    format!("{} {state}: ", screen_title(id))
}

pub fn stop_group(pid: u32) -> String {
    format!("kill -- -{pid}")
}

pub fn check_pid(pid: u32) -> String {
    format!("ps -p {pid}")
}
pub const WAITING_FACT: &str = "waiting in terminal";
pub const PARTLY_SENT: &str = "partly sent";
pub const BACKGROUND_PROCESSES: &str = "background processes";

pub fn process_name(id: u32) -> String {
    format!("process {id}")
}

pub fn signal_not_sent(kind: SignalKind) -> &'static str {
    match kind {
        SignalKind::Interrupt => "interrupt not sent",
        SignalKind::Kill => "stop not sent",
    }
}

pub fn then_signalled(kind: SignalKind) -> &'static str {
    match kind {
        SignalKind::Interrupt => ", then interrupted it",
        SignalKind::Kill => ", then stopped it",
    }
}

pub fn then_signalled_act(kind: SignalKind) -> &'static str {
    match kind {
        SignalKind::Interrupt => ", then interrupted",
        SignalKind::Kill => ", then stopped",
    }
}

fn owner(name: Option<&str>) -> String {
    match name {
        Some(name) => format!("{name}'s"),
        None => "the".to_owned(),
    }
}

pub fn output_of(name: Option<&str>) -> String {
    format!("{} output", owner(name))
}

pub fn screen_of(name: Option<&str>) -> String {
    format!("{} screen", owner(name))
}

pub fn searched_for(name: Option<&str>, pattern: &str) -> String {
    format!("{} output for \"{pattern}\"", owner(name))
}

pub fn typed_into_name(name: &str) -> String {
    format!("into {name}")
}

pub fn pressed_in(keys: &str, name: &str) -> String {
    format!("{keys} in {name}")
}

pub fn and_pressed(keys: &str, running: bool) -> String {
    format!("and {} {keys}", process_act(ProcessVerb::Pressed, running))
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ProcessVerb {
    Typed,
    Pressed,
    Interrupted,
    Stopped,
    WaitedFor,
    Read,
    Searched,
    AskedToTakeOver,
    Listed,
}

pub fn process_verb(verb: ProcessVerb, running: bool) -> &'static str {
    let (now, done) = match verb {
        ProcessVerb::Typed => ("Typing", "Typed"),
        ProcessVerb::Pressed => ("Pressing", "Pressed"),
        ProcessVerb::Interrupted => ("Interrupting", "Interrupted"),
        ProcessVerb::Stopped => ("Stopping", "Stopped"),
        ProcessVerb::WaitedFor => ("Waiting for", "Waited for"),
        ProcessVerb::Read => ("Reading", "Read"),
        ProcessVerb::Searched => ("Searching", "Searched"),
        ProcessVerb::AskedToTakeOver => ("Asking you to take over", "Asked you to take over"),
        ProcessVerb::Listed => ("Listing", "Listed"),
    };
    if running { now } else { done }
}

pub fn process_act(verb: ProcessVerb, running: bool) -> &'static str {
    let (now, done) = match verb {
        ProcessVerb::Typed => ("typing", "typed"),
        ProcessVerb::Pressed => ("pressing", "pressed"),
        ProcessVerb::Interrupted => ("interrupting", "interrupted"),
        ProcessVerb::Stopped => ("stopping", "stopped"),
        ProcessVerb::WaitedFor => ("waiting", "waited"),
        ProcessVerb::Read => ("reading", "read"),
        ProcessVerb::Searched => ("searching", "searched"),
        ProcessVerb::AskedToTakeOver => ("asking you to take over", "asked you to take over"),
        ProcessVerb::Listed => ("listing", "listed"),
    };
    if running { now } else { done }
}

pub fn process_end_words(end: &ProcessEnd) -> String {
    end_words(end)
}

pub fn could_not(action: Option<&ToolAction>) -> &'static str {
    match action {
        Some(ToolAction::Run) => "couldn't run",
        Some(ToolAction::Read) => "couldn't read",
        Some(ToolAction::Write) => "couldn't write",
        Some(ToolAction::Edit) => "couldn't edit",
        Some(ToolAction::Search) => "couldn't search",
        Some(ToolAction::Fetch) => "couldn't fetch",
        Some(ToolAction::Other { .. } | ToolAction::Unknown) | None => "couldn't use",
        Some(ToolAction::Input) => "couldn't type",
        Some(ToolAction::Wait) => "couldn't wait",
        Some(ToolAction::Signal {
            kind: SignalKind::Interrupt,
        }) => "couldn't interrupt",
        Some(ToolAction::Signal {
            kind: SignalKind::Kill,
        }) => "couldn't stop",
        Some(ToolAction::Attach) => "couldn't ask you to take over",
        Some(ToolAction::Show) => "couldn't show",
    }
}

pub fn tool_verb(action: Option<&ToolAction>, running: bool) -> &'static str {
    let (now, done) = match action {
        Some(ToolAction::Run) => ("Running", "Ran"),
        Some(ToolAction::Read) => ("Reading", "Read"),
        Some(ToolAction::Write) => ("Writing", "Wrote"),
        Some(ToolAction::Edit) => ("Editing", "Edited"),
        Some(ToolAction::Search) => ("Searching", "Searched"),
        Some(ToolAction::Fetch) => ("Fetching", "Fetched"),
        Some(ToolAction::Other { .. } | ToolAction::Unknown) | None => ("Using", "Used"),
        Some(ToolAction::Input) => ("Typing", "Typed"),
        Some(ToolAction::Wait) => ("Waiting", "Waited"),
        Some(ToolAction::Signal {
            kind: SignalKind::Interrupt,
        }) => ("Interrupting", "Interrupted"),
        Some(ToolAction::Signal {
            kind: SignalKind::Kill,
        }) => ("Stopping", "Stopped"),
        Some(ToolAction::Attach) => ("Asking you to take over", "Asked you to take over"),
        Some(ToolAction::Show) => ("Showing", "Showed"),
    };
    if running { now } else { done }
}

pub fn more_lines(n: usize) -> String {
    let noun = if n == 1 { "line" } else { "lines" };
    format!("(+{n} {noun})")
}

pub fn grouped(n: usize) -> String {
    thousands(u64::try_from(n).unwrap_or(u64::MAX))
}

pub fn pasted_text(n: usize, lines: usize, chars: usize) -> String {
    if lines > 1 {
        format!("[Pasted text #{n} +{} lines]", grouped(lines))
    } else {
        format!("[Pasted text #{n} +{} characters]", grouped(chars))
    }
}

fn plural(n: u64, one: &str, many: &str) -> String {
    if n == 1 {
        format!("1 {one}")
    } else {
        format!("{} {many}", crate::num(n))
    }
}

pub fn size(bytes: u64) -> String {
    const KB: u64 = 1000;
    match bytes {
        0..KB => plural(bytes, "byte", "bytes"),
        KB..1_000_000 => format!("{} KB", bytes / KB),
        1_000_000..999_950_000 => {
            let mb = format!("{:.1}", bytes as f64 / 1_000_000.0);
            format!("{} MB", mb.strip_suffix(".0").unwrap_or(&mb))
        }
        _ => {
            let gb = format!("{:.1}", bytes as f64 / 1_000_000_000.0);
            format!("{} GB", gb.strip_suffix(".0").unwrap_or(&gb))
        }
    }
}

pub fn fact_kind(fact: &ToolFact) -> Option<&'static str> {
    match fact {
        ToolFact::Part { .. } => Some("text"),
        _ => None,
    }
}

pub fn fact(fact: &ToolFact, g: &Glyphs) -> String {
    match fact {
        ToolFact::Exit { code } => format!("exit {code}"),
        ToolFact::Lines { n } => plural(*n, "line", "lines"),
        ToolFact::Bytes { n } => size(*n),
        ToolFact::Matches { n, files } => format!(
            "{} in {}",
            plural(*n, "match", "matches"),
            plural(*files, "file", "files")
        ),
        ToolFact::Diffstat { added, removed } => format!("+{added} {}{removed}", g.minus),
        ToolFact::Process { id } => format!("process {id}"),
        ToolFact::Media { media_type } => media_type.to_string(),
        ToolFact::Part {
            sent,
            total,
            more_than,
        } => {
            if *more_than {
                let noun = if *sent == 1 { "line" } else { "lines" };
                return format!("first {} {noun}; the file has more", thousands(*sent));
            }
            let noun = if *total == 1 { "line" } else { "lines" };
            if sent == total {
                format!("{} {noun}", thousands(*total))
            } else {
                format!("{} of {} {noun}", thousands(*sent), thousands(*total))
            }
        }
        ToolFact::Tokens { n } => format!(
            "{}{} tokens",
            approx(g.set),
            wisp_protocol::context::format_tokens(*n)
        ),
        ToolFact::Hits {
            tools,
            skills,
            documents,
        } => {
            let parts: Vec<_> = [
                (*tools, "tool", "tools"),
                (*skills, "skill", "skills"),
                (*documents, "document", "documents"),
            ]
            .into_iter()
            .filter(|(n, _, _)| *n > 0)
            .map(|(n, one, many)| plural(n, one, many))
            .collect();
            if parts.is_empty() {
                "nothing found".to_owned()
            } else {
                parts.join(&format!(" {} ", g.separator))
            }
        }
        ToolFact::Unknown => String::new(),
    }
}

pub fn signal(n: i32) -> String {
    format!("signal {n}")
}

pub fn timed_out_after(clock: &str) -> String {
    format!("timed out after {clock}")
}

pub fn not_run(reason: &NotRunReason) -> String {
    match reason {
        NotRunReason::ReplyStopped { stop_reason } => {
            format!("the reply stopped ({})", stop_reason.as_str())
        }
        NotRunReason::TurnStopped => TURN_STOPPED.to_owned(),
        NotRunReason::AfterQuestion => AFTER_QUESTION.to_owned(),
    }
}

pub fn ran_in(cwd: &str) -> String {
    format!("in {cwd}")
}

pub fn saved_to(path: &str) -> String {
    format!("Output saved to {path}")
}

pub fn file_gone(reason: &str) -> String {
    format!("The output file couldn't be read ({reason}), so only its last part is shown.")
}

pub const DONE_WORD: &str = "done";
pub const ERROR_WORD: &str = "finished with an error";

pub fn outcome_words(outcome: &ToolOutcome, took_ms: u64, process: Option<u32>) -> String {
    match outcome {
        ToolOutcome::Completed { exit, is_error } => match exit {
            Some(wisp_protocol::ExitStatus::Code { code }) => format!("exit {code}"),
            Some(wisp_protocol::ExitStatus::Signal { signal: n }) => signal(*n),
            None if *is_error => ERROR_WORD.to_owned(),
            None => DONE_WORD.to_owned(),
        },
        ToolOutcome::TimedOut { after_ms } => timed_out_after(&clock(after_ms / 1000)),
        ToolOutcome::Cancelled => STOPPED_FACT.to_owned(),
        ToolOutcome::NotRun { reason } => format!("{NOT_RUN_FACT}: {}", not_run(reason)),
        ToolOutcome::InvalidInput { .. }
        | ToolOutcome::Failed {
            before_start: true, ..
        } => COULD_NOT_START.to_owned(),
        ToolOutcome::Failed { .. } => FAILED_FACT.to_owned(),
        ToolOutcome::Backgrounded { reason } => {
            let place = match reason {
                BackgroundReason::Started => "started in the background".to_owned(),
                BackgroundReason::MovedByUser
                | BackgroundReason::Quiet
                | BackgroundReason::StillRunning => {
                    format!("moved to the background after {}", clock(took_ms / 1000))
                }
            };
            match process {
                Some(id) => format!("{place} as {}", process_name(id)),
                None => place,
            }
        }
    }
}

pub fn with_reason(words: &str, reason: &str) -> String {
    format!("{words}: {reason}")
}

pub fn earlier_output_in(more: &str, path: &str) -> String {
    format!("{more} earlier output is in {path}")
}

pub fn kept_steps(n: u32) -> String {
    if n == 1 {
        "1 step was kept: the commands that ran and their output.".to_owned()
    } else {
        format!("{n} steps were kept: the commands that ran and their output.")
    }
}

pub fn stopped(kept: u32) -> String {
    format!("{STOPPED} {}", kept_or_nothing(kept))
}

pub fn kept_or_nothing(kept: u32) -> String {
    if kept == 0 {
        NOTHING_KEPT.to_owned()
    } else {
        kept_steps(kept)
    }
}

pub fn requests(n: u32) -> String {
    plural(u64::from(n), "request", "requests")
}

pub fn tools(n: u32) -> String {
    plural(u64::from(n), "tool", "tools")
}

pub fn block(name: &str) -> String {
    format!("A {name} block this version doesn't show.")
}

pub fn redacted(provider: &str) -> String {
    format!("redacted by {provider}")
}

pub const FILE_WITHOUT_PATH: &str = "file";

pub fn unchanged_lines(g: &Glyphs, n: usize) -> String {
    let noun = if n == 1 { "line" } else { "lines" };
    format!("{} {} unchanged {noun}", g.more, grouped(n))
}

pub fn hidden_lines(n: usize) -> String {
    let noun = if n == 1 { "line" } else { "lines" };
    format!("{} more {noun}", grouped(n))
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Counts {
    pub requests: u32,
    pub tools: u32,
}

pub const LOW_READS: u8 = 50;

pub fn shown_rebuild(
    rebuilt: Option<RebuildReason>,
    cache_miss: bool,
    usage: Option<&Usage>,
) -> Option<RebuildReason> {
    let low = usage
        .and_then(Usage::cache_percent)
        .is_some_and(|pct| pct < LOW_READS);
    rebuilt.filter(|reason| {
        let whole = matches!(
            reason,
            RebuildReason::EndpointSwitch | RebuildReason::NativeSearchRefused
        );
        cache_miss || (whole && low)
    })
}

pub fn footer_parts(
    outcome: &TurnOutcome,
    stats: &TurnStats,
    cache_miss: bool,
    rebuilt: Option<RebuildReason>,
    counts: Counts,
) -> Vec<(Fact, Token)> {
    let mut parts = vec![(Fact::plain(duration(stats.duration_ms)), Token::TextFaint)];
    if counts.tools > 0 || counts.requests > 1 {
        parts.push((Fact::plain(requests(counts.requests)), Token::TextFaint));
    }
    if counts.tools > 0 {
        parts.push((Fact::plain(tools(counts.tools)), Token::TextFaint));
    }
    if stats.retries > 0 {
        parts.push((Fact::plain(retries(stats.retries)), Token::TextFaint));
    }
    if let TurnOutcome::Completed { stop_reason, .. } = outcome
        && let Some(words) = stop_words(stop_reason)
    {
        parts.push((Fact::plain(words), Token::Text));
    }
    match (outcome.cost(), outcome) {
        (Some(c), _) => parts.extend(
            cost_facts(&c)
                .into_iter()
                .map(|fact| (fact, Token::TextFaint)),
        ),
        (None, TurnOutcome::Completed { .. }) => {
            parts.push((Fact::plain(COST_UNKNOWN), Token::TextFaint));
        }
        (None, _) => parts.push((Fact::plain(POSSIBLY_BILLED), Token::Text)),
    }
    if let Some(reason) = shown_rebuild(rebuilt, cache_miss, outcome.usage()) {
        parts.push((Fact::plain(cache_rebuilt(&reason)), Token::TextFaint));
    } else if cache_miss {
        parts.push((Fact::plain(CACHE_MISS), Token::Text));
    } else if let Some(pct) = outcome.usage().and_then(Usage::cache_percent) {
        parts.push((Fact::plain(cache_rate(pct)), Token::TextFaint));
    }
    if let TurnOutcome::Completed {
        committed: false, ..
    } = outcome
    {
        parts.push((Fact::plain(NOT_KEPT), Token::Text));
    }
    parts
}

pub fn output_capped(tokens: &str) -> String {
    format!("output capped at {tokens} tokens")
}

pub const PICKER_NO_ROOM: &str = "Make the terminal taller: the picker doesn't fit.";

pub const UP_TO: &str = "up to";

pub const NO_CLIPBOARD_IMAGE: &str = "No image on the clipboard.";
pub const NO_CLIPBOARD_OVER_SSH: &str = "Paste the image's path: no clipboard over SSH.";
pub const NO_CLIPBOARD: &str = "Paste the image's path: no clipboard was found.";
pub const STILL_READING_CLIPBOARD: &str = "Still reading the clipboard.";
pub const CLIPBOARD_IMAGE: &str = "the clipboard image";
pub const CLIPBOARD_TOO_LARGE: &str = "The clipboard image is larger than 40 MB.";
pub const CLIPBOARD_TEXT_TOO_LARGE: &str = "The clipboard text is larger than 40 MB.";

pub fn reading_clipboard(more: &str) -> String {
    format!("Reading the clipboard{more}")
}

pub fn clipboard_unreadable(why: &str) -> String {
    format!("Couldn't read the clipboard: {why}.")
}

pub fn not_attached(what: &str, why: &str) -> String {
    format!("Couldn't attach {what}. {why}")
}

pub fn no_images_for(model: &str) -> String {
    format!(
        "{} can't take images: its limits are unknown.",
        model_line(model)
    )
}

pub fn too_many_images(max: usize) -> String {
    format!("Too many images: a message can carry at most {max}.")
}

pub fn image_chip(n: usize, size: (u32, u32), tokens: u64, set: GlyphSet) -> String {
    let glyphs = Glyphs::of(set);
    let times = match set {
        GlyphSet::Unicode => "\u{00D7}",
        GlyphSet::AmbiguousWide | GlyphSet::Ascii => "x",
    };
    let sep = glyphs.separator;
    format!(
        "[Image #{n} {sep} {}{times}{} {sep} {}{}]",
        size.0,
        size.1,
        approx(set),
        plural(tokens, "token", "tokens")
    )
}

pub const ASKING: &str = "Asking";
pub const ASKED: &str = "Asked";
pub const NOT_ANSWERED: &str = "not answered";
pub const STEP_IN_NOT_ANSWERED: &str = "Not answered";
pub const ANSWER_STOPPED: &str = "the turn was stopped";
pub const ANSWER_MESSAGE_INSTEAD: &str = "you wrote a message instead";
pub const ANSWER_INTERRUPTED: &str = "Wisp stopped";
pub const ANSWER_WITHDRAWN: &str = "withdrawn";
pub const ANSWER_CLOSED: &str = "you closed it";
pub const QUESTION_WITHDRAWN: &str = "The agent withdrew its question";
pub const SKIPPED: &str = "skipped";
pub const NOT_ANSWERED_YET: &str = "not answered yet";
pub const OWN_ANSWER: &str = "Type your own answer";
pub const OWN_PREFIX: &str = "Your own: ";
pub const OWN_SHORT: &str = "your own: ";
pub const CHOOSE_ANY: &str = "(choose any)";
pub const REVIEW: &str = "Review";
pub const SEND_ANSWERS: &str = "Send answers";
pub const GO_BACK: &str = "Go back";
pub const SECRET_FIELD: &str = "Secret";
pub const SECRET_SENDING: &str = "sending";
pub const SECRET_HIDE: &str = "hide ends";
pub const SECRET_SHOW: &str = "show ends";
pub const SECRET_CLEAR: &str = "clear";
pub const SECRET_EMPTY: &str = "Type or paste the secret first.";
pub const SECRET_CLOSED: &str = "Box closed; nothing was sent.";

pub fn secret_destination(destination: crate::chrome::SecretGoes<'_>, separator: &str) -> String {
    match destination {
        crate::chrome::SecretGoes::Process { id, command } => format!(
            "Goes to process {id} {separator} {}",
            crate::chrome::one_line(command.lines().next().unwrap_or_default())
        ),
        crate::chrome::SecretGoes::Server { name, command } => {
            let target = format!("Goes to the server {name}");
            command.map_or(target.clone(), |command| {
                format!("{target} {separator} {command}")
            })
        }
    }
}

pub const CARD_SETTLING: &str = "waiting for you to finish typing";
pub const CARD_SENDING: &str = "sending your answers";
pub const NEXT_QUESTION: &str = "next question";
pub const CARD_SKIPPED_SENT: &str = "unanswered questions are sent as skipped";
pub const WAITING: &str = "Waiting";

pub fn waiting_for_reports() -> String {
    format!("{WAITING} for reports")
}

pub fn waiting_for_answer() -> String {
    format!("{WAITING} for your answer")
}

pub fn waiting_fact() -> String {
    waiting_for_answer().to_lowercase()
}

pub fn answer_given(header: &str, words: &str) -> String {
    format!("You answered {header}: {words}")
}

pub const CACHE_EXPIRED_NOW: &str = "cache expired";

pub fn use_the_api() -> KeyHint {
    KeyHint::new(ENTER_KEY, "use the API")
}

pub fn copy_key() -> KeyHint {
    KeyHint::new(CTRL_C_KEY, COPY_WORD)
}

pub fn card_pick(rows: usize, set: GlyphSet) -> KeyHint {
    let dash = match set {
        GlyphSet::Unicode => "\u{2013}",
        GlyphSet::AmbiguousWide | GlyphSet::Ascii => "-",
    };
    KeyHint::new(format!("1{dash}{rows}"), CHOOSE_WORD)
}

pub fn question_of(n: usize, total: usize) -> String {
    format!("question {n} of {total}")
}

pub fn page_of(n: usize, total: usize) -> String {
    format!("{n} of {total}")
}

pub fn card_position(n: usize, total: usize, set: GlyphSet) -> String {
    match set {
        GlyphSet::Unicode => format!("\u{2039} {n}/{total} \u{203A}"),
        GlyphSet::AmbiguousWide | GlyphSet::Ascii => format!("< {n}/{total} >"),
    }
}

pub fn answered_count(answered: usize, total: usize) -> String {
    format!("{answered} of {total} answered")
}

pub fn cache_expires_in(minutes: u64) -> String {
    format!("cache expires in {minutes} min")
}

pub const ANSWER_BACK: &str = "What you typed for the question is back in the composer.";
pub const CARD_NO_PICKER: &str = "Answer the question first.";

pub const COMPACTING: &str = "Compacting";
pub const CONTEXT_COMPACTED: &str = "context compacted";
pub const COMPACTION_STOPPED: &str = "Compaction stopped.";
pub const NOTHING_CHANGED: &str = "Nothing changed";
pub const COMPACTED_PARTIAL: &str = "partial";
pub const COMPACTED_CONVERTED: &str = "converted";
pub const FOCUS_NOT_APPLIED: &str = "converted; focus not applied";
pub const NO_SUMMARY_WRITTEN: &str = "the model stopped without writing a summary";
pub const SUMMARY_CHECK_FAILED: &str = "the summary didn't pass Wisp's check";

pub const AUTO_PAUSED: &str = "Automatic compaction paused for the rest of this turn: the conversation is still over the point after compacting.";

pub const BACKGROUND_PREPARED: &str =
    "Summary prepared in the background; Wisp swaps it in on the next request";

pub fn background_prepared(cost: &[Fact]) -> String {
    if cost.is_empty() {
        return BACKGROUND_PREPARED.to_owned();
    }
    format!("{BACKGROUND_PREPARED}{SEP}{}", Fact::joined(cost, SEP))
}

pub fn auto_failed_paused(kind: &CompactionFailure, retried: bool) -> String {
    let what = failure_words(kind, retried);
    format!("Automatic compaction failed ({what}); paused for the rest of this turn.")
}

pub fn requested_by(hook: &str) -> String {
    format!("Compaction requested by {hook}")
}

pub fn request_failed(hook: &str, kind: &CompactionFailure, retried: bool) -> String {
    let what = failure_words(kind, retried);
    format!("Compaction requested by {hook} failed ({what}); hook requests wait for the next turn.")
}

pub fn request_refused(hook: &str, rule: &str) -> Option<String> {
    let reason = match rule {
        "request_failed" => "a compaction a hook asked for failed earlier in this turn",
        "request_blocked" => "a compaction a hook asked for was blocked earlier in this turn",
        "auto_paused" => {
            "automatic compaction failed or left the conversation over the point earlier in this turn"
        }
        _ => return None,
    };
    Some(format!(
        "Wisp refused the compaction {hook} asked for: {reason}. Hook requests wait for the next turn."
    ))
}

fn failure_words(kind: &CompactionFailure, retried: bool) -> String {
    match kind {
        CompactionFailure::Error(kind) if retried => {
            format!("{}, after retries", retry_cause(*kind, None))
        }
        CompactionFailure::Error(kind) => retry_cause(*kind, None).to_owned(),
        CompactionFailure::NoSummary { .. } => NO_SUMMARY_WRITTEN.to_owned(),
        CompactionFailure::CheckFailed => SUMMARY_CHECK_FAILED.to_owned(),
        CompactionFailure::Cancelled => "stopped".to_owned(),
        CompactionFailure::Discarded { reason } => reason.clone(),
        CompactionFailure::Blocked => "blocked by a hook".to_owned(),
        CompactionFailure::Other(text) => text.clone(),
    }
}

pub fn fill_change(before: u64, after: u64, arrow: &str) -> String {
    format!("{} {arrow} {}", format_tokens(before), format_tokens(after))
}

fn after_colon(provider: &str, text: &str) -> String {
    let text = text.trim().trim_end_matches('.');
    if text.starts_with(provider) || text.starts_with("Wisp") {
        return text.to_owned();
    }
    let mut chars = text.chars();
    chars.next().map_or_else(String::new, |first| {
        first.to_lowercase().chain(chars).collect()
    })
}

pub fn compaction_reason(
    provider: &str,
    kind: &CompactionFailure,
    message: &str,
) -> Option<String> {
    let reason = match kind {
        CompactionFailure::Cancelled => return None,
        CompactionFailure::Error(kind) => error_sentence(provider, *kind),
        CompactionFailure::NoSummary { .. } => NO_SUMMARY_WRITTEN.to_owned(),
        CompactionFailure::CheckFailed => SUMMARY_CHECK_FAILED.to_owned(),
        CompactionFailure::Discarded { reason } => reason.clone(),
        CompactionFailure::Blocked | CompactionFailure::Other(_) => message.to_owned(),
    };
    Some(after_colon(provider, &reason)).filter(|r| !r.is_empty())
}

pub fn compaction_ended(reason: Option<&str>, cost: Option<&str>, sep: &str) -> String {
    let lead = match reason {
        Some(reason) => format!("Compaction failed: {reason}."),
        None => COMPACTION_STOPPED.to_owned(),
    };
    match cost {
        Some(cost) => format!("{lead} {NOTHING_CHANGED}{sep}{cost}"),
        None => format!("{lead} {NOTHING_CHANGED}."),
    }
}

pub fn compaction_refused(reason: &str) -> String {
    let reason = reason.trim();
    if reason.is_empty() {
        "Not compacted.".to_owned()
    } else {
        reason.to_owned()
    }
}

pub fn compactions_so_far(n: u64, cost: Option<&str>) -> String {
    let counted = match n {
        0 => "no compactions".to_owned(),
        1 => "1 compaction".to_owned(),
        n => format!("{n} compactions"),
    };
    match cost {
        Some(cost) => format!("{counted} so far, {cost}"),
        None => format!("{counted} so far"),
    }
}

pub const SUMMARY_BLOCK_REFUSED: &str =
    "The server refused the summary block, so the existing summary was kept in Wisp's form.";

pub fn mail_label(kind: wisp_protocol::MailKind, quiet: bool, from: &str) -> String {
    let noun = match kind {
        wisp_protocol::MailKind::Task => "task",
        wisp_protocol::MailKind::Message if quiet => "note",
        wisp_protocol::MailKind::Report => "report",
        wisp_protocol::MailKind::Notice => "notice",
        wisp_protocol::MailKind::Message
        | wisp_protocol::MailKind::Question
        | wisp_protocol::MailKind::Unknown => "message",
    };
    format!("{noun} from {from}")
}

pub const AGENTS_TITLE: &str = "Agents";
pub const NO_AGENTS: &str = "No agents yet.";

pub fn node_word(state: wisp_protocol::slot::NodeState) -> &'static str {
    use wisp_protocol::slot::NodeState;
    match state {
        NodeState::Working => "working",
        NodeState::Waiting | NodeState::Unknown => "waiting",
        NodeState::Done => DONE_WORD,
        NodeState::Stopped => STOPPED_FACT,
        NodeState::Failed => FAILED_FACT,
    }
}

pub fn agents_count(n: usize) -> String {
    match n {
        1 => "1 agent".to_owned(),
        n => format!("{n} agents"),
    }
}

pub fn agents_working(n: usize) -> String {
    format!("{n} working")
}

pub fn agents_failed(n: usize) -> String {
    format!("{n} failed")
}

pub fn agent_state(state: wisp_protocol::AgentState) -> String {
    use wisp_protocol::{AgentState, QueueReason, slot::NodeState};
    match state {
        AgentState::Queued { .. } if state.node_state() == NodeState::Working => {
            AgentState::Running.as_str().to_owned()
        }
        AgentState::Queued {
            reason: QueueReason::Concurrency,
        } => "queued: running limit".to_owned(),
        AgentState::Queued {
            reason: QueueReason::Stagger,
        } => "queued: warming".to_owned(),
        AgentState::Queued {
            reason: QueueReason::Retry,
        } => "queued: retrying".to_owned(),
        AgentState::Idle {
            outcome: Some(outcome),
        } => format!("idle: {outcome}"),
        AgentState::Held { reason } => format!("held: {}", hold_words(reason).0),
        _ => state.as_str().to_owned(),
    }
}

pub fn agent_activity(activity: &wisp_protocol::Activity) -> Option<String> {
    match activity {
        wisp_protocol::Activity::Tool { name } => Some(name.clone()),
        wisp_protocol::Activity::Streaming => Some("writing".to_owned()),
        _ => None,
    }
}

pub fn spawned_in(cwd: &str) -> String {
    format!("in {cwd}")
}

pub fn reported_line(outcome: &str, line: Option<&str>, sep: &str) -> String {
    match line {
        Some(line) => format!("{outcome}{sep}{line}"),
        None => outcome.to_owned(),
    }
}

pub fn mail_waits(count: usize, reason: Option<wisp_protocol::HoldReason>) -> Vec<String> {
    let mut parts = match count {
        1 => vec![
            "1 message from an agent waits".to_owned(),
            "send a message to deliver it".to_owned(),
        ],
        n => vec![
            format!("{n} messages from agents wait"),
            "send a message to deliver them".to_owned(),
        ],
    };
    parts.extend(
        reason
            .and_then(|reason| hold_words(reason).1)
            .map(str::to_owned),
    );
    parts
}

fn hold_words(reason: wisp_protocol::HoldReason) -> (&'static str, Option<&'static str>) {
    use wisp_protocol::HoldReason;
    match reason {
        HoldReason::Stopped => ("stopped", Some("the last turn was stopped")),
        HoldReason::HeldMessages => ("your messages", Some("your messages are held")),
        HoldReason::RetryOffer => ("retry offered", Some("a retry is offered")),
        reason => (reason.as_str(), None),
    }
}

pub fn quit_with_agents(busy: usize) -> String {
    let agents = match busy {
        1 => "1 running agent stops".to_owned(),
        n => format!("{n} running agents stop"),
    };
    format!("Press {CTRL_C_KEY} again to quit: {agents} until {RESUME_COMMAND}")
}

pub const CHAT_STAYS_OPEN: &str = "The chat stays open.";
pub const BYE: &str = "Bye for now.";
pub const THAT_WAS: &str = "That was ";
pub const RESUME_COMMAND: &str = "wisp --resume";

pub fn goodbye_agents(stopped: usize, logged: bool) -> Option<String> {
    match (stopped, logged) {
        (0, true) => Some("Come back any time with:".to_owned()),
        (0, false) => None,
        (1, true) => Some("Continue your agent with:".to_owned()),
        (n, true) => Some(format!("Continue your {n} agents with:")),
        (1, false) => Some("Your agent isn't saved.".to_owned()),
        (n, false) => Some(format!("Your {n} agents aren't saved.")),
    }
}

pub fn stopped_main(busy: usize, sep: &str) -> String {
    let agents = match busy {
        1 => "1 agent keeps running".to_owned(),
        n => format!("{n} agents keep running"),
    };
    format!("Stopped {ROOT_NAME}{sep}{agents} (ctrl-t)")
}

pub fn main_needs_you() -> String {
    format!("{ROOT_NAME} needs you")
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Moves {
    #[default]
    Not,
    UpDown,
    Around,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct AgentKeys {
    pub opens: bool,
    pub release: bool,
    pub moves: Moves,
}

pub fn all_arrows(set: GlyphSet, moves: Moves) -> Option<KeyHint> {
    let keys = match (set, moves) {
        (_, Moves::Not) => return None,
        (GlyphSet::Unicode, Moves::UpDown) => return Some(arrows(set)),
        (GlyphSet::Unicode, Moves::Around) => "\u{2190}\u{2191}\u{2193}\u{2192}",
        (GlyphSet::AmbiguousWide | GlyphSet::Ascii, _) => "arrows",
    };
    Some(KeyHint::new(keys, MOVE_WORD))
}

pub fn agents_keys(set: GlyphSet, keys: AgentKeys, exit: &str) -> Vec<KeyHint> {
    let named = [
        keys.opens.then(|| KeyHint::new(ENTER_KEY, OPEN_WORD)),
        keys.release.then(|| KeyHint::new("r", RELEASE_WORD)),
        all_arrows(set, keys.moves),
    ];
    named.into_iter().flatten().chain([esc(exit)]).collect()
}

pub const RELEASE_WORD: &str = "release";

pub fn message_to(agent: &str) -> String {
    format!("Message {agent}")
}

pub const QUEUE_WORD: &str = "queue";

pub fn agent_held(agent: &str, failed: bool, held: usize, sep: &str) -> String {
    let what = if failed {
        format!("{agent}'s turn failed")
    } else {
        format!("{agent} stopped")
    };
    let count = messages(held, "queued message held", "queued messages held");
    format!("{what}{sep}{count}")
}

pub fn message_back(agent: &str) -> String {
    format!("Your message to {agent} is back in the composer")
}

pub fn message_came_back(agent: &str, sep: &str) -> String {
    format!("Your message to {agent} came back{sep}ctrl-t")
}

pub fn agent_ended(agent: &str, kept: usize, sep: &str) -> String {
    match kept {
        0 => format!("{agent} ended"),
        1 => format!("{agent} ended{sep}your message is in {ROOT_NAME}'s history"),
        n => format!("{agent} ended{sep}your {n} messages are in {ROOT_NAME}'s history"),
    }
}

pub fn presets_changed(count: usize) -> String {
    let presets = if count == 1 { "preset" } else { "presets" };
    format!("{count} {presets} changed since agents started; see /agents")
}

pub fn preset_change_mark(change: wisp_protocol::PresetChange) -> &'static str {
    use wisp_protocol::PresetChange;
    match change {
        PresetChange::Disabled => "preset off",
        PresetChange::Removed => "preset removed",
        PresetChange::Edited | PresetChange::Unknown => "preset changed",
    }
}

pub const NOT_PRICED: &str = "not priced";

pub const KEEP_WARM: &str = "keep-warm";

pub fn keep_warm_fact(warm: wisp_protocol::KeepWarm) -> String {
    if warm.paused {
        format!("{KEEP_WARM}: paused until its next turn")
    } else {
        format!(
            "{KEEP_WARM}: {} of {} refreshes",
            warm.refreshes, warm.limit
        )
    }
}

pub const NEEDS_NEWER_WISP: &str = "needs a newer Wisp";

pub fn couldnt_update(reason: &str) -> String {
    format!("couldn't update: {reason}")
}

pub fn stopped_updating(sep: &str) -> String {
    format!("stopped updating{sep}/config")
}

pub fn plugin_restarted(sep: &str) -> String {
    format!("{sep}plugin restarted")
}

pub fn slot_failure(failure: &wisp_protocol::slot::SlotFailure) -> String {
    use wisp_protocol::slot::SlotFailure;
    match failure {
        SlotFailure::Failed { message } => message.clone(),
        SlotFailure::TimedOut => "timed out".to_owned(),
        SlotFailure::TooLarge { bytes } => format!("content too large ({})", size(*bytes)),
        SlotFailure::Invalid { rule } => format!("invalid content ({rule})"),
        SlotFailure::Unknown => "unknown error".to_owned(),
    }
}

pub fn more_rows(more: &str, n: usize) -> String {
    format!("{more} {n} more")
}

pub fn more_sections(more: &str, n: usize) -> String {
    if n == 1 {
        format!("{more} 1 more section")
    } else {
        format!("{more} {n} more sections")
    }
}

pub fn instructions_note(
    from: Option<&wisp_protocol::SourceInfo>,
    to: Option<&wisp_protocol::SourceInfo>,
    changed: &[wisp_protocol::SourceChange],
) -> String {
    if let (Some(from), Some(to)) = (from, to)
        && from.scope != to.scope
    {
        return format!("instructions switched{SEP}{}", to.display_path);
    }
    let paths: Vec<&str> = changed.iter().map(|c| c.display_path.as_str()).collect();
    format!("instructions updated{SEP}{}", paths.join(", "))
}

pub fn tools_changed(
    added: &[String],
    removed: &[String],
    via: ToolChangeVia,
    cache: Option<&CacheEffect>,
) -> Vec<String> {
    let cache = match cache {
        Some(CacheEffect::Kept) => format!("{SEP}{CACHE_KEPT}"),
        Some(CacheEffect::KeptIfPassedOn) => format!("{SEP}{CACHE_KEPT_TOOLS}"),
        Some(CacheEffect::Rebuilt { .. }) => format!("{SEP}cache rebuilt (tools)"),
        None => String::new(),
    };
    if added.is_empty() && removed.is_empty() && via == ToolChangeVia::Rebuild && !cache.is_empty()
    {
        return vec![format!("tools rebuilt{cache}")];
    }
    let added_words = match via {
        ToolChangeVia::Rebuild => "tools added",
        ToolChangeVia::Inline | ToolChangeVia::TurnedOff => "tools on",
    };
    [("tools off", removed), (added_words, added)]
        .into_iter()
        .filter(|(_, names)| !names.is_empty())
        .map(|(words, names)| {
            let names: Vec<String> = names.iter().map(|n| crate::chrome::one_line(n)).collect();
            format!("{words}: {}{cache}", names.join(", "))
        })
        .collect()
}

pub fn plugin_name(id: &str, version: &str, source: &PluginSource) -> String {
    let name = format!(
        "{} {}",
        crate::chrome::one_line(id),
        crate::chrome::one_line(version)
    );
    match source {
        PluginSource::Wasm { sha256 } => {
            let short: String = crate::chrome::one_line(sha256).chars().take(8).collect();
            format!("{name} (wasm, {short})")
        }
        PluginSource::Bundled | PluginSource::Unknown => name,
    }
}

pub fn plugins_listed(words: &str, records: &[PluginRecord]) -> Option<String> {
    let names: Vec<String> = records
        .iter()
        .map(|r| plugin_name(&r.id, &r.version, &r.source))
        .collect();
    (!names.is_empty()).then(|| format!("{words}: {}", names.join(", ")))
}

pub fn plugins_changed(
    added: &[PluginRecord],
    updated: &[PluginRecord],
    removed: &[PluginRecord],
) -> Vec<String> {
    [
        ("plugins added", added),
        ("plugins updated", updated),
        ("plugins removed", removed),
    ]
    .into_iter()
    .filter_map(|(words, records)| plugins_listed(words, records))
    .collect()
}

pub fn plugin_note(note: &PluginNote) -> String {
    let plugin = crate::chrome::one_line(&note.plugin);
    format!(
        "plugin {plugin}: {}",
        crate::chrome::one_line(&note.message)
    )
}

pub fn subscriptions_estimate(plan: f64) -> String {
    format!(
        "{} {}",
        marked_amount(&usd(plan), true, false),
        wisp_protocol::SUBSCRIPTIONS_WORD
    )
}

pub fn cost_facts(c: &Cost) -> Vec<Fact> {
    let spend = AccountSpend::of(
        c.turn_usd,
        c.turn_plan_usd,
        c.turn_billing,
        c.turn_lower_bound,
    );
    total_facts(spend, c.turn_billing)
}

pub(crate) fn total_facts(spend: AccountSpend, billing: wisp_protocol::Billing) -> Vec<Fact> {
    match (billing, spend.api_usd(), spend.plan_usd) {
        (wisp_protocol::Billing::Mixed, Some(api), Some(plan)) => vec![
            Fact::Amount(marked_amount(&usd(api), false, spend.lower_bound)),
            Fact::plain(subscriptions_estimate(plan)),
        ],
        _ => Fact::billed_cost(spend.usd, spend.lower_bound, billing),
    }
}

pub fn mixed_note_parts() -> Vec<&'static str> {
    MIXED_BILLING_NOTE.split(SEP).collect()
}

pub const TYPE_ANSWER: &str = "Type your answer";

pub fn asked_by(asked_by: &wisp_protocol::AskedBy) -> Option<String> {
    match asked_by {
        wisp_protocol::AskedBy::Server { name } => Some(format!(
            "Asked by {}, one of your servers",
            crate::chrome::one_line(name)
        )),
        wisp_protocol::AskedBy::Unknown => None,
    }
}
