use std::collections::BTreeMap;
use std::fmt;
use std::path::Path;
use std::sync::Arc;

use futures::future::BoxFuture;
use sha2::{Digest, Sha256};

use super::catalog::LiveCatalog;
use super::generation::Unavailable;
use super::host::Reconnected;

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Stamp(pub(crate) Vec<Option<(u64, i64, i64, u64)>>);

impl Stamp {
    pub fn of<'a>(paths: impl IntoIterator<Item = Option<&'a Path>>) -> Self {
        Self(
            paths
                .into_iter()
                .map(|path| std::fs::metadata(path?).ok().map(|meta| stamp_of(&meta)))
                .collect(),
        )
    }
}

#[cfg(unix)]
fn stamp_of(meta: &std::fs::Metadata) -> (u64, i64, i64, u64) {
    use std::os::unix::fs::MetadataExt;
    (meta.len(), meta.mtime(), meta.mtime_nsec(), meta.ino())
}

#[cfg(not(unix))]
fn stamp_of(meta: &std::fs::Metadata) -> (u64, i64, i64, u64) {
    let since = meta
        .modified()
        .ok()
        .and_then(|at| at.duration_since(std::time::UNIX_EPOCH).ok())
        .unwrap_or_default();
    let secs = i64::try_from(since.as_secs()).unwrap_or(i64::MAX);
    (meta.len(), secs, i64::from(since.subsec_nanos()), 0)
}

#[derive(Clone, PartialEq, Eq)]
pub struct Print([u8; 32]);

impl Print {
    pub fn of<'a>(parts: impl IntoIterator<Item = &'a [u8]>) -> Self {
        let mut hasher = Sha256::new();
        for part in parts {
            hasher.update(part.len().to_le_bytes());
            hasher.update(part);
        }
        Self(hasher.finalize().into())
    }
}

impl fmt::Debug for Print {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("Print(<redacted>)")
    }
}

pub type Prints = BTreeMap<String, Print>;

pub fn prints(entries: &BTreeMap<super::EndpointName, super::Availability>) -> Prints {
    use crate::provider::credential::Credential;
    super::listed(entries)
        .map(|(name, entry)| {
            let print = match entry {
                super::Availability::Ready(r) => {
                    let secret = match &r.credential {
                        Credential::ApiKey(key) | Credential::AuthToken(key) => key.as_bytes(),
                        Credential::Subscription { profile, .. } => profile
                            .as_deref()
                            .map_or(&[][..], |p| p.as_os_str().as_encoded_bytes()),
                    };
                    let shape = format!("{:?}{:?}{:?}", r.provider, r.key_env, r.pricing);
                    Print::of([shape.as_bytes(), r.base_url.as_bytes(), secret])
                }
                super::Availability::Unavailable(unready) => {
                    let cause = format!("{:?}", unready.cause);
                    Print::of([
                        &b"unavailable"[..],
                        cause.as_bytes(),
                        unready.reason.as_bytes(),
                    ])
                }
                super::Availability::SameAs(other) => Print::of([other.as_str().as_bytes()]),
            };
            (name.to_string(), print)
        })
        .collect()
}

pub struct Look {
    pub prints: Prints,
    pub view: Reconnected,
}

pub trait AccountsWatch: Send + Sync {
    fn stamp(&self) -> Stamp;
    fn look(
        &self,
        off: crate::plugin::registry::ProvidersOff,
    ) -> BoxFuture<'_, Result<Look, String>>;
}

pub(crate) struct Watch {
    source: Arc<dyn AccountsWatch>,
    last: tokio::sync::Mutex<(
        Option<(Stamp, crate::plugin::registry::ProvidersOff)>,
        Prints,
    )>,
}

impl Watch {
    pub(crate) fn new(source: Arc<dyn AccountsWatch>, prints: Prints) -> Self {
        Self {
            source,
            last: tokio::sync::Mutex::new((None, prints)),
        }
    }

    pub(crate) async fn check(&self, catalog: &LiveCatalog) {
        let mut last = self.last.lock().await;
        let stamp = (self.source.stamp(), catalog.providers_off());
        if last.0.as_ref() == Some(&stamp) {
            return;
        }
        let Ok(look) = self.source.look(stamp.1.clone()).await else {
            return;
        };
        let here = catalog.taken_here();
        let mut kept = look.prints.clone();
        let mut again = false;
        for (name, print) in &look.prints {
            if last.1.get(name) == Some(print)
                || (here.contains(name) && !catalog.provider_off(name))
            {
                continue;
            }
            if let Err(Unavailable::Busy) = catalog.changed_elsewhere(name, look.view.clone()) {
                again = true;
                match last.1.get(name) {
                    Some(old) => kept.insert(name.clone(), old.clone()),
                    None => kept.remove(name),
                };
            }
        }
        for name in last
            .1
            .keys()
            .filter(|name| !look.prints.contains_key(*name))
        {
            catalog.removed_elsewhere(name, look.view.clone());
        }
        catalog.remarked(&look.view.endpoints);
        *last = ((!again).then_some(stamp), kept);
    }
}

#[cfg(test)]
mod tests;
