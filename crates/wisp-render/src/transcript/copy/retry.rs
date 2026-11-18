use crate::theme::glyphs::{GlyphSet, Glyphs};

pub const RETRY: &str = "Retry";
pub const SIGN_IN_THEN_RETRY: &str = "Sign in again, then retry";
pub const NEW_KEY_THEN_RETRY: &str = "Enter a new key, then retry";
pub const SWITCH_ACCOUNT: &str = "Switch account";
pub const RETRY_ON_TITLE: &str = "Retry on which account?";
pub const RETRY_WORD: &str = "retry";
pub const BACK_WORD: &str = "back";
pub const SENDS_AGAIN: &str = "sends your message again";
pub const PICK_THEN_RETRY: &str = "pick one, then retry";
pub const SWITCH_REBUILDS: &str = super::CACHE_REBUILD;
pub const DISMISS_WORD: &str = "dismiss";
pub const RETRIED_TAG: &str = "retried";
pub const CACHE_WARM: &str = "cache warm";
pub const CACHE_MAY_HAVE_EXPIRED: &str = "cache may have expired";
pub const RETRIES_THEN: &str = "Wisp retries by itself then.";
pub const RETRIES_AT_RESET: &str = "Wisp retries by itself when it resets.";

pub fn from_step(step: u32) -> String {
    format!("from step {step}")
}

pub fn on_endpoint(endpoint: &str) -> String {
    format!("on {endpoint}")
}

pub fn asked_to_wait(wait: &str) -> String {
    format!("asked to wait {wait}")
}

pub fn retried(on: Option<&str>) -> String {
    match on {
        Some(endpoint) => format!("Retried on {endpoint}"),
        None => "Retried".to_owned(),
    }
}

pub fn retry_refused(reason: &str) -> String {
    format!("Couldn't retry: {reason}.")
}

pub fn retrying(step: Option<u32>, endpoint: &str) -> String {
    match step {
        Some(step) => format!("Retrying step {step} on {endpoint}"),
        None => format!("Retrying your message on {endpoint}"),
    }
}

pub fn retrying_on(
    step: Option<u32>,
    endpoint: &str,
    model: &str,
    effort: Option<&str>,
) -> [String; 2] {
    [retrying(step, endpoint), model_at(model, effort)]
}

pub fn model_at(model: &str, effort: Option<&str>) -> String {
    let model = crate::chrome::model_line(model);
    match effort {
        Some(effort) => format!("{model} at {effort}"),
        None => model,
    }
}

pub fn applies_next(change: &str) -> String {
    format!("your switch to {change} applies from your next message")
}

pub fn retry_mark(set: GlyphSet) -> &'static str {
    match set {
        GlyphSet::Unicode => "\u{21BB} ",
        GlyphSet::AmbiguousWide | GlyphSet::Ascii => "",
    }
}

pub fn checking(set: GlyphSet) -> String {
    format!("checking{}", Glyphs::of(set).more)
}
