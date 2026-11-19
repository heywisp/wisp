use std::ops::Range;
use std::path::Path;
use std::time::Duration;

use reqwest::Url;
use serde::{Deserialize, Serialize};
use toml::de::DeValue;
use wisp_protocol::settings::ClientVersion;

use crate::endpoints::{ProviderKind, ResolvedEndpoint};
use crate::models::list::{ListWait, REFRESH_AFTER};
use crate::models::{ModelTable, RowError, bad_value, table_of};
use crate::plan_usage::record::{self, Claim};

const LOOKUP_TIMEOUT: Duration = Duration::from_secs(3);
const RELEASE_LIMIT: usize = 1024 * 1024;

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Latest {
    pub url: Url,
    pub tag_prefix: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Client {
    pub version: ClientVersion,
    pub latest: Option<Latest>,
    pub pinned: Option<ClientVersion>,
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
        let version = version
            .as_str()
            .and_then(ClientVersion::parse)
            .ok_or_else(bad)?;
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
        Ok(Self {
            version,
            latest,
            pinned: None,
        })
    }
}

pub(super) fn for_account(
    row: &Client,
    subscription: bool,
    official: bool,
    pinned: Option<ClientVersion>,
) -> Option<Client> {
    subscription.then(|| Client {
        version: row.version,
        latest: row.latest.as_ref().filter(|_| official).cloned(),
        pinned,
    })
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Lookup {
    NoRelease,
    Busy,
    Unrecorded(Option<ClientVersion>),
    Kept(ClientVersion),
    Found(ClientVersion),
}

pub(super) fn account_client(endpoint: &ResolvedEndpoint) -> Option<Client> {
    ModelTable::shared()
        .provider(endpoint.provider)
        .and_then(|row| row.client.as_ref())
        .and_then(|row| {
            for_account(
                row,
                endpoint.credential.subscription().is_some(),
                endpoint.official(),
                endpoint.client_version,
            )
        })
}

pub async fn check_account(endpoint: &ResolvedEndpoint, folder: Option<&Path>) -> Lookup {
    let Some(client) = account_client(endpoint) else {
        return Lookup::NoRelease;
    };
    check(&client, folder, endpoint.provider).await
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub(super) struct Record {
    pub(super) version: Option<ClientVersion>,
    pub(super) checked: i64,
}

pub(super) fn record_alias(kind: ProviderKind) -> String {
    format!("client-{}", kind.as_str())
}

pub(super) fn current(client: &Client, folder: Option<&Path>, kind: ProviderKind) -> ClientVersion {
    client
        .pinned
        .or_else(|| {
            folder
                .and_then(|folder| record::read::<Record>(folder, &record_alias(kind)))
                .and_then(|record| record.version)
        })
        .unwrap_or(client.version)
}

pub(super) async fn resolve(
    client: &Client,
    folder: Option<&Path>,
    kind: ProviderKind,
) -> ClientVersion {
    if let Some(version) = client.pinned {
        return version;
    }
    match attempt(client, folder, kind, false).await {
        Lookup::Found(version) | Lookup::Kept(version) | Lookup::Unrecorded(Some(version)) => {
            version
        }
        _ => current(client, folder, kind),
    }
}

async fn check(client: &Client, folder: Option<&Path>, kind: ProviderKind) -> Lookup {
    attempt(client, folder, kind, true).await
}

async fn attempt(
    client: &Client,
    folder: Option<&Path>,
    kind: ProviderKind,
    force: bool,
) -> Lookup {
    let Some(latest) = &client.latest else {
        return Lookup::NoRelease;
    };
    let Some(folder) = folder else {
        return Lookup::Unrecorded(None);
    };
    let alias = record_alias(kind);
    let _lock = match record::claim(folder, &alias) {
        Claim::Held(lock) => lock,
        Claim::Busy => return Lookup::Busy,
        Claim::Unshared => return Lookup::Unrecorded(None),
    };
    let now = jiff::Timestamp::now().as_second();
    let Ok(saved) = record::try_read::<Record>(folder, &alias) else {
        return Lookup::Unrecorded(None);
    };
    let version = saved.and_then(|saved| saved.version);
    let fallback = version.unwrap_or(client.version);
    if !force && saved.is_some_and(|saved| now.saturating_sub(saved.checked) < REFRESH_AFTER) {
        return Lookup::Kept(fallback);
    }
    let mut attempt = Record {
        version,
        checked: now,
    };
    if record::write(folder, &alias, &attempt).is_err() {
        return Lookup::Unrecorded(None);
    }
    ListWait::reserve_for_release(LOOKUP_TIMEOUT);
    let Some(found) = lookup(latest).await else {
        return Lookup::Kept(fallback);
    };
    attempt.version = Some(found);
    match record::write(folder, &alias, &attempt) {
        Ok(()) => Lookup::Found(found),
        Err(_) => Lookup::Unrecorded(Some(found)),
    }
}

pub(crate) async fn lookup(latest: &Latest) -> Option<ClientVersion> {
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

fn tag_version(release: &Release, prefix: &str) -> Option<ClientVersion> {
    if release.draft || release.prerelease || release.tag_name.len() > 64 {
        return None;
    }
    ClientVersion::parse(release.tag_name.strip_prefix(prefix)?)
}

#[cfg(test)]
mod tests;
