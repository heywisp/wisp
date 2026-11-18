use std::collections::BTreeSet;
use std::path::PathBuf;

use jiff::{Timestamp, civil::Date};
use tokio::sync::mpsc;
use wisp_protocol::{AccountSpend, AgentEvent, ChatRow, RequestId, SpendDay, SpendWindow};

use super::{Agent, emit};
use crate::resume::chat_rows;

impl Agent {
    fn listing(&self) -> impl FnOnce() -> Vec<ChatRow> + Send + 'static {
        let here = self
            .tools
            .as_ref()
            .map(|env| env.workspace.shorten(&env.workspace.root));
        let dir = self.agents.sessions_dir();
        let (known, marks): (Vec<String>, Vec<_>) = match &self.endpoint_catalog {
            Some(catalog) => {
                let view = catalog.view();
                let known = view
                    .ready
                    .into_iter()
                    .map(|c| c.name)
                    .chain(view.unavailable.into_iter().map(|(name, _)| name))
                    .collect();
                (known, view.endpoints)
            }
            None => (
                self.connections.iter().map(|c| c.name.clone()).collect(),
                Vec::new(),
            ),
        };
        let now = self.clock.now();
        let session_id = self.session_id.clone();
        move || {
            let (Some(here), Some(dir)) = (here, dir) else {
                return Vec::new();
            };
            chat_rows(
                dir,
                &here,
                Some(&session_id),
                now,
                |name| known.iter().any(|k| k == name),
                |name| marks.iter().find(|s| s.name == name).and_then(|s| s.mark),
            )
            .unwrap_or_default()
        }
    }

    pub(super) fn spend_inputs(&self) -> (Option<PathBuf>, Date, BTreeSet<Date>) {
        (
            self.agents.sessions_dir(),
            crate::spend::day(Timestamp::now(), self.clock.as_ref()),
            self.spend_lost.clone(),
        )
    }

    pub(super) fn list_spend(&self, request: RequestId, events: &mpsc::Sender<AgentEvent>) {
        let (dir, today, lost) = self.spend_inputs();
        let events = events.clone();
        tokio::spawn(async move {
            let (days, window) = tokio::task::spawn_blocking(move || {
                dir.map(|dir| crate::spend::days(&dir, today, &lost))
                    .unwrap_or_default()
            })
            .await
            .unwrap_or_else(|_| {
                let total = AccountSpend {
                    lower_bound: true,
                    ..AccountSpend::default()
                };
                (
                    vec![SpendDay {
                        day: today.to_string(),
                        total,
                        ..SpendDay::default()
                    }],
                    SpendWindow {
                        since: Some(today.to_string()),
                        total,
                        ..SpendWindow::default()
                    },
                )
            });
            emit(
                &events,
                AgentEvent::SpendListed {
                    request,
                    days,
                    window,
                },
            )
            .await;
        });
    }

    pub(super) fn list_chats(&self, request: RequestId, events: &mpsc::Sender<AgentEvent>) {
        let rows = self.listing();
        let events = events.clone();
        tokio::spawn(async move {
            let chats = tokio::task::spawn_blocking(rows).await.unwrap_or_default();
            emit(&events, AgentEvent::ChatsListed { request, chats }).await;
        });
    }
}
