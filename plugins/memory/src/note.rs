use std::fmt::Write as _;

use jiff::Timestamp;
use jiff::civil::Date;
use jiff::tz::TimeZone;
use serde::{Deserialize, Serialize};
use wisp_plugin_api::host::Instant;

pub(super) const FILE_MAX: u32 = 4096;
pub(super) const SLUG_MIN: usize = 1;
pub(super) const SLUG_MAX: usize = 48;
pub(super) const FACT_MAX: usize = 240;
pub(super) const SUFFIX: &str = ".md";

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
pub(super) enum Scope {
    Project,
    You,
}

impl Scope {
    pub(super) fn key(self, name: &str) -> String {
        let scope = match self {
            Self::Project => "project",
            Self::You => "user",
        };
        format!("memory.{scope}.{name}")
    }

    pub(super) fn label(self) -> &'static str {
        match self {
            Self::Project => "This project",
            Self::You => "You",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub(super) struct Identity {
    pub(super) scope: Scope,
    pub(super) name: String,
    pub(super) sha256: String,
}

pub(super) struct Note {
    pub(super) id: Identity,
    pub(super) fact: String,
    pub(super) saved: Date,
}

pub(super) fn slug(name: &str) -> bool {
    let bytes = name.as_bytes();
    let alnum = |b: u8| b.is_ascii_lowercase() || b.is_ascii_digit();
    (SLUG_MIN..=SLUG_MAX).contains(&bytes.len())
        && bytes.first().is_some_and(|b| alnum(*b))
        && bytes.iter().all(|b| alnum(*b) || *b == b'-')
}

pub(super) fn valid_fact(fact: &str) -> bool {
    fact.len() <= FACT_MAX
        && !fact.trim().is_empty()
        && !fact.contains([
            '\n', '\r', '\x0B', '\x0C', '\u{0085}', '\u{2028}', '\u{2029}',
        ])
}

fn day(text: &str) -> Option<Date> {
    if text.len() != 10
        || !text.bytes().enumerate().all(|(i, b)| {
            if matches!(i, 4 | 7) {
                b == b'-'
            } else {
                b.is_ascii_digit()
            }
        })
    {
        return None;
    }
    text.parse::<Date>().ok().filter(|d| d.year() > 0)
}

pub(super) fn utc_day(now: Instant) -> Option<Date> {
    let day = Timestamp::new(now.seconds, i32::try_from(now.nanos).ok()?)
        .ok()?
        .to_zoned(TimeZone::UTC)
        .date();
    (day.year() > 0).then_some(day)
}

pub(super) fn encode(name: &str, fact: &str, saved: impl std::fmt::Display, body: &str) -> String {
    let fact = serde_json::Value::String(fact.to_owned());
    format!("---\nname: {name}\ndescription: {fact}\nsaved: {saved}\n---\n{body}")
}

pub(super) fn decode(scope: Scope, name: &str, raw: &str) -> Option<(Note, String)> {
    let (parsed, saved) = wisp_instructions::skills::parse_with_saved(raw).ok()?;
    if parsed.name != name || !valid_fact(&parsed.description) {
        return None;
    }
    Some((
        Note {
            id: Identity {
                scope,
                name: parsed.name,
                sha256: wisp_instructions::sha256_hex(raw),
            },
            fact: parsed.description,
            saved: day(saved.as_deref()?)?,
        },
        parsed.body,
    ))
}

pub(super) fn escape(text: &str) -> String {
    let mut out = String::new();
    for ch in text.chars() {
        match ch {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            ch if ch.is_control() => {
                let _ = write!(out, "\\u{{{:x}}}", u32::from(ch));
            }
            ch => out.push(ch),
        }
    }
    out
}
