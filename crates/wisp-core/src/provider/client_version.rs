use std::fmt;
use std::ops::Range;
use std::path::Path;
use std::time::Duration;

use reqwest::Url;
use serde::{Deserialize, Serialize};
use toml::de::DeValue;

use crate::endpoints::ProviderKind;
use crate::models::list::{ListWait, REFRESH_AFTER};
use crate::models::{RowError, bad_value, table_of};
use crate::plan_usage::record::{self, Claim};

const LOOKUP_TIMEOUT: Duration = Duration::from_secs(3);
const RELEASE_LIMIT: usize = 1024 * 1024;

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(try_from = "String", into = "String")]
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

impl TryFrom<String> for Version {
    type Error = &'static str;

    fn try_from(value: String) -> Result<Self, Self::Error> {
        Self::parse(&value).ok_or("invalid client version")
    }
}

impl From<Version> for String {
    fn from(version: Version) -> Self {
        version.to_string()
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Latest {
    pub url: Url,
    pub tag_prefix: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Client {
    pub version: Version,
    pub latest: Option<Latest>,
}

impl Client {
    pub(crate) fn parse(at: &Range<usize>, value: &DeValue<'_>) -> Result<Self, RowError> {
        let bad = || {
            bad_value(
                "client",
                "a table with a version of three dot-separated numbers, an optional clean HTTPS latest URL and a tag_prefix of at most 16 ASCII letters, digits, hyphens or underscores",
                at,
            )
        };
        let [Some(version), latest, tag_prefix] =
            table_of(["version", "latest", "tag_prefix"], value, &bad)?
        else {
            return Err(bad());
        };
        let version = version.as_str().and_then(Version::parse).ok_or_else(bad)?;
        let tag_prefix = match tag_prefix {
            Some(value) => value
                .as_str()
                .filter(|prefix| {
                    prefix.len() <= 16
                        && prefix
                            .bytes()
                            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_'))
                })
                .ok_or_else(bad)?,
            None => "",
        };
        let latest = latest
            .map(|value| -> Result<_, RowError> {
                let url = value
                    .as_str()
                    .and_then(|s| Url::parse(s).ok())
                    .filter(|url| {
                        url.scheme() == "https"
                            && url.has_host()
                            && url.username().is_empty()
                            && url.password().is_none()
                            && url.query().is_none()
                            && url.fragment().is_none()
                    })
                    .ok_or_else(bad)?;
                Ok(Latest {
                    url,
                    tag_prefix: tag_prefix.to_owned(),
                })
            })
            .transpose()?;
        Ok(Self { version, latest })
    }

    pub(super) fn for_origin(mut self, official: bool) -> Self {
        if !official {
            self.latest = None;
        }
        self
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub(super) struct Record {
    pub(super) version: Option<Version>,
    pub(super) checked: i64,
}

pub(super) fn record_alias(kind: ProviderKind) -> String {
    format!("client-{}", kind.as_str())
}

pub(super) fn current(client: &Client, folder: Option<&Path>, kind: ProviderKind) -> Version {
    folder
        .and_then(|folder| record::read::<Record>(folder, &record_alias(kind)))
        .and_then(|record| record.version)
        .unwrap_or(client.version)
}

pub(super) async fn resolve(client: &Client, folder: Option<&Path>, kind: ProviderKind) -> Version {
    let (Some(folder), Some(latest)) = (folder, &client.latest) else {
        return current(client, folder, kind);
    };
    let alias = record_alias(kind);
    let Claim::Held(_lock) = record::claim(folder, &alias) else {
        return current(client, Some(folder), kind);
    };
    let now = jiff::Timestamp::now().as_second();
    let Ok(saved) = record::try_read::<Record>(folder, &alias) else {
        return client.version;
    };
    let version = saved.and_then(|saved| saved.version);
    let fallback = version.unwrap_or(client.version);
    if saved.is_some_and(|saved| now.saturating_sub(saved.checked) < REFRESH_AFTER) {
        return fallback;
    }
    let mut attempt = Record {
        version,
        checked: now,
    };
    if record::write(folder, &alias, &attempt).is_err() {
        return fallback;
    }
    ListWait::reserve_for_release(LOOKUP_TIMEOUT);
    let Some(found) = lookup(latest).await else {
        return fallback;
    };
    attempt.version = Some(found);
    let _ = record::write(folder, &alias, &attempt);
    found
}

pub(crate) async fn lookup(latest: &Latest) -> Option<Version> {
    let client = reqwest::Client::builder()
        .no_proxy()
        .redirect(reqwest::redirect::Policy::none())
        .retry(reqwest::retry::never())
        .timeout(LOOKUP_TIMEOUT)
        .build()
        .ok()?;
    let response = client
        .get(latest.url.clone())
        .header(reqwest::header::ACCEPT, "application/vnd.github+json")
        .header("x-github-api-version", "2022-11-28")
        .header(reqwest::header::USER_AGENT, crate::plugin::http::USER_AGENT)
        .send()
        .await
        .ok()?;
    if response.status() != reqwest::StatusCode::OK {
        return None;
    }
    let bytes = super::body::read_capped(response, RELEASE_LIMIT)
        .await
        .ok()?;
    if bytes.trim_ascii_start().first() != Some(&b'{') {
        return None;
    }
    let release = serde_json::from_slice(&bytes).ok()?;
    tag_version(&release, &latest.tag_prefix)
}

#[derive(Deserialize)]
struct Release {
    tag_name: String,
    draft: bool,
    prerelease: bool,
}

fn tag_version(release: &Release, prefix: &str) -> Option<Version> {
    if release.draft || release.prerelease || release.tag_name.len() > 64 {
        return None;
    }
    Version::parse(release.tag_name.strip_prefix(prefix)?)
}

#[cfg(test)]
mod tests;
