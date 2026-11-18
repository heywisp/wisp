use futures::FutureExt;
use futures::future::BoxFuture;
use tokio::sync::Semaphore;
use wisp_protocol::settings::{FileState, SettingChange, SettingValue};

use super::*;
use crate::config_file::SiteEdit;
use crate::settings::host::{HostError, SavedFile, SavedRead, SettingsHost, WrittenSite};

struct Gated(Semaphore);

impl Gated {
    fn new() -> Arc<Self> {
        Arc::new(Self(Semaphore::new(0)))
    }

    fn open(&self) {
        self.0.add_permits(Semaphore::MAX_PERMITS / 2);
    }
}

impl SettingsHost for Gated {
    fn save(&self, edits: Vec<SiteEdit>) -> BoxFuture<'_, Result<SavedFile, HostError>> {
        async move {
            drop(self.0.acquire().await);
            Ok(SavedFile {
                path_display: "config.toml".into(),
                written: edits
                    .iter()
                    .map(|edit| WrittenSite {
                        path: edit.path().clone(),
                        value: match edit {
                            SiteEdit::Set(_, value) => Some(value.clone()),
                            SiteEdit::Unset(_) => None,
                        },
                        line: Some(1),
                    })
                    .collect(),
                notes: Vec::new(),
            })
        }
        .boxed()
    }

    fn read(&self) -> BoxFuture<'_, Result<SavedRead, HostError>> {
        async move {
            Ok(SavedRead {
                often: Vec::new(),
                saved: Vec::new(),
                file: FileState::Missing,
            })
        }
        .boxed()
    }
}

fn save_effort(request: u64) -> ClientCommand {
    ClientCommand::ChangeSettings {
        request: RequestId(request),
        changes: vec![SettingChange::named("effort", SettingValue::Text("high".into())).unwrap()],
        save: true,
    }
}

fn gated(plan: Vec<Step>) -> (Rig, Arc<Gated>) {
    let host = Gated::new();
    let mut setup = Setup::new(plan);
    setup.host = Some(host.clone());
    (setup.rig(), host)
}

fn answered(events: &[AgentEvent]) -> Vec<u64> {
    events
        .iter()
        .filter_map(|e| match e {
            AgentEvent::SettingsChanged {
                answers: Some(r), ..
            }
            | AgentEvent::Rejected {
                request: Some(r), ..
            } => Some(r.0),
            AgentEvent::TurnStarted {
                answers: Some(r), ..
            } => Some(r.0),
            _ => None,
        })
        .collect()
}

#[tokio::test]
async fn a_retry_behind_a_held_save_is_judged_after_it_and_echoes_its_request() {
    let (mut rig, host) = gated(vec![failed(ErrorKind::Server), done("ok")]);
    rig.run(vec![ClientCommand::send("tui-1", "go")], 1).await;
    let waiting = rig.idle(vec![save_effort(1), retry_turn(1, None, 2)]).await;
    assert!(started(&waiting).is_empty());
    host.open();
    let events = rig.run(Vec::new(), 1).await;
    assert_eq!(answered(&events), [1, 2]);
    assert_eq!(started(&events), [(2, Some(1), Some(RequestId(2)))]);
    let bodies = rig.a.texts();
    assert_eq!(
        bodies[1], bodies[0],
        "the saved effort waits for the next message"
    );
}

#[tokio::test]
async fn a_message_sent_before_the_verdict_cancels_the_retry_and_starts_alone() {
    let (mut rig, host) = gated(vec![failed(ErrorKind::Server), done("ok")]);
    rig.run(vec![ClientCommand::send("tui-1", "go")], 1).await;
    rig.idle(vec![
        save_effort(1),
        retry_turn(1, None, 2),
        ClientCommand::send("tui-2", "never mind"),
    ])
    .await;
    host.open();
    let events = rig.run(Vec::new(), 1).await;
    assert_eq!(
        refusals(&events),
        [(
            RequestId(2),
            "a message was sent after the failure".to_owned()
        )]
    );
    assert_eq!(started(&events), [(2, None, None)]);
    assert_eq!(delivered(&events), [(ids(&["tui-2"]), 1)]);
}

#[tokio::test]
async fn an_idle_release_before_the_verdict_cancels_the_retry() {
    let (mut rig, host) = gated(vec![
        uses(&[tool_call()]).then(ClientCommand::queue("tui-2", "later")),
        failed(ErrorKind::Server),
        done("released"),
    ]);
    rig.run(vec![ClientCommand::send("tui-1", "go")], 1).await;
    rig.idle(vec![
        save_effort(1),
        retry_turn(1, None, 2),
        ClientCommand::Release,
    ])
    .await;
    host.open();
    let events = rig.run(Vec::new(), 1).await;
    assert_eq!(
        refusals(&events),
        [(
            RequestId(2),
            "a message was sent after the failure".to_owned()
        )]
    );
    assert_eq!(started(&events), [(2, None, None)]);
}

#[tokio::test]
async fn an_idle_cancel_answers_a_waiting_retry() {
    let (mut rig, host) = gated(vec![failed(ErrorKind::Server), done("ok")]);
    rig.run(vec![ClientCommand::send("tui-1", "go")], 1).await;
    rig.idle(vec![save_effort(1), retry_turn(1, None, 2)]).await;
    let events = rig.idle(vec![ClientCommand::Cancel]).await;
    assert_eq!(
        refusals(&events),
        [(RequestId(2), "cancelled before it started".to_owned())]
    );
    host.open();
    let events = rig.idle(Vec::new()).await;
    assert_eq!(answered(&events), [1]);
    assert!(started(&events).is_empty());
    let events = rig.run(vec![retry_turn(1, None, 3)], 1).await;
    assert_eq!(started(&events), [(2, Some(1), Some(RequestId(3)))]);
}

#[tokio::test]
async fn closing_the_channel_answers_a_waiting_retry() {
    let (mut rig, host) = gated(vec![failed(ErrorKind::Server)]);
    rig.run(vec![ClientCommand::send("tui-1", "go")], 1).await;
    rig.idle(vec![save_effort(1), retry_turn(1, None, 2)]).await;
    host.open();
    let (tx, _) = mpsc::channel(1);
    for fake in [&rig.a, &rig.b] {
        fake.send_to(tx.clone());
    }
    drop(std::mem::replace(&mut rig.tx, tx));
    let events = rig.drive(Vec::new(), |_| false, PATIENT).await;
    assert_eq!(answered(&events), [1, 2]);
    assert_eq!(
        refusals(&events),
        [(RequestId(2), "the session ended".to_owned())]
    );
    assert!(started(&events).is_empty());
}

#[tokio::test]
async fn an_endpoint_change_queued_behind_a_waiting_retry_goes_to_the_next_turn() {
    let host = Gated::new();
    let mut setup =
        Setup::new(vec![failed(ErrorKind::Server), done("retried")]).with_b(vec![done("next")]);
    setup.host = Some(host.clone());
    let mut rig = setup.rig();
    rig.run(vec![ClientCommand::send("tui-1", "go")], 1).await;
    let move_to_b = ClientCommand::shortcut(RequestId(3), Some(B.into()), None, None);
    rig.idle(vec![save_effort(1), retry_turn(1, None, 2), move_to_b])
        .await;
    host.open();
    let mut events = rig.run(Vec::new(), 1).await;
    events.extend(rig.idle(Vec::new()).await);
    assert_eq!(answered(&events), [1, 2, 3]);
    assert_eq!(started(&events), [(2, Some(1), Some(RequestId(2)))]);
    assert_eq!((rig.a.texts().len(), rig.b.texts().len()), (2, 0));
    assert_eq!(rig.agent.endpoint_name(), A);
    assert_eq!(
        rig.agent.pending_settings().map(|(endpoint, ..)| endpoint),
        Some(B)
    );
    rig.run(vec![ClientCommand::send("tui-2", "continue")], 1)
        .await;
    assert_eq!((rig.a.texts().len(), rig.b.texts().len()), (2, 1));
}
