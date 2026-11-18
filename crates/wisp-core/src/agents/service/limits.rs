use wisp_protocol::{Activity, AgentEvent, AgentPath, AgentState, HoldReason, QueueReason};

use super::{AgentsHandle, AgentsService, Tree};
use crate::agents::Limits;
use crate::agents::state::{self, Change};

pub(super) fn stoppable(state: AgentState) -> bool {
    state.is_busy()
        || state
            == AgentState::Held {
                reason: HoldReason::Concurrency,
            }
}

fn waits_for_permit(state: AgentState) -> bool {
    matches!(
        state,
        AgentState::Queued {
            reason: QueueReason::Concurrency
        } | AgentState::Held {
            reason: HoldReason::Concurrency
        }
    )
}

impl Tree {
    pub(super) fn remember_before_queue(
        &mut self,
        path: &AgentPath,
        before: AgentState,
        after: AgentState,
    ) {
        let settled = matches!(before, AgentState::Idle { .. } | AgentState::Held { .. });
        if waits_for_permit(after) && !waits_for_permit(before) && settled {
            self.before_queue.insert(path.clone(), before);
        } else if !waits_for_permit(after) {
            self.before_queue.remove(path);
        }
    }

    pub(super) fn running_under(&self, parent: &AgentPath) -> usize {
        self.entries
            .values()
            .filter(|e| e.permit && e.worker && e.path().parent().as_ref() == Some(parent))
            .count()
    }

    fn tree_running(&self) -> usize {
        self.entries
            .values()
            .filter(|e| e.permit && e.worker)
            .count()
    }

    fn has_room(&self, parent: &AgentPath, limits: Limits) -> bool {
        self.running_under(parent) < cap(limits.max_running)
            && self.tree_running() < cap(limits.max_tree_running)
    }

    pub(super) fn admit(
        &mut self,
        parent: &AgentPath,
        path: &AgentPath,
        worker: bool,
        limits: Limits,
    ) -> (bool, AgentState) {
        let spawned = state::next(AgentState::Idle { outcome: None }, Change::Spawned);
        if !self.restarting && (!worker || self.has_room(parent, limits)) {
            return (true, spawned);
        }
        self.waiters.push_back(path.clone());
        let queued = Change::Queued(QueueReason::Concurrency);
        (false, state::next(spawned, queued))
    }

    pub(super) fn grant(&mut self, limits: Limits) {
        if self.restarting {
            return;
        }
        for next in std::mem::take(&mut self.waiters) {
            let waiting = self
                .get(&next)
                .is_some_and(|e| e.state.is_live() && !e.permit);
            let Some(parent) = next.parent().filter(|_| waiting) else {
                continue;
            };
            if self.get(&next).is_some_and(|entry| entry.worker) && !self.has_room(&parent, limits)
            {
                self.waiters.push_back(next);
            } else if let Some(entry) = self.get_mut(&next) {
                entry.permit = true;
                entry.granted.notify_one();
            }
        }
    }
}

pub(super) fn cap(limit: Option<u32>) -> usize {
    limit
        .and_then(|max| usize::try_from(max).ok())
        .unwrap_or(usize::MAX)
}

impl AgentsService {
    fn unqueue(&self, path: &AgentPath) -> Option<AgentEvent> {
        let mut tree = self.tree();
        let path = &tree.resolve(path);
        if tree.waiters.contains(path) {
            return None;
        }
        let restored = tree
            .before_queue
            .remove(path)
            .unwrap_or(AgentState::Idle { outcome: None });
        let entry = tree
            .get_mut(path)
            .filter(|e| !e.permit && waits_for_permit(e.state))?;
        entry.state = restored;
        entry.track_running();
        entry.activity = state::activity_of(restored).unwrap_or(Activity::Idle);
        Some(entry.announce(self.now_ms(), &self.shared.news))
    }

    pub(super) fn ask_permit(&self, path: &AgentPath) -> bool {
        let mut tree = self.tree();
        if tree.closed || tree.restarting {
            return false;
        }
        let path = &tree.resolve(path);
        let Some(parent) = path.parent() else {
            return true;
        };
        let Some(entry) = tree.get(path) else {
            return true;
        };
        if entry.permit {
            return true;
        }
        if !entry.worker || tree.has_room(&parent, self.shared.limits) {
            tree.waiters.retain(|w| w != path);
            if let Some(entry) = tree.get_mut(path) {
                entry.permit = true;
            }
            return true;
        }
        if !tree.waiters.contains(path) {
            tree.waiters.push_back(path.clone());
        }
        false
    }

    pub(super) fn permit_waiting(&self, path: &AgentPath) -> bool {
        let tree = self.tree();
        tree.restarting || tree.waiters.contains(&tree.resolve(path))
    }

    pub(super) fn release_permit(&self, path: &AgentPath, leave: bool) {
        let mut tree = self.tree();
        let path = &tree.resolve(path);
        if leave {
            tree.waiters.retain(|w| w != path);
        }
        let held = tree
            .get_mut(path)
            .map(|e| std::mem::replace(&mut e.permit, false))
            .unwrap_or(false);
        if held {
            tree.grant(self.shared.limits);
        }
    }

    pub(super) fn holds_permit(&self, path: &AgentPath) -> bool {
        self.tree().get(path).is_some_and(|e| e.permit)
    }

    pub(super) fn publish_spend(
        &self,
        path: &AgentPath,
        usd: f64,
        lower_bound: bool,
    ) -> Option<AgentEvent> {
        let mut tree = self.tree();
        let entry = tree.get_mut(path).filter(|e| !e.path().is_root())?;
        entry.spent_usd = usd;
        entry.lower_bound = lower_bound;
        (entry.state.is_live() && entry.unannounced())
            .then(|| entry.announce(self.now_ms(), &self.shared.news))
    }

    pub(super) fn tree_spend(&self) -> Option<(f64, bool)> {
        let tree = self.tree();
        let mut children = tree
            .entries
            .values()
            .filter(|e| !e.path().is_root())
            .peekable();
        children.peek()?;
        Some(children.fold((0.0, false), |(usd, lower), e| {
            (usd + e.spent_usd, lower || e.lower_bound || !e.priced())
        }))
    }
}

impl AgentsHandle {
    pub(crate) fn unqueue(&self) -> Option<AgentEvent> {
        self.service.as_ref().and_then(|s| s.unqueue(&self.path()))
    }

    pub(crate) fn ask_permit(&self) -> bool {
        self.service
            .as_ref()
            .is_none_or(|s| s.ask_permit(&self.path()))
    }

    pub(crate) fn permit_waiting(&self) -> bool {
        self.service
            .as_ref()
            .is_some_and(|s| s.permit_waiting(&self.path()))
    }

    pub(crate) fn holds_permit(&self) -> bool {
        self.service
            .as_ref()
            .is_some_and(|s| s.holds_permit(&self.path()))
    }

    pub(crate) fn release_permit(&self) {
        if let Some(service) = &self.service {
            service.release_permit(&self.path(), false);
        }
    }

    pub(crate) fn leave_pool(&self) {
        if let Some(service) = &self.service {
            service.release_permit(&self.path(), true);
        }
    }

    pub(crate) fn publish_spend(&self, usd: f64, lower_bound: bool) -> Option<AgentEvent> {
        self.service
            .as_ref()
            .and_then(|s| s.publish_spend(&self.path(), usd, lower_bound))
    }

    pub(crate) fn tree_plan_usd(&self) -> Option<f64> {
        self.service.as_ref().and_then(|service| {
            let tree = service.tree();
            let mut children = tree
                .entries
                .values()
                .filter(|entry| !entry.path().is_root())
                .peekable();
            children.peek()?;
            children
                .map(|entry| {
                    entry.plan_usd.or(match entry.billing {
                        wisp_protocol::Billing::Metered => Some(0.0),
                        wisp_protocol::Billing::Mixed => None,
                        _ => Some(entry.spent_usd),
                    })
                })
                .sum()
        })
    }

    pub(crate) fn tree_billing(&self) -> wisp_protocol::Billing {
        self.service
            .as_ref()
            .and_then(|service| {
                service
                    .tree()
                    .entries
                    .values()
                    .filter(|entry| !entry.path().is_root() && entry.spent_usd > 0.0)
                    .map(|entry| entry.billing)
                    .reduce(wisp_protocol::Billing::combined)
            })
            .unwrap_or_default()
    }

    pub(crate) fn tree_spend(&self) -> Option<(f64, bool)> {
        self.service
            .as_ref()
            .filter(|_| self.path().is_root())
            .and_then(AgentsService::tree_spend)
    }
}
