use std::collections::BTreeMap;
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::time::Duration;

use tokio::sync::Notify;
use tokio::time::Instant;

use crate::provider::PrefixBytes;
use crate::session::{sha256_bytes_hex, sha256_hex};

pub const WARM_FOR: Duration = Duration::from_secs(60 * 60);
pub const STAGGER_FOR: Duration = Duration::from_secs(5);

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Component {
    Endpoint,
    Model,
    Effort,
    Thinking,
    Tools,
    System,
}

impl Component {
    pub const ALL: [Self; 6] = [
        Self::Endpoint,
        Self::Model,
        Self::Effort,
        Self::Thinking,
        Self::Tools,
        Self::System,
    ];

    pub fn as_str(self) -> &'static str {
        match self {
            Self::Endpoint => "endpoint",
            Self::Model => "model",
            Self::Effort => "effort",
            Self::Thinking => "thinking",
            Self::Tools => "tools",
            Self::System => "system",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub struct WarmthKey(String);

impl WarmthKey {
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Fingerprint {
    key: WarmthKey,
    parts: [String; 6],
}

impl Fingerprint {
    pub fn new(endpoint: &str, bytes: &PrefixBytes) -> Self {
        let parts = [
            endpoint.as_bytes(),
            bytes.model.as_slice(),
            bytes.effort.as_slice(),
            bytes.thinking.as_slice(),
            bytes.tools.as_slice(),
            bytes.system.as_slice(),
        ]
        .map(sha256_bytes_hex);
        let joined: String = Component::ALL
            .iter()
            .zip(&parts)
            .map(|(component, digest)| format!("{}={digest}\n", component.as_str()))
            .collect();
        Self {
            key: WarmthKey(sha256_hex(&joined)),
            parts,
        }
    }

    pub fn key(&self) -> &WarmthKey {
        &self.key
    }

    pub fn part(&self, component: Component) -> &str {
        Component::ALL
            .iter()
            .zip(&self.parts)
            .find_map(|(c, digest)| (*c == component).then_some(digest.as_str()))
            .unwrap_or_default()
    }

    pub fn changed(&self, earlier: &Self) -> Vec<Component> {
        Component::ALL
            .into_iter()
            .filter(|c| self.part(*c) != earlier.part(*c))
            .collect()
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Heat {
    Cold,
    Warming,
    Warm,
}

#[derive(Debug, Clone, Default)]
pub struct WarmthTable {
    table: Arc<Mutex<Table>>,
    changed: Arc<Notify>,
}

#[derive(Debug, Default)]
struct Table {
    next: u64,
    keys: BTreeMap<WarmthKey, Entry>,
}

#[derive(Debug, Clone, Copy)]
struct Entry {
    warming: Option<u64>,
    latest: Instant,
}

impl Entry {
    fn live(&self, now: Instant) -> bool {
        self.warming.is_some() || now.saturating_duration_since(self.latest) < WARM_FOR
    }
}

impl WarmthTable {
    fn table(&self) -> MutexGuard<'_, Table> {
        self.table.lock().unwrap_or_else(PoisonError::into_inner)
    }

    pub fn sent(&self, fingerprint: Fingerprint) -> Ticket {
        let now = Instant::now();
        let mut table = self.live_table(now);
        self.issue(&mut table, fingerprint, now)
    }

    pub fn claim(&self, fingerprint: Fingerprint) -> Result<Ticket, Box<Fingerprint>> {
        let now = Instant::now();
        let mut table = self.live_table(now);
        if table
            .keys
            .get(&fingerprint.key)
            .is_some_and(|e| e.warming.is_some())
        {
            return Err(Box::new(fingerprint));
        }
        Ok(self.issue(&mut table, fingerprint, now))
    }

    pub async fn claim_within(self, fingerprint: Fingerprint, limit: Duration) -> Ticket {
        let deadline = Instant::now() + limit;
        let mut fingerprint = fingerprint;
        loop {
            match self.claim(fingerprint) {
                Ok(ticket) => return ticket,
                Err(back) => fingerprint = *back,
            }
            let left = deadline.saturating_duration_since(Instant::now());
            if left.is_zero() {
                return self.sent(fingerprint);
            }
            self.clone().cooled(fingerprint.key.clone(), left).await;
        }
    }

    fn live_table(&self, now: Instant) -> MutexGuard<'_, Table> {
        let mut table = self.table();
        table.keys.retain(|_, entry| entry.live(now));
        table
    }

    fn issue(&self, table: &mut Table, fingerprint: Fingerprint, now: Instant) -> Ticket {
        let id = table.next;
        table.next = table.next.wrapping_add(1);
        table.keys.entry(fingerprint.key.clone()).or_insert(Entry {
            warming: Some(id),
            latest: now,
        });
        Ticket {
            warmth: self.clone(),
            fingerprint,
            id,
            sent_at: now,
            streamed: false,
        }
    }

    pub fn heat(&self, key: &WarmthKey) -> Heat {
        let now = Instant::now();
        match self.table().keys.get(key) {
            Some(entry) if entry.warming.is_some() => Heat::Warming,
            Some(entry) if entry.live(now) => Heat::Warm,
            _ => Heat::Cold,
        }
    }

    pub async fn cooled(self, key: WarmthKey, limit: Duration) {
        let deadline = Instant::now() + limit;
        loop {
            let notified = self.changed.notified();
            tokio::pin!(notified);
            notified.as_mut().enable();
            if self.heat(&key) != Heat::Warming {
                return;
            }
            if tokio::time::timeout_at(deadline, notified).await.is_err() {
                return;
            }
        }
    }

    #[cfg(test)]
    pub(crate) fn requests(&self) -> u64 {
        self.table().next
    }

    #[cfg(test)]
    pub(crate) fn heats(&self) -> Vec<Heat> {
        let keys: Vec<WarmthKey> = self.table().keys.keys().cloned().collect();
        keys.iter().map(|key| self.heat(key)).collect()
    }

    fn streamed(&self, key: &WarmthKey, sent_at: Instant) {
        let mut table = self.table();
        let entry = table.keys.entry(key.clone()).or_insert(Entry {
            warming: None,
            latest: sent_at,
        });
        entry.warming = None;
        entry.latest = entry.latest.max(sent_at);
        drop(table);
        self.changed.notify_waiters();
    }

    fn failed(&self, key: &WarmthKey, id: u64) {
        let mut table = self.table();
        if table.keys.get(key).is_some_and(|e| e.warming == Some(id)) {
            table.keys.remove(key);
            drop(table);
            self.changed.notify_waiters();
        }
    }
}

#[derive(Debug)]
pub struct Ticket {
    warmth: WarmthTable,
    fingerprint: Fingerprint,
    id: u64,
    sent_at: Instant,
    streamed: bool,
}

impl Ticket {
    pub fn fingerprint(&self) -> &Fingerprint {
        &self.fingerprint
    }

    pub fn streamed(&mut self) {
        if !self.streamed {
            self.streamed = true;
            self.warmth.streamed(&self.fingerprint.key, self.sent_at);
        }
    }
}

impl Drop for Ticket {
    fn drop(&mut self) {
        if !self.streamed {
            self.warmth.failed(&self.fingerprint.key, self.id);
        }
    }
}

#[cfg(test)]
mod tests;
