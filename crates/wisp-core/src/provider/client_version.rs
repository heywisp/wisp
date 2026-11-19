use std::fmt;
use std::ops::Range;

use toml::de::DeValue;

use crate::models::{RowError, bad_value, table_of};

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) struct Version([u32; 3]);

impl Version {
    pub(crate) fn parse(version: &str) -> Option<Self> {
        let mut parts = version.split('.');
        let mut numbers = [0; 3];
        for number in &mut numbers {
            let part = parts.next()?;
            if !(1..=9).contains(&part.len())
                || (part.len() > 1 && part.starts_with('0'))
                || !part.bytes().all(|byte| byte.is_ascii_digit())
            {
                return None;
            }
            *number = part.parse().ok()?;
        }
        parts.next().is_none().then_some(Self(numbers))
    }
}

impl fmt::Display for Version {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let [major, minor, patch] = self.0;
        write!(f, "{major}.{minor}.{patch}")
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct Client {
    pub version: Version,
}

impl Client {
    pub(crate) fn parse(at: &Range<usize>, value: &DeValue<'_>) -> Result<Self, RowError> {
        let bad = || {
            bad_value(
                "client",
                "a table with a version of three dot-separated numbers",
                at,
            )
        };
        let [Some(version)] = table_of(["version"], value, &bad)? else {
            return Err(bad());
        };
        let version = version.as_str().and_then(Version::parse).ok_or_else(bad)?;
        Ok(Self { version })
    }
}
