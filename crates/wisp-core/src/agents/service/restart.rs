use std::pin::Pin;

use tokio::sync::oneshot;
use wisp_protocol::{AgentEvent, RestartState, TurnOrigin, Unfinished, Unready};

use super::{AgentsHandle, AgentsService, Tree};
use crate::agents::state::Work;

impl Tree {
    fn rows(&self) -> Vec<(wisp_protocol::AgentPath, Work)> {
        let mut rows = Vec::new();
        for (path, entry) in &self.entries {
            rows.extend(entry.watch.unfinished().into_iter().map(|mut work| {
                work.opening &= path.is_root();
                (path.clone(), work)
            }));
            for process in entry.processes.handles() {
                if process.state.log().is_some() || process.state.live().end.is_some() {
                    continue;
                }
                rows.push((
                    path.clone(),
                    Work {
                        what: Unfinished::Process,
                        title: entry.watch.process_title(process.id),
                        target: process.state.target().to_owned(),
                        started: process.state.started(),
                        tie: (String::new(), process.id),
                        opening: false,
                    },
                ));
            }
        }
        rows.sort_by(|(a, x), (b, y)| {
            (a, x.what, &x.target, x.started, &x.tie)
                .cmp(&(b, y.what, &y.target, y.started, &y.tie))
        });
        rows
    }
}

impl AgentsService {
    fn get_ready(&self, on: bool) -> Option<AgentEvent> {
        let mut tree = self.tree();
        if tree.restarting == on {
            return None;
        }
        tree.restarting = on;
        tree.restart_pending = on;
        tree.restart_rows = None;
        tree.resumed.notify_waiters();
        let state = if on {
            RestartState::GettingReady
        } else {
            tree.grant(self.shared.limits);
            RestartState::Cancelled
        };
        Some(AgentEvent::Restart {
            state,
            open: Vec::new(),
        })
    }

    pub(super) fn report(&self) -> Option<AgentEvent> {
        let mut tree = self.tree();
        if !tree.restarting || tree.restart_pending {
            return None;
        }
        let rows = tree.rows();
        if tree.restart_rows.as_ref() == Some(&rows) {
            return None;
        }
        let state = if rows.is_empty() {
            RestartState::Ready
        } else {
            RestartState::GettingReady
        };
        let open = rows
            .iter()
            .map(|(agent, work)| Unready {
                agent: agent.clone(),
                what: work.what,
                title: work.title.clone(),
                target: work.target.clone(),
                running_ms: u64::try_from(work.started.elapsed().as_millis()).unwrap_or(u64::MAX),
                opening: work.opening,
            })
            .collect();
        tree.restart_rows = Some(rows);
        Some(AgentEvent::Restart { state, open })
    }
}

impl AgentsHandle {
    pub(crate) fn get_ready(&self, on: bool) -> Option<AgentEvent> {
        self.service
            .as_ref()
            .filter(|_| self.path().is_root())?
            .get_ready(on)
    }

    pub(crate) fn restarting(&self) -> bool {
        self.service.as_ref().is_some_and(|s| s.tree().restarting)
    }

    pub(crate) fn restart_changed(&self) -> Pin<Box<impl Future<Output = ()> + 'static>> {
        let mut changed = self
            .service
            .as_ref()
            .map(|service| Box::pin(service.tree().resumed.clone().notified_owned()));
        if let Some(changed) = &mut changed {
            changed.as_mut().enable();
        }
        Box::pin(async move {
            match changed {
                Some(changed) => changed.await,
                None => std::future::pending().await,
            }
        })
    }

    pub(crate) async fn resumed(&self) {
        self.restart_until(false).await;
    }

    pub(crate) async fn starting(&self) {
        self.restart_until(true).await;
    }

    async fn restart_until(&self, on: bool) {
        loop {
            let changed = self.restart_changed();
            if self.restarting() == on {
                return;
            }
            changed.await;
        }
    }

    pub(crate) fn preparing(&self, turn: u64, origin: Option<TurnOrigin>) -> Preparing {
        if let Some(service) = &self.service {
            service.with_entry(&self.path(), |entry| entry.watch.preparing(turn, origin));
        }
        Preparing {
            agents: self.clone(),
            turn,
        }
    }

    pub(crate) fn parking(&self) -> Option<oneshot::Receiver<()>> {
        self.service
            .as_ref()?
            .with_entry(&self.path(), |entry| entry.watch.parking())
    }

    pub(crate) fn requesting(&self) {
        if let Some(service) = &self.service {
            service.with_entry(&self.path(), |entry| entry.watch.requesting());
        }
    }
}

pub(crate) struct Preparing {
    agents: AgentsHandle,
    turn: u64,
}

impl Drop for Preparing {
    fn drop(&mut self) {
        if let Some(service) = &self.agents.service {
            service.with_entry(&self.agents.path(), |entry| entry.watch.prepared(self.turn));
            service.shared.recount.notify_one();
        }
    }
}
