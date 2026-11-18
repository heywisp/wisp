use std::collections::{BTreeMap, BTreeSet};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};

use tokio::sync::Notify;
use wisp_protocol::settings::{CredentialKind, names};

use crate::agents;
use crate::settings::{Current, Layers};

pub const REFRESH_RANK: u32 = u32::MAX;

type Place = (u32, u64);

#[derive(Debug, Clone, Default)]
pub struct Lanes {
    table: Arc<Mutex<Table>>,
    changed: Arc<Notify>,
}

#[derive(Debug, Default)]
struct Table {
    default: Option<u32>,
    limits: BTreeMap<String, Option<u32>>,
    lanes: BTreeMap<String, Lane>,
    next: u64,
}

#[derive(Debug, Default)]
struct Lane {
    in_flight: u32,
    waiting: BTreeSet<Place>,
}

impl Table {
    fn limit(&self, endpoint: &str) -> Option<u32> {
        self.limits.get(endpoint).copied().unwrap_or(self.default)
    }

    fn admit(&mut self, endpoint: &str, place: Place) -> Result<(), u32> {
        let limit = self.limit(endpoint);
        let lane = self.lanes.entry(endpoint.to_owned()).or_default();
        let ahead = lane.waiting.range(..place).count();
        let free = limit.map_or(usize::MAX, |l| {
            usize::try_from(l.saturating_sub(lane.in_flight)).unwrap_or(usize::MAX)
        });
        if ahead < free {
            lane.waiting.remove(&place);
            lane.in_flight = lane.in_flight.saturating_add(1);
            Ok(())
        } else {
            Err(u32::try_from(ahead).unwrap_or(u32::MAX))
        }
    }
}

impl Lanes {
    pub fn new(limits: BTreeMap<String, Option<u32>>, default: Option<u32>) -> Self {
        let table = Table {
            default,
            limits,
            ..Table::default()
        };
        Self {
            table: Arc::new(Mutex::new(table)),
            changed: Arc::default(),
        }
    }

    pub fn resolve<'a>(
        layers: &Layers<'_>,
        endpoints: impl IntoIterator<Item = (&'a str, Option<&'a CredentialKind>)>,
        model: &str,
    ) -> Self {
        let limit = |endpoint: Option<&str>, credential| {
            let current = Current {
                endpoint: endpoint.map(str::to_owned),
                model: model.to_owned(),
            };
            agents::limit(names::MAX_IN_FLIGHT, layers, &current, credential).value
        };
        let limits = endpoints
            .into_iter()
            .map(|(name, credential)| (name.to_owned(), limit(Some(name), credential)))
            .collect();
        Self::new(limits, limit(None, None))
    }

    fn table(&self) -> MutexGuard<'_, Table> {
        self.table.lock().unwrap_or_else(PoisonError::into_inner)
    }

    pub fn limit(&self, endpoint: &str) -> Option<u32> {
        self.table().limit(endpoint)
    }

    pub fn enter(&self, endpoint: &str, depth: u32) -> Result<Pass, Waiter> {
        let mut table = self.table();
        if table.limit(endpoint).is_none() {
            return Ok(Pass {
                lane: None,
                generation: None,
            });
        }
        let place = (depth, table.next);
        table.next = table.next.wrapping_add(1);
        table
            .lanes
            .entry(endpoint.to_owned())
            .or_default()
            .waiting
            .insert(place);
        let admitted = table.admit(endpoint, place);
        drop(table);
        let waiter = Waiter {
            lanes: self.clone(),
            endpoint: endpoint.to_owned(),
            place,
            waiting: admitted.is_err(),
        };
        match admitted {
            Ok(()) => Ok(waiter.lane_pass()),
            Err(_) => {
                self.changed.notify_waiters();
                Err(waiter)
            }
        }
    }

    pub async fn pass(&self, endpoint: &str, depth: u32) -> Pass {
        let mut waiter = match self.enter(endpoint, depth) {
            Ok(pass) => return pass,
            Err(waiter) => waiter,
        };
        let mut shown = waiter.ahead();
        loop {
            match waiter.admitted(shown).await {
                Ok(pass) => return pass,
                Err(ahead) => shown = ahead,
            }
        }
    }

    #[cfg(test)]
    pub(crate) fn in_flight(&self, endpoint: &str) -> u32 {
        self.table().lanes.get(endpoint).map_or(0, |l| l.in_flight)
    }

    #[cfg(test)]
    pub(crate) fn waiting(&self, endpoint: &str) -> usize {
        self.table()
            .lanes
            .get(endpoint)
            .map_or(0, |l| l.waiting.len())
    }
}

#[derive(Debug)]
pub struct Waiter {
    lanes: Lanes,
    endpoint: String,
    place: Place,
    waiting: bool,
}

impl Waiter {
    pub fn ahead(&self) -> u32 {
        let table = self.lanes.table();
        let ahead = table
            .lanes
            .get(&self.endpoint)
            .map_or(0, |lane| lane.waiting.range(..self.place).count());
        u32::try_from(ahead).unwrap_or(u32::MAX)
    }

    pub async fn admitted(&mut self, shown: u32) -> Result<Pass, u32> {
        loop {
            let notified = self.lanes.changed.notified();
            tokio::pin!(notified);
            notified.as_mut().enable();
            let admitted = self.lanes.table().admit(&self.endpoint, self.place);
            match admitted {
                Ok(()) => {
                    self.waiting = false;
                    self.lanes.changed.notify_waiters();
                    return Ok(self.lane_pass());
                }
                Err(ahead) if ahead != shown => return Err(ahead),
                Err(_) => notified.await,
            }
        }
    }

    fn lane_pass(&self) -> Pass {
        Pass {
            lane: Some((self.lanes.clone(), self.endpoint.clone())),
            generation: None,
        }
    }
}

impl Drop for Waiter {
    fn drop(&mut self) {
        if !self.waiting {
            return;
        }
        let mut table = self.lanes.table();
        if let Some(lane) = table.lanes.get_mut(&self.endpoint) {
            lane.waiting.remove(&self.place);
        }
        drop(table);
        self.lanes.changed.notify_waiters();
    }
}

#[derive(Debug)]
#[must_use]
pub struct Pass {
    generation: Option<crate::endpoints::generation::Admission>,
    lane: Option<(Lanes, String)>,
}

impl Pass {
    pub(crate) fn with_generation(
        mut self,
        generation: Option<crate::endpoints::generation::Admission>,
    ) -> Self {
        self.generation = generation;
        self
    }
}

impl Drop for Pass {
    fn drop(&mut self) {
        let Some((lanes, endpoint)) = self.lane.take() else {
            return;
        };
        let mut table = lanes.table();
        if let Some(lane) = table.lanes.get_mut(&endpoint) {
            lane.in_flight = lane.in_flight.saturating_sub(1);
        }
        drop(table);
        lanes.changed.notify_waiters();
    }
}

#[cfg(test)]
mod tests;
