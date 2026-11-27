use futures::FutureExt;
use futures::future::BoxFuture;
use tokio::sync::{Semaphore, watch};
use wisp_protocol::endpoint::{
    CheckKind, CheckResult, EndpointSpec, NotSavedReason, StoreChoice, StoreKind,
};
use wisp_protocol::{DeliveryMode, InputId, RejectKind, RequestId, Secret};

use super::*;
use crate::agent::queue::EndpointSave;
use crate::endpoints::host::{
    EndpointHost, HostError, NotSaved, Reconnected, SAVE_WORST, SaveOutcome, Saved,
};
use crate::session::MemoryStore;

const KEY: &str = "sk-ant-CANARYKEY0001";
const URL_SECRET: &str = "CANARYPATHTOKEN";
const NOTE: &str = "note: credentials.json is in ~/dotfiles/wisp, inside the git repository";

struct Host {
    gate: Semaphore,
    outcome: Mutex<Vec<Result<SaveOutcome, HostError>>>,
    reconnected: Mutex<Option<Result<Reconnected, HostError>>>,
    saves: Mutex<Vec<(EndpointSpec, bool)>>,
    keys: Mutex<Vec<Secret>>,
}

impl Host {
    fn new(
        outcome: Result<SaveOutcome, HostError>,
        reconnected: Option<Result<Reconnected, HostError>>,
    ) -> Arc<Self> {
        Self::many(vec![outcome], reconnected)
    }

    fn many(
        outcomes: Vec<Result<SaveOutcome, HostError>>,
        reconnected: Option<Result<Reconnected, HostError>>,
    ) -> Arc<Self> {
        Arc::new(Self {
            gate: Semaphore::new(0),
            outcome: Mutex::new(outcomes.into_iter().rev().collect()),
            reconnected: Mutex::new(reconnected),
            saves: Mutex::new(Vec::new()),
            keys: Mutex::new(Vec::new()),
        })
    }

    fn open(&self) {
        if self.gate.available_permits() == 0 {
            self.gate.add_permits(Semaphore::MAX_PERMITS / 2);
        }
    }

    fn saves(&self) -> Vec<(EndpointSpec, bool)> {
        self.saves.lock().unwrap().clone()
    }
}

impl EndpointHost for Host {
    fn save(
        &self,
        spec: EndpointSpec,
        key: Secret,
        check: bool,
        cancel: watch::Receiver<bool>,
    ) -> BoxFuture<'_, Result<SaveOutcome, HostError>> {
        async move {
            assert!(!*cancel.borrow());
            drop(self.gate.acquire().await);
            self.saves.lock().unwrap().push((spec, check));
            self.keys.lock().unwrap().push(key);
            let mut outcomes = self.outcome.lock().unwrap();
            if outcomes.len() > 1 {
                outcomes.pop().unwrap()
            } else {
                outcomes[0].clone()
            }
        }
        .boxed()
    }

    fn edit(
        &self,
        _name: String,
        _edit: wisp_protocol::endpoint::EndpointEdit,
        _store: Option<StoreChoice>,
        _check: bool,
    ) -> BoxFuture<'_, Result<crate::endpoints::host::EditOutcome, HostError>> {
        async { Err(HostError("not scripted".into())) }.boxed()
    }

    fn remove(
        &self,
        _name: String,
        _keep_key: bool,
    ) -> BoxFuture<'_, Result<crate::endpoints::host::RemoveOutcome, HostError>> {
        async { Err(HostError("not scripted".into())) }.boxed()
    }

    fn connections(&self) -> BoxFuture<'_, Result<Reconnected, HostError>> {
        async move {
            self.reconnected
                .lock()
                .unwrap()
                .clone()
                .unwrap_or_else(|| Err(HostError("no answer scripted".into())))
        }
        .boxed()
    }
}

fn connection(name: &str, provider: Arc<Fake>, secrets: &[&str]) -> Connection {
    Connection {
        name: name.into(),
        provider,
        context: LogContext {
            endpoint: format!("https://{name}.example.com"),
            endpoint_name: name.into(),
            credential_source: CredentialSource::Keyring,
            redactions: Redactions::new(secrets.iter().map(|s| (*s).to_owned())),
        },
        priced: true,
    }
}

fn spec(name: &str) -> EndpointSpec {
    EndpointSpec {
        name: name.into(),
        provider: "anthropic".into(),
        auth: "bearer".into(),
        url: Some(Secret::new(format!(
            "https://work.example.com/{URL_SECRET}"
        ))),
        store: StoreChoice::Auto,
        default: false,
    }
}

fn save(request: u64, name: &str) -> ClientCommand {
    ClientCommand::SaveEndpoint {
        request: RequestId(request),
        endpoint: spec(name),
        key: Secret::new(KEY),
        check: true,
    }
}

fn saved_work() -> Result<SaveOutcome, HostError> {
    Ok(SaveOutcome::Saved(Saved {
        name: "work".into(),
        store: StoreKind::Keyring,
        check: CheckResult::of(CheckKind::Ok),
        notes: vec![NOTE.into()],
    }))
}

fn with_work(work: Arc<Fake>) -> Option<Result<Reconnected, HostError>> {
    Some(Ok(Reconnected {
        ready: vec![
            connection("direct", scripted(Vec::new()), &[]),
            connection("work", work, &[KEY, URL_SECRET]),
        ],
        unavailable: vec![("old".into(), "its key is missing".into())],
        endpoints: vec![summary("direct", true), summary("work", false)],
    }))
}

fn two_sided(direct: Arc<Fake>, env: &Env, store: &MemoryStore) -> Agent {
    logged(direct.clone(), env, store).with_connections(
        vec![connection("direct", direct, &[])],
        Vec::new(),
        "direct",
    )
}

fn drain(rx: &mut mpsc::Receiver<AgentEvent>) -> Vec<AgentEvent> {
    let mut seen = Vec::new();
    while let Ok(event) = rx.try_recv() {
        seen.push(event);
    }
    seen
}

async fn queue_save(agent: &mut Agent, command: ClientCommand, host: &Host) -> Vec<AgentEvent> {
    let (tx, mut rx) = mpsc::channel(1024);
    let ClientCommand::SaveEndpoint {
        request,
        endpoint,
        key,
        check,
    } = command
    else {
        panic!("not a save");
    };
    let save = EndpointSave {
        request,
        endpoint,
        key,
        check,
    };
    agent.enqueue(Queued::Save(save), &tx).await;
    host.open();
    agent.settle_queue(&tx).await;
    drop(tx);
    drain(&mut rx)
}

async fn switch(agent: &mut Agent, name: &str) -> Vec<AgentEvent> {
    let (tx, mut rx) = mpsc::channel(1024);
    let queued = Queued::Change {
        request: Some(RequestId(90)),
        changes: ClientCommand::shortcut_changes(Some(name.into()), None, None),
        save: false,
        projected: crate::agent::settings::Projected::default(),
        by: None,
        answer: None,
    };
    agent.enqueue(queued, &tx).await;
    agent.settle_queue(&tx).await;
    drop(tx);
    drain(&mut rx)
}

fn no_canary(text: &str) {
    for canary in ["CANARYKEY", URL_SECRET] {
        assert!(!text.contains(canary), "{text}");
    }
}

#[tokio::test]
async fn a_save_announces_the_new_set_then_answers_and_logs_neither_notes_nor_secrets() {
    let env = Env::new("endpoint-save");
    let store = MemoryStore::default();
    let host = Host::new(saved_work(), with_work(scripted(Vec::new())));
    let mut agent =
        two_sided(scripted(vec![done("hi")]), &env, &store).with_endpoint_host(host.clone());
    let (_, first) = turn_with(&mut agent, "hello", None).await;
    no_canary(&serde_json::to_string(&first).unwrap());
    let events = queue_save(&mut agent, save(7, "work"), &host).await;
    let [
        AgentEvent::EndpointsChanged { endpoints },
        AgentEvent::EndpointSaved {
            answers,
            name,
            store: kind,
            check,
            notes,
        },
    ] = events.as_slice()
    else {
        panic!("{events:?}");
    };
    assert_eq!(*answers, RequestId(7));
    assert_eq!(name, "work");
    assert_eq!(*kind, StoreKind::Keyring);
    assert_eq!(check.outcome, CheckKind::Ok);
    assert_eq!(notes, &[NOTE.to_owned()]);
    assert_eq!(
        endpoints,
        &[summary("direct", true), summary("work", false)]
    );
    assert_eq!(host.saves(), [(spec("work"), true)]);
    assert_eq!(host.keys.lock().unwrap()[0].expose(), KEY);
    let names: Vec<&str> = agent.connections.iter().map(|c| c.name.as_str()).collect();
    assert_eq!(names, ["direct", "work"]);
    assert_eq!(agent.endpoint_name(), "direct");
    assert_eq!(agent.settings.layers.endpoints, *endpoints);
    no_canary(&serde_json::to_string(&events).unwrap());
    let records = log_events(&store);
    let tail: Vec<&LogEvent> = records
        .iter()
        .filter(|r| {
            matches!(
                r,
                LogEvent::EndpointSaved { .. } | LogEvent::EndpointsChanged { .. }
            )
        })
        .collect();
    let [
        LogEvent::EndpointsChanged { turn: None, .. },
        LogEvent::EndpointSaved {
            turn: None,
            name,
            store: StoreKind::Keyring,
            ..
        },
    ] = tail.as_slice()
    else {
        panic!("{tail:?}");
    };
    assert_eq!(name, "work");
    let log = serde_json::to_string(&test_guard::records(&store)).unwrap();
    assert!(!log.contains("dotfiles"), "{log}");
    no_canary(&log);
}

#[tokio::test]
async fn a_save_never_changes_the_next_request_on_the_current_endpoint() {
    let control = {
        let env = Env::new("endpoint-control");
        let store = MemoryStore::default();
        let direct = scripted(vec![done("one"), done("two")]);
        let mut agent = two_sided(direct.clone(), &env, &store);
        turn_with(&mut agent, "first", None).await;
        let parts = agent.request_parts().1;
        turn_with(&mut agent, "second", None).await;
        (direct.texts(), parts)
    };
    let env = Env::new("endpoint-saved");
    let store = MemoryStore::default();
    let direct = scripted(vec![done("one"), done("two")]);
    let host = Host::new(saved_work(), with_work(scripted(Vec::new())));
    let mut agent = two_sided(direct.clone(), &env, &store).with_endpoint_host(host.clone());
    turn_with(&mut agent, "first", None).await;
    let before = agent.settings.binding_controls;
    queue_save(&mut agent, save(1, "work"), &host).await;
    assert_eq!(agent.settings.binding_controls, before);
    assert!(!agent.settings.binding_controls);
    assert_eq!(agent.request_parts().1, control.1);
    turn_with(&mut agent, "second", None).await;
    assert_eq!(direct.texts(), control.0);
}

#[tokio::test]
async fn the_first_switch_to_a_saved_endpoint_turns_binding_controls_on() {
    let env = Env::new("endpoint-switch");
    let store = MemoryStore::default();
    let work = scripted(vec![done("there")]);
    let host = Host::new(saved_work(), with_work(work.clone()));
    let direct = scripted(vec![done("one")]);
    let mut agent = two_sided(direct.clone(), &env, &store).with_endpoint_host(host.clone());
    turn_with(&mut agent, "first", None).await;
    queue_save(&mut agent, save(1, "work"), &host).await;
    let changed = switch(&mut agent, "work").await;
    assert!(
        changed
            .iter()
            .any(|e| matches!(e, AgentEvent::SettingsChanged { .. })),
        "{changed:?}"
    );
    assert!(!agent.settings.binding_controls);
    turn_with(&mut agent, "second", None).await;
    assert!(agent.settings.binding_controls);
    assert_eq!(work.sent().len(), 1);
    assert_eq!(direct.sent().len(), 1);
    assert_eq!(agent.endpoint_name(), "work");
}

#[tokio::test]
async fn a_save_before_the_first_request_decides_binding_controls_as_a_start_would() {
    let env = Env::new("endpoint-early");
    let store = MemoryStore::default();
    let host = Host::new(saved_work(), with_work(scripted(Vec::new())));
    let mut agent = two_sided(scripted(Vec::new()), &env, &store).with_endpoint_host(host.clone());
    assert!(!agent.settings.binding_controls);
    queue_save(&mut agent, save(1, "work"), &host).await;
    assert!(agent.settings.binding_controls);
}

#[tokio::test]
async fn the_new_endpoints_secrets_are_redacted_from_the_log_after_the_save() {
    for (reconnected, reason) in [
        (
            with_work(scripted(Vec::new())),
            format!("the gateway at /{URL_SECRET} refused {KEY}"),
        ),
        (
            Some(Err(HostError("config.toml is locked".into()))),
            format!("the gateway refused {KEY}"),
        ),
    ] {
        let env = Env::new("endpoint-redact");
        let store = MemoryStore::default();
        let host = Host::many(vec![saved_work(), Err(HostError(reason))], reconnected);
        let mut agent =
            two_sided(scripted(Vec::new()), &env, &store).with_endpoint_host(host.clone());
        let saved = queue_save(&mut agent, save(1, "work"), &host).await;
        assert!(matches!(
            saved.last(),
            Some(AgentEvent::EndpointSaved { .. })
        ));
        let events = queue_save(&mut agent, save(2, "other"), &host).await;
        assert!(matches!(
            events.as_slice(),
            [AgentEvent::Rejected {
                request: Some(RequestId(2)),
                ..
            }]
        ));
        let log = serde_json::to_string(&records(&store)).unwrap();
        no_canary(&log);
        assert!(log.contains("[redacted]"), "{log}");
    }
}

#[tokio::test]
async fn an_outcome_to_act_on_is_answered_and_logged_without_changing_the_set() {
    let env = Env::new("endpoint-not-saved");
    let store = MemoryStore::default();
    let why = NotSavedReason::Check(CheckResult {
        outcome: CheckKind::Unreachable,
        reason: Some("connection refused".into()),
    });
    let host = Host::new(
        Ok(SaveOutcome::NotSaved(NotSaved {
            name: "work".into(),
            why: why.clone(),
        })),
        None,
    );
    let mut agent = two_sided(scripted(Vec::new()), &env, &store).with_endpoint_host(host.clone());
    let events = queue_save(&mut agent, save(3, "work"), &host).await;
    assert_eq!(
        events,
        [AgentEvent::EndpointNotSaved {
            answers: RequestId(3),
            name: "work".into(),
            why: why.clone(),
        }]
    );
    assert_eq!(agent.connections.len(), 1);
    assert!(log_events(&store).contains(&LogEvent::EndpointNotSaved {
        turn: None,
        name: "work".into(),
        why,
    }));
}

#[tokio::test]
async fn host_errors_bad_specs_and_a_missing_host_are_endpoint_refusals() {
    let env = Env::new("endpoint-refused");
    let store = MemoryStore::default();
    let host = Host::new(Err(HostError("config.toml is locked".into())), None);
    let mut agent = two_sided(scripted(Vec::new()), &env, &store).with_endpoint_host(host.clone());
    let events = queue_save(&mut agent, save(4, "work"), &host).await;
    assert_eq!(
        events,
        [AgentEvent::endpoint_refused(
            Some(RequestId(4)),
            "config.toml is locked"
        )]
    );
    let rejected_key = "sk-ant-CANARYKEYREJECTEDSAVE0002";
    let invalid = ClientCommand::SaveEndpoint {
        request: RequestId(5),
        endpoint: spec("CANARY-bad"),
        key: Secret::new(rejected_key),
        check: true,
    };
    let events = queue_save(&mut agent, invalid, &host).await;
    assert_eq!(
        crate::session::without_registered_secrets(rejected_key),
        rejected_key
    );
    let [
        AgentEvent::Rejected {
            reason,
            kind: Some(RejectKind::Endpoint),
            request: Some(RequestId(5)),
            ..
        },
    ] = events.as_slice()
    else {
        panic!("{events:?}");
    };
    assert!(!reason.contains("CANARY"));
    assert_eq!(host.saves().len(), 1);
    let (tx, mut rx) = mpsc::channel(16);
    agent
        .auth_command(crate::endpoints::testing::logout(7, "direct"), &tx)
        .await;
    let events = drain(&mut rx);
    assert!(
        matches!(
            events.as_slice(),
            [AgentEvent::SubscriptionAuth { event }] if matches!(
                event.outcome,
                wisp_protocol::subscription_auth::AuthOutcome::Failed {
                    kind: wisp_protocol::subscription_auth::AuthFailureKind::Unavailable,
                    ..
                }
            )
        ),
        "{events:?}"
    );
    assert_eq!(agent.endpoint_name(), "direct");
    let mut bare = two_sided(scripted(Vec::new()), &env, &MemoryStore::default());
    let events = queue_save(&mut bare, save(6, "work"), &host).await;
    assert_eq!(
        events,
        [AgentEvent::endpoint_refused(
            Some(RequestId(6)),
            super::super::queue::NO_ENDPOINT_HOST
        )]
    );
}

#[tokio::test]
async fn a_cancelled_save_sends_no_answer() {
    let env = Env::new("endpoint-cancelled");
    let store = MemoryStore::default();
    let host = Host::new(Ok(SaveOutcome::Cancelled), None);
    let mut agent = two_sided(scripted(Vec::new()), &env, &store).with_endpoint_host(host.clone());
    assert!(
        queue_save(&mut agent, save(8, "work"), &host)
            .await
            .is_empty()
    );
}

#[tokio::test]
async fn a_message_sent_during_a_save_waits_and_can_go_to_the_new_endpoint() {
    let env = Env::new("endpoint-live");
    let store = MemoryStore::default();
    let work = scripted(vec![done("from work")]);
    let host = Host::new(saved_work(), with_work(work.clone()));
    let direct = scripted(Vec::new());
    let agent = two_sided(direct.clone(), &env, &store).with_endpoint_host(host.clone());
    let (commands, mut events, task) = spawn(agent);
    commands.send(save(1, "work")).await.unwrap();
    commands
        .send(ClientCommand::shortcut(
            RequestId(2),
            Some("work".into()),
            None,
            None,
        ))
        .await
        .unwrap();
    commands
        .send(ClientCommand::Send {
            id: InputId("tui-1".into()),
            text: "hello".into(),
            images: Vec::new(),
            mode: DeliveryMode::Steer,
            turn: None,
            replaces: None,
        })
        .await
        .unwrap();
    tokio::time::sleep(Duration::from_millis(200)).await;
    assert_eq!(*task.worst.borrow(), SAVE_WORST);
    host.open();
    let mut seen = Vec::new();
    until_turn_end(&mut events, &mut seen).await;
    let at = |f: fn(&AgentEvent) -> bool| seen.iter().position(f).unwrap();
    let saved = at(|e| matches!(e, AgentEvent::EndpointSaved { .. }));
    let changed = at(|e| matches!(e, AgentEvent::SettingsChanged { .. }));
    let started = at(|e| matches!(e, AgentEvent::TurnStarted { .. }));
    assert!(saved < changed && changed < started, "{seen:?}");
    assert_eq!(work.sent().len(), 1);
    assert!(direct.sent().is_empty());
    drop(commands);
    while events.recv().await.is_some() {}
    assert!(task.join().await);
}

mod edits;
