use std::path::PathBuf;

use tokio::sync::mpsc;
use wisp_core::agent::{BriefOwners, send_scrubbed_with_owners, with_scrubbed_link_frames};
use wisp_protocol::link::ToClient;
use wisp_protocol::{AgentEvent, Opening, SessionView};

pub struct Scrub {
    home: Option<PathBuf>,
    owners: BriefOwners,
}

impl Scrub {
    pub(crate) fn new(home: Option<PathBuf>, owners: BriefOwners) -> Self {
        Self { home, owners }
    }

    pub fn write(
        &self,
        event: &AgentEvent,
        mut write: impl FnMut(&AgentEvent) -> Result<(), String>,
    ) -> Result<(), String> {
        let mut frames = [ToClient::event(event.clone())];
        self.frames(&mut frames, |out| {
            out.iter().try_for_each(|frame| match frame {
                ToClient::Event(event) => write(event),
                _ => Ok(()),
            })
        })
        .unwrap_or(Ok(()))
    }

    pub fn frames<R>(
        &self,
        frames: &mut [ToClient],
        publish: impl FnOnce(&[ToClient]) -> R,
    ) -> Option<R> {
        with_scrubbed_link_frames(frames, self.home.as_deref(), Some(&self.owners), publish)
    }

    pub async fn send(&self, to: &mpsc::Sender<AgentEvent>, event: AgentEvent) {
        send_scrubbed_with_owners(to, event, self.home.as_deref(), &self.owners).await;
    }

    pub fn opened_view(&self, opening: &Opening, view: SessionView) -> SessionView {
        let mut frames = [ToClient::Opened {
            opening: Box::new(opening.clone()),
            view,
            now: None,
        }];
        let kept = self.frames(&mut frames, |_| ()).is_some();
        let [ToClient::Opened { mut view, .. }] = frames else {
            return SessionView::default();
        };
        if !kept {
            view.events.clear();
        }
        view
    }
}

#[cfg(feature = "test-support")]
pub mod testing {
    use wisp_protocol::{
        AgentPath, OutputKind, RejectKind, TodoItem, TodoStatus, ToolAction, ToolDisplay,
        ToolPurpose,
    };

    use super::*;

    pub const CANARY: &str = "CANARYOpaqueRegisteredAfterQueued";

    pub fn scrub() -> Scrub {
        scrub_with(None, BriefOwners::default())
    }

    pub fn scrub_with(home: Option<PathBuf>, owners: BriefOwners) -> Scrub {
        Scrub::new(home, owners)
    }

    pub fn queued() -> Vec<AgentEvent> {
        let checklist = AgentEvent::TodosUpdated {
            call_id: Some("t1".into()),
            items: vec![TodoItem {
                text: format!("Draft {CANARY}"),
                section: Some("Next".into()),
                status: TodoStatus::InProgress,
            }],
        };
        let child = AgentEvent::Agent {
            agent: AgentPath::new("/worker"),
            event: Box::new(checklist.clone()),
        };
        let blocked = AgentEvent::Rejected {
            id: None,
            request: None,
            kind: Some(RejectKind::InputBlocked),
            reason: format!("blocked {CANARY}"),
        };
        let call = AgentEvent::ToolCallRequested {
            step: 1,
            block: 0,
            call_id: "c1".into(),
            tool: "bash".into(),
            input: serde_json::json!({"command": "ls", "description": format!("Check {CANARY}")}),
            display: ToolDisplay {
                action: ToolAction::Run,
                verbs: None,
                target: "ls".into(),
                output: OutputKind::Text,
                view: None,
                title: None,
                purpose: ToolPurpose::Run,
            },
            refused: false,
        };
        vec![checklist, child, blocked, call]
    }
}
