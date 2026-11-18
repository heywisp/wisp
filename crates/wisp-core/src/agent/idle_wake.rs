use std::time::Duration;

use tokio::sync::mpsc;
use tokio::time::Instant;
use wisp_protocol::agents::IDLE_WAKE;
use wisp_protocol::{AgentEvent, MailKind, TodoItem, todos};

use super::Agent;

const WALL_RECHECK: i64 = 60;

#[derive(Default)]
pub(super) struct IdleWake {
    pub(super) since: Option<Instant>,
    posted: Option<(Instant, Vec<TodoItem>)>,
}

impl Agent {
    pub(super) fn idle_wake_at(&self) -> Option<Instant> {
        let delay = self.agents.wake_when_stuck()?;
        if !self.path().is_root() || !self.agents.in_tree() || !todos::open(&self.todos) {
            return None;
        }
        let recheck = Some(Instant::now() + delay);
        let Some(idle) = self.agents.idle_since() else {
            return recheck;
        };
        let armed = self
            .idle_wake
            .posted
            .as_ref()
            .is_none_or(|(at, items)| idle > *at || *items != self.todos);
        if !armed
            || !self.queue.is_empty()
            || self.queue.retry.is_some()
            || !self.binding_ready(self.next_endpoint())
            || self.agents.restarting()
            || self.session_hold().is_some()
        {
            return recheck;
        }
        Some(idle.max(self.idle_wake.since.unwrap_or(idle)) + delay)
    }

    pub(super) fn idle_woke(&mut self) {
        let now = Instant::now();
        if self.idle_wake_at().is_some_and(|at| at <= now)
            && self
                .agents
                .mail_to(&self.path(), MailKind::Notice, true, IDLE_WAKE.into())
        {
            self.idle_wake.posted = Some((now, self.todos.clone()));
        }
    }

    pub(super) fn reset_retry_at(&self) -> Option<Instant> {
        let offered = self.offered.as_ref().filter(|_| self.offer_stands())?;
        let at = offered.offer.retries_at?;
        if !self.queue.is_empty()
            || self.queue.retry.is_some()
            || self.next_endpoint() != offered.endpoint
        {
            return None;
        }
        let wait = at.saturating_sub(self.clock.now().as_second());
        let wait = u64::try_from(wait.min(WALL_RECHECK)).unwrap_or(0);
        Some(Instant::now() + Duration::from_secs(wait))
    }

    pub(super) async fn reset_retried(&mut self, events: &mpsc::Sender<AgentEvent>) -> bool {
        if self.reset_retry_at().is_none_or(|at| at > Instant::now()) {
            return false;
        }
        let Some(offered) = &mut self.offered else {
            return false;
        };
        offered.offer.retries_at = None;
        let turn = offered.turn;
        self.retry_command(None, turn, None, None, false, events)
            .await
    }
}
