use std::fmt;

use serde::de::Error as _;
use serde::{Deserialize, Deserializer, Serialize, Serializer};

use super::Percent;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TokensOrPercent {
    Tokens(u64),
    Share(Percent),
}

impl TokensOrPercent {
    pub fn parse(text: &str) -> Option<Self> {
        let text = text.trim();
        if let Some(share) = text.strip_suffix('%') {
            let share = share.trim();
            if share.is_empty() || !share.bytes().all(|b| b.is_ascii_digit()) {
                return None;
            }
            let value: u8 = share.parse().ok()?;
            return Percent::new(value).ok().map(Self::Share);
        }
        parse_tokens(text).map(Self::Tokens)
    }

    pub fn of(value: &SettingValue) -> Option<Self> {
        match value {
            SettingValue::Tokens(tokens) => Some(Self::Tokens(*tokens)),
            SettingValue::Percent(percent) => Some(Self::Share(*percent)),
            _ => None,
        }
    }

    pub fn tokens_on(self, window: u64) -> u64 {
        match self {
            Self::Tokens(tokens) => tokens,
            Self::Share(percent) => super::share_point(window, percent),
        }
    }
}

impl fmt::Display for TokensOrPercent {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Tokens(tokens) => write!(f, "{tokens}"),
            Self::Share(percent) => write!(f, "{percent}"),
        }
    }
}

pub fn parse_tokens(text: &str) -> Option<u64> {
    let text = text.trim();
    let (digits, scale) = match text.char_indices().last() {
        Some((i, 'k' | 'K')) => (&text[..i], 1_000),
        Some((i, 'm' | 'M')) => (&text[..i], 1_000_000),
        _ => (text, 1),
    };
    if digits.is_empty() || !digits.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    digits.parse::<u64>().ok()?.checked_mul(scale)
}

pub fn parse_bool(text: &str) -> Option<bool> {
    match text.trim().to_ascii_lowercase().as_str() {
        "on" | "true" | "yes" => Some(true),
        "off" | "false" | "no" => Some(false),
        _ => None,
    }
}

pub const MICROS_PER_USD: u64 = 1_000_000;

pub fn parse_usd(text: &str) -> Option<u64> {
    let text = text.trim();
    let text = text.strip_prefix('$').unwrap_or(text);
    let (whole, fraction) = match text.split_once('.') {
        Some((whole, fraction)) => (whole, fraction),
        None => (text, ""),
    };
    if whole.is_empty() && fraction.is_empty() {
        return None;
    }
    let digits = |s: &str| s.bytes().all(|b| b.is_ascii_digit());
    if !digits(whole) || !digits(fraction) || fraction.len() > 6 {
        return None;
    }
    let whole: u64 = if whole.is_empty() {
        0
    } else {
        whole.parse().ok()?
    };
    let padded = format!("{fraction:0<6}");
    let fraction: u64 = padded.parse().ok()?;
    whole.checked_mul(MICROS_PER_USD)?.checked_add(fraction)
}

pub fn usd_text(micros: u64) -> String {
    let whole = micros / MICROS_PER_USD;
    let fraction = micros % MICROS_PER_USD;
    if fraction == 0 {
        return whole.to_string();
    }
    let digits = format!("{fraction:06}");
    format!("{whole}.{}", digits.trim_end_matches('0'))
}

pub fn usd_shown(micros: u64) -> String {
    let whole = micros / MICROS_PER_USD;
    let fraction = micros % MICROS_PER_USD;
    if fraction.is_multiple_of(10_000) {
        format!("${whole}.{:02}", fraction / 10_000)
    } else {
        format!("${}", usd_text(micros))
    }
}

pub fn thousands(n: u64) -> String {
    let digits = n.to_string();
    let mut out = String::new();
    for (i, c) in digits.chars().enumerate() {
        if i > 0 && (digits.len() - i).is_multiple_of(3) {
            out.push(',');
        }
        out.push(c);
    }
    out
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SettingValue {
    Bool(bool),
    Text(String),
    Int(i64),
    Tokens(u64),
    Percent(Percent),
    Usd { micros: u64 },
    List(Vec<String>),
    Other { kind: String, text: String },
}

impl SettingValue {
    pub fn kind_name(&self) -> &str {
        match self {
            Self::Bool(_) => "bool",
            Self::Text(_) => "text",
            Self::Int(_) => "int",
            Self::Tokens(_) => "tokens",
            Self::Percent(_) => "percent",
            Self::Usd { .. } => "usd",
            Self::List(_) => "list",
            Self::Other { kind, .. } => kind,
        }
    }

    pub fn text(&self) -> String {
        match self {
            Self::Bool(true) => "on".to_owned(),
            Self::Bool(false) => "off".to_owned(),
            Self::Text(text) => text.clone(),
            Self::Int(n) => n.to_string(),
            Self::Tokens(n) => n.to_string(),
            Self::Percent(p) => p.to_string(),
            Self::Usd { micros } => usd_text(*micros),
            Self::List(items) => items.join(","),
            Self::Other { text, .. } => text.clone(),
        }
    }

    pub fn shown(&self) -> String {
        match self {
            Self::Tokens(n) => thousands(*n),
            Self::Usd { micros } => usd_shown(*micros),
            Self::List(items) if items.is_empty() => "none".to_owned(),
            Self::List(items) => items.join(", "),
            other => other.text(),
        }
    }

    pub fn from_wire(kind: &str, text: &str) -> Self {
        let other = || Self::Other {
            kind: kind.to_owned(),
            text: text.to_owned(),
        };
        let parsed = match kind {
            "bool" => match text {
                "on" => Some(Self::Bool(true)),
                "off" => Some(Self::Bool(false)),
                _ => None,
            },
            "text" => Some(Self::Text(text.to_owned())),
            "int" => text.parse().ok().map(Self::Int),
            "tokens" => text.parse().ok().map(Self::Tokens),
            "percent" => text
                .strip_suffix('%')
                .and_then(|p| p.parse::<u8>().ok())
                .and_then(|p| Percent::new(p).ok())
                .map(Self::Percent),
            "usd" => parse_usd(text).map(|micros| Self::Usd { micros }),
            "list" if text.is_empty() => Some(Self::List(Vec::new())),
            "list" => Some(Self::List(text.split(',').map(str::to_owned).collect())),
            _ => None,
        };
        let value = parsed.unwrap_or_else(other);
        if value.text() == text { value } else { other() }
    }
}

impl From<TokensOrPercent> for SettingValue {
    fn from(value: TokensOrPercent) -> Self {
        match value {
            TokensOrPercent::Tokens(n) => Self::Tokens(n),
            TokensOrPercent::Share(p) => Self::Percent(p),
        }
    }
}

#[derive(Serialize, Deserialize)]
struct Wire {
    kind: String,
    text: String,
}

impl Serialize for SettingValue {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        Wire {
            kind: self.kind_name().to_owned(),
            text: self.text(),
        }
        .serialize(serializer)
    }
}

impl<'de> Deserialize<'de> for SettingValue {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let wire = Wire::deserialize(deserializer).map_err(D::Error::custom)?;
        Ok(Self::from_wire(&wire.kind, &wire.text))
    }
}
