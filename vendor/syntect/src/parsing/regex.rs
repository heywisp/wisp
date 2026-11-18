use std::sync::OnceLock;

use serde::de::{Deserialize, Deserializer};

pub const BACKTRACK_LIMIT: usize = 30_000;

#[derive(Debug)]
pub struct Regex {
    regex_str: String,
    regex: OnceLock<Option<fancy_regex::Regex>>,
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct Region {
    positions: Vec<Option<(usize, usize)>>,
}

fn compile(regex_str: &str) -> Option<fancy_regex::Regex> {
    fancy_regex::RegexBuilder::new(regex_str)
        .oniguruma_mode(true)
        .backtrack_limit(BACKTRACK_LIMIT)
        .build()
        .ok()
}

impl Regex {
    pub fn new(regex_str: String) -> Regex {
        Regex {
            regex_str,
            regex: OnceLock::new(),
        }
    }

    pub fn regex_str(&self) -> &str {
        &self.regex_str
    }

    pub fn compiles(&self) -> bool {
        self.regex().is_some()
    }

    pub fn is_match(&self, text: &str) -> bool {
        self.regex()
            .is_some_and(|r| r.is_match(text).unwrap_or(false))
    }

    pub fn search(
        &self,
        text: &str,
        begin: usize,
        end: usize,
        region: Option<&mut Region>,
    ) -> bool {
        let Some(regex) = self.regex() else {
            return false;
        };
        let Some(haystack) = text.get(..end) else {
            return false;
        };
        if begin > haystack.len() {
            return false;
        }
        match regex.captures_from_pos(haystack, begin) {
            Ok(Some(captures)) => {
                if let Some(region) = region {
                    region.positions.clear();
                    region.positions.extend(
                        (0..captures.len()).map(|i| captures.get(i).map(|m| (m.start(), m.end()))),
                    );
                }
                true
            }
            Ok(None) | Err(_) => false,
        }
    }

    fn regex(&self) -> Option<&fancy_regex::Regex> {
        self.regex.get_or_init(|| compile(&self.regex_str)).as_ref()
    }
}

impl Clone for Regex {
    fn clone(&self) -> Self {
        Regex::new(self.regex_str.clone())
    }
}

impl PartialEq for Regex {
    fn eq(&self, other: &Regex) -> bool {
        self.regex_str == other.regex_str
    }
}

impl Eq for Regex {}

impl<'de> Deserialize<'de> for Regex {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let regex_str = String::deserialize(deserializer)?;
        Ok(Regex::new(regex_str))
    }
}

impl Region {
    pub fn new() -> Region {
        Region::default()
    }

    pub fn pos(&self, index: usize) -> Option<(usize, usize)> {
        self.positions.get(index).copied().flatten()
    }
}
