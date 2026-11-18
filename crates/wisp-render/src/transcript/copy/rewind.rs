use wisp_protocol::{Effort, RewindQuote, Warmth, marked_amount};

use super::{ESC_KEY, usd};
use crate::chrome::KeyHint;
use crate::theme::glyphs::GlyphSet;

pub fn again_to_rewind() -> KeyHint {
    KeyHint::new(ESC_KEY, "again to rewind")
}
pub const REWIND_BETWEEN_TURNS: &str = "/rewind runs between turns. Press esc to stop this one.";
pub const REWIND_AFTER_TURN: &str = "/rewind runs between turns. Send it after this one.";
pub const REWIND_WHILE_COMPACTING: &str = "/rewind waits for the compaction. Send it after.";
pub const REWIND_SUMMARY: &str = "Go back to an earlier message";
pub const REWIND_NAME: &str = "Rewind";
pub const TURN_COLUMN: &str = "turn";
pub const MESSAGE_COLUMN: &str = "message";
pub const CACHE_COLUMN: &str = "cache";
pub const REWIND_WORD: &str = "rewind";
pub const FILES_STAY: &str = "files stay as they are";
pub const FILES_UNCHANGED: &str = "files unchanged";
pub const REWIND_LOADING: &str = "loading";
pub const REWIND_SENT: &str = "rewinding";

pub fn point_turn(turn: u64) -> String {
    format!("turn {turn}")
}

fn dash(set: GlyphSet) -> &'static str {
    match set {
        GlyphSet::Unicode => "\u{2013}",
        GlyphSet::AmbiguousWide | GlyphSet::Ascii => "-",
    }
}

fn set_aside(dropped: &[u64], set: GlyphSet) -> String {
    match dropped {
        [] => "nothing set aside".to_owned(),
        [one] => format!("turn {one} set aside"),
        [first, .., last] if last - first + 1 == dropped.len() as u64 => {
            format!("turns {first}{}{last} set aside", dash(set))
        }
        many => format!("{} turns set aside", many.len()),
    }
}

pub fn rewound_divider(turn: u64, dropped: &[u64], sep: &str, set: GlyphSet) -> String {
    format!(
        "rewound to before turn {turn}{sep}{}{sep}{FILES_UNCHANGED}",
        set_aside(dropped, set)
    )
}

fn figure(micros: Option<u64>) -> Option<String> {
    micros.map(|m| marked_amount(&usd(m as f64 / 1_000_000.0), true, false).to_string())
}

pub fn quote_cost(quote: &RewindQuote) -> Option<String> {
    match quote.warm {
        Warmth::Warm => figure(quote.read_micros),
        _ => figure(quote.rewrite_micros),
    }
}

fn effort_word(effort: Effort) -> &'static str {
    effort.as_str()
}

pub fn quote_text(quote: &RewindQuote, next_model: &str, sep: &str) -> String {
    let cost = quote_cost(quote);
    let priced = |words: String| match &cost {
        Some(cost) => format!("{words} {cost}"),
        None => format!("{words} (no prices)"),
    };
    match &quote.warm {
        Warmth::Warm => priced("cache warm".to_owned()),
        Warmth::MayHaveExpired => priced("cache may have expired".to_owned()),
        Warmth::PrefixChanged => priced("cache rebuilt since".to_owned()),
        Warmth::OtherModel { model } => match &cost {
            Some(cost) => format!("cache is for {model}{sep}{cost} on {next_model}"),
            None => format!("cache is for {model} (no prices)"),
        },
        Warmth::OtherEndpoint { name } => match &cost {
            Some(cost) => format!("cache is on {name}{sep}{cost} here"),
            None => format!("cache is on {name} (no prices)"),
        },
        Warmth::OtherEffort { effort } => match &cost {
            Some(cost) => format!("cache is for {} effort{sep}{cost}", effort_word(*effort)),
            None => format!("cache is for {} effort (no prices)", effort_word(*effort)),
        },
    }
}

pub fn quote_short(quote: &RewindQuote) -> String {
    match (&quote.warm, quote_cost(quote)) {
        (Warmth::Warm, Some(cost)) => cost,
        (Warmth::Warm, None) => "warm".to_owned(),
        (_, Some(cost)) => cost,
        (_, None) => "cold".to_owned(),
    }
}

pub fn rewind_to(turn: u64, label: &str, sep: &str) -> String {
    if label.is_empty() {
        format!("Rewind to before turn {turn}{sep}{FILES_STAY}")
    } else {
        format!("Rewind to before turn {turn}{sep}\"{label}\"{sep}{FILES_STAY}")
    }
}

pub fn next_message_reads(quote: &RewindQuote) -> String {
    let tokens = crate::num(quote.prefix_tokens);
    let cost = quote_cost(quote)
        .map(|cost| format!(" ({cost})"))
        .unwrap_or_default();
    match quote.warm {
        Warmth::Warm => format!("Your next message reads about {tokens} cached tokens{cost}."),
        _ => format!("Your next message writes about {tokens} tokens to the cache again{cost}."),
    }
}

fn numbers(ids: &[u32]) -> String {
    let words: Vec<String> = ids.iter().map(u32::to_string).collect();
    super::listed(&words)
}

pub fn processes_keep_running(ids: &[u32], sep: &str) -> String {
    match ids {
        [] => String::new(),
        [one] => format!("Process {one} keeps running{sep}{}", super::PS_STOPS_IT),
        many => format!(
            "Processes {} keep running{sep}/ps stops them",
            numbers(many)
        ),
    }
}

pub fn processes_started_after(count: usize) -> String {
    match count {
        1 => "1 process started after this point".to_owned(),
        n => format!("{n} processes started after this point"),
    }
}

pub fn before_compaction(turn: u64) -> String {
    format!("before the compaction at turn {turn}")
}

pub fn compacts_first(turn: u64) -> String {
    format!(
        "before the compaction at turn {turn}: the conversation there is past the compaction point, so your next message compacts it first"
    )
}

pub fn returned_image(n: usize) -> String {
    format!("[Image #{n}]")
}

pub fn image_not_restored(ordinals: &[u32]) -> String {
    let names: Vec<String> = ordinals.iter().map(|n| format!("#{n}")).collect();
    match names.as_slice() {
        [] => String::new(),
        [one] => format!("image {one} wasn't restored: it couldn't be processed"),
        many => format!(
            "images {} weren't restored: they couldn't be processed",
            super::listed(many)
        ),
    }
}
