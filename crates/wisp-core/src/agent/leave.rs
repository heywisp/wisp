use tokio::sync::{mpsc, oneshot};
use wisp_protocol::subscription_auth::Followed;
use wisp_protocol::{AgentEvent, ClientCommand};

use super::Agent;

#[derive(Debug)]
pub(crate) enum Left {
    Busy,
    Moved(Followed),
    Stayed,
}

#[derive(Debug)]
pub(crate) struct Leave {
    pub(crate) to: Option<String>,
    pub(crate) gone: String,
    pub(crate) kept: Option<Vec<String>>,
    pub(crate) reply: oneshot::Sender<Left>,
}

#[derive(Debug)]
pub(super) struct Kept {
    to: Option<String>,
    gone: String,
    names: Vec<String>,
}

pub(crate) type LeaveSender = mpsc::UnboundedSender<Leave>;

pub(super) async fn asked(leaves: &mut mpsc::UnboundedReceiver<Leave>) -> Leave {
    match leaves.recv().await {
        Some(leave) => leave,
        None => std::future::pending().await,
    }
}

impl Agent {
    pub(crate) fn leave_sender(&self) -> LeaveSender {
        self.leaves.0.clone()
    }

    pub(super) fn busy(&self) -> bool {
        self.track.is_some() || self.hooks.pre_turn || self.compacting()
    }

    pub(super) async fn take_leave(&mut self, leave: Leave, events: &mpsc::Sender<AgentEvent>) {
        if leave.reply.is_closed() || self.busy() {
            if let Some(names) = leave.kept {
                self.kept_leave = Some(Kept {
                    to: leave.to,
                    gone: leave.gone,
                    names,
                });
            }
            let _ = leave.reply.send(Left::Busy);
            return;
        }
        let left = {
            match self.leave(leave.to.as_deref(), &leave.gone, events).await {
                Some(moved) => Left::Moved(moved),
                None => Left::Stayed,
            }
        };
        let _ = leave.reply.send(left);
    }

    pub(super) async fn take_kept(&mut self, events: &mpsc::Sender<AgentEvent>) {
        let Some(kept) = self.kept_leave.take() else {
            return;
        };
        if let Some(followed) = self.leave(kept.to.as_deref(), &kept.gone, events).await {
            let event = AgentEvent::AccountsGone {
                names: kept.names,
                moved: vec![followed],
                stay: Vec::new(),
                ways: Vec::new(),
            };
            super::emit(events, event).await;
        }
    }

    pub(super) async fn leave(
        &mut self,
        to: Option<&str>,
        gone: &str,
        events: &mpsc::Sender<AgentEvent>,
    ) -> Option<Followed> {
        self.disarm_keep_warm();
        let target = to.and_then(|to| self.connections.iter().position(|c| c.name == to));
        let moved = match target {
            Some(index) => {
                let changes = ClientCommand::shortcut_changes(
                    Some(self.connections[index].name.clone()),
                    None,
                    None,
                );
                let projected = self.projected();
                match self.verdict(&changes, false, &projected) {
                    Ok(verdict) => {
                        self.apply_verdict(None, verdict, None, events).await;
                        self.apply_pending_binding().ok().map(|()| Followed {
                            agent: self.path().as_str().trim_start_matches('/').to_owned(),
                            model: self.session.model.clone(),
                            effort: self.session.effort,
                            rebuild_usd: self.rewrite_usd(self.fill_estimate()),
                            from: None,
                            to: None,
                        })
                    }
                    Err(_) => None,
                }
            }
            None => None,
        };
        if moved.is_none() {
            self.endpoint_binding = None;
            self.pending_endpoint_binding = None;
            self.settings.pending = None;
            self.binding_fault = Some(if self.path().is_root() {
                super::NO_ACCOUNT_LEFT.to_owned()
            } else {
                crate::agents::endpoints::gone_text(gone)
            });
        }
        self.settle_entry();
        moved
    }
}
