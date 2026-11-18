use wisp_protocol::VariableName;
use wisp_protocol::endpoint::{ConnectionField, EndpointEdit, VariableEdit};
use wisp_protocol::settings::{SettingChange, SettingValue};

use super::*;
use crate::agent::queue::{EndpointOp, NEXT_TURN, SWITCH_FIRST, WAITING_RETRY};
use crate::endpoints::host::{
    EditOutcome, Edited, NOTHING_TO_CHANGE, RemoveOutcome, Removed, key_shaped_variable,
};

type Edit = (String, EndpointEdit, Option<StoreChoice>, bool);

struct EditHost {
    gate: Semaphore,
    edited: Mutex<Vec<Result<EditOutcome, HostError>>>,
    removed: Mutex<Vec<Result<RemoveOutcome, HostError>>>,
    reconnected: Mutex<Option<Result<Reconnected, HostError>>>,
    edits: Mutex<Vec<Edit>>,
    removes: Mutex<Vec<(String, bool)>>,
}

impl EditHost {
    fn new(
        edited: Vec<Result<EditOutcome, HostError>>,
        removed: Vec<Result<RemoveOutcome, HostError>>,
        reconnected: Option<Result<Reconnected, HostError>>,
    ) -> Arc<Self> {
        Self::gated(Semaphore::MAX_PERMITS / 2, edited, removed, reconnected)
    }

    fn gated(
        permits: usize,
        edited: Vec<Result<EditOutcome, HostError>>,
        removed: Vec<Result<RemoveOutcome, HostError>>,
        reconnected: Option<Result<Reconnected, HostError>>,
    ) -> Arc<Self> {
        Arc::new(Self {
            gate: Semaphore::new(permits),
            edited: Mutex::new(edited.into_iter().rev().collect()),
            removed: Mutex::new(removed.into_iter().rev().collect()),
            reconnected: Mutex::new(reconnected),
            edits: Mutex::new(Vec::new()),
            removes: Mutex::new(Vec::new()),
        })
    }

    fn open(&self) {
        if self.gate.available_permits() == 0 {
            self.gate.add_permits(Semaphore::MAX_PERMITS / 2);
        }
    }

    fn calls(&self) -> usize {
        self.edits.lock().unwrap().len() + self.removes.lock().unwrap().len()
    }
}

impl EndpointHost for EditHost {
    fn save(
        &self,
        _spec: EndpointSpec,
        _key: Secret,
        _check: bool,
        _cancel: watch::Receiver<bool>,
    ) -> BoxFuture<'_, Result<SaveOutcome, HostError>> {
        async { Err(HostError("not scripted".into())) }.boxed()
    }

    fn edit(
        &self,
        name: String,
        edit: EndpointEdit,
        store: Option<StoreChoice>,
        check: bool,
    ) -> BoxFuture<'_, Result<EditOutcome, HostError>> {
        async move {
            drop(self.gate.acquire().await);
            self.edits.lock().unwrap().push((name, edit, store, check));
            self.edited
                .lock()
                .unwrap()
                .pop()
                .unwrap_or_else(|| Err(HostError("no edit scripted".into())))
        }
        .boxed()
    }

    fn remove(
        &self,
        name: String,
        keep_key: bool,
    ) -> BoxFuture<'_, Result<RemoveOutcome, HostError>> {
        async move {
            drop(self.gate.acquire().await);
            self.removes.lock().unwrap().push((name, keep_key));
            self.removed
                .lock()
                .unwrap()
                .pop()
                .unwrap_or_else(|| Err(HostError("no removal scripted".into())))
        }
        .boxed()
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

fn with_both(direct: Arc<Fake>, work: Arc<Fake>, env: &Env, store: &MemoryStore) -> Agent {
    logged(direct.clone(), env, store).with_connections(
        vec![
            connection("direct", direct, &[]),
            connection("work", work, &[KEY]),
        ],
        Vec::new(),
        "direct",
    )
}

fn key_edit() -> EndpointEdit {
    EndpointEdit {
        key: Some(Secret::new(KEY)),
        ..EndpointEdit::default()
    }
}

fn edit(request: u64, name: &str, edit: EndpointEdit) -> ClientCommand {
    ClientCommand::EditEndpoint {
        request: RequestId(request),
        name: name.into(),
        edit,
        store: None,
        check: true,
    }
}

fn remove(request: u64, name: &str) -> ClientCommand {
    ClientCommand::RemoveEndpoint {
        request: RequestId(request),
        name: name.into(),
        keep_key: false,
    }
}

fn edited_work() -> Result<EditOutcome, HostError> {
    Ok(EditOutcome::Edited(Edited {
        name: "work".into(),
        changed: vec![ConnectionField::Key],
        store: Some(StoreKind::Keyring),
        check: CheckResult::of(CheckKind::Ok),
        notes: vec![NOTE.into()],
    }))
}

fn removed_work() -> Result<RemoveOutcome, HostError> {
    Ok(RemoveOutcome::Removed(Removed {
        name: "work".into(),
        kept_item: false,
        default_cleared: false,
        notes: vec![NOTE.into()],
        left: false,
        new_start: None,
    }))
}

fn only_direct() -> Option<Result<Reconnected, HostError>> {
    Some(Ok(Reconnected {
        ready: vec![connection("direct", scripted(Vec::new()), &[])],
        unavailable: Vec::new(),
        endpoints: vec![summary("direct", true)],
    }))
}

fn enqueued(command: ClientCommand) -> Queued {
    match command {
        ClientCommand::EditEndpoint {
            request,
            name,
            edit,
            store,
            check,
        } => Queued::Endpoint(EndpointOp::Edit {
            request,
            name,
            edit,
            store,
            check,
        }),
        ClientCommand::RemoveEndpoint {
            request,
            name,
            keep_key,
        } => Queued::Endpoint(EndpointOp::Remove {
            request,
            name,
            keep_key,
        }),
        ClientCommand::Retry {
            request,
            turn,
            endpoint,
        } => Queued::Retry {
            request,
            turn,
            endpoint,
        },
        _ => panic!("not queued here"),
    }
}

async fn run(agent: &mut Agent, commands: Vec<ClientCommand>, host: &EditHost) -> Vec<AgentEvent> {
    let (tx, mut rx) = mpsc::channel(1024);
    for command in commands {
        agent.enqueue(enqueued(command), &tx).await;
    }
    host.open();
    agent.settle_queue(&tx).await;
    drop(tx);
    drain(&mut rx)
}

fn refusal(events: &[AgentEvent]) -> (RequestId, String) {
    let [
        AgentEvent::Rejected {
            reason,
            kind: Some(RejectKind::Endpoint),
            request: Some(request),
            ..
        },
    ] = events
    else {
        panic!("{events:?}");
    };
    (*request, reason.clone())
}

#[tokio::test]
async fn an_edit_announces_the_set_then_answers_and_logs_no_notes_or_secrets() {
    let env = Env::new("endpoint-edit");
    let store = MemoryStore::default();
    let fresh = scripted(vec![done("from the new key")]);
    let host = EditHost::new(vec![edited_work()], Vec::new(), with_work(fresh.clone()));
    let stale = scripted(Vec::new());
    let mut agent = with_both(scripted(vec![done("hi")]), stale.clone(), &env, &store)
        .with_endpoint_host(host.clone());
    turn_with(&mut agent, "hello", None).await;
    let events = run(&mut agent, vec![edit(7, "work", key_edit())], &host).await;
    let [
        AgentEvent::EndpointsChanged { endpoints },
        AgentEvent::EndpointEdited {
            answers,
            name,
            changed,
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
    assert_eq!(changed, &[ConnectionField::Key]);
    assert_eq!(*kind, Some(StoreKind::Keyring));
    assert_eq!(check.outcome, CheckKind::Ok);
    assert_eq!(notes, &[NOTE.to_owned()]);
    assert_eq!(
        endpoints,
        &[summary("direct", true), summary("work", false)]
    );
    let edits = host.edits.lock().unwrap().clone();
    assert_eq!(edits.len(), 1);
    assert_eq!(edits[0].1.key.as_ref().map(Secret::expose), Some(KEY));
    no_canary(&serde_json::to_string(&events).unwrap());
    switch(&mut agent, "work").await;
    turn_with(&mut agent, "again", None).await;
    assert_eq!(fresh.sent().len(), 1, "the edited endpoint is used");
    assert!(stale.sent().is_empty());
    let records = log_events(&store);
    assert!(records.contains(&LogEvent::EndpointEdited {
        turn: None,
        name: "work".into(),
        changed: vec![ConnectionField::Key],
        store: Some(StoreKind::Keyring),
        check: CheckResult::of(CheckKind::Ok),
    }));
    let log = serde_json::to_string(&test_guard::records(&store)).unwrap();
    assert!(!log.contains("dotfiles"), "{log}");
    no_canary(&log);
}

#[tokio::test]
async fn an_edits_typed_key_is_redacted_from_the_log_when_reload_fails() {
    let env = Env::new("endpoint-edit-reload-redact");
    let store = MemoryStore::default();
    let key = "sk-ant-CANARYKEYEDITRELOAD0002";
    let host = EditHost::new(
        vec![
            edited_work(),
            Err(HostError(format!("the gateway refused {key}"))),
        ],
        Vec::new(),
        Some(Err(HostError("config.toml is locked".into()))),
    );
    let mut agent = with_both(scripted(Vec::new()), scripted(Vec::new()), &env, &store)
        .with_endpoint_host(host.clone());
    let mut replacement = key_edit();
    replacement.key = Some(Secret::new(format!(" {key}\n")));
    let events = run(
        &mut agent,
        vec![edit(1, "work", replacement), edit(2, "work", key_edit())],
        &host,
    )
    .await;
    assert!(matches!(
        events.as_slice(),
        [
            AgentEvent::EndpointEdited { .. },
            AgentEvent::Rejected { .. }
        ]
    ));
    assert_eq!(host.calls(), 2);
    let log = serde_json::to_string(&records(&store)).unwrap();
    no_canary(&log);
    assert!(log.contains("[redacted]"), "{log}");
}

#[tokio::test]
async fn an_edit_of_the_current_endpoint_leaves_this_sessions_requests_alone() {
    let env = Env::new("endpoint-edit-current");
    let store = MemoryStore::default();
    let direct = scripted(vec![done("one"), done("two")]);
    let edited = Ok(EditOutcome::Edited(Edited {
        name: "direct".into(),
        changed: vec![ConnectionField::Key],
        store: Some(StoreKind::Keyring),
        check: CheckResult::of(CheckKind::Ok),
        notes: Vec::new(),
    }));
    let replacement = scripted(Vec::new());
    let reconnected = Some(Ok(Reconnected {
        ready: vec![
            connection("direct", replacement.clone(), &[]),
            connection("work", scripted(Vec::new()), &[]),
        ],
        unavailable: Vec::new(),
        endpoints: vec![summary("direct", true), summary("work", false)],
    }));
    let host = EditHost::new(vec![edited], Vec::new(), reconnected);
    let mut agent = with_both(direct.clone(), scripted(Vec::new()), &env, &store)
        .with_endpoint_host(host.clone());
    turn_with(&mut agent, "first", None).await;
    run(&mut agent, vec![edit(1, "direct", key_edit())], &host).await;
    turn_with(&mut agent, "second", None).await;
    assert_eq!(direct.sent().len(), 2);
    assert!(replacement.sent().is_empty());
}

#[tokio::test]
async fn invalid_edits_are_refused_at_the_head_without_reaching_the_host() {
    let env = Env::new("endpoint-edit-invalid");
    let store = MemoryStore::default();
    let host = EditHost::new(Vec::new(), Vec::new(), None);
    let mut agent = with_both(scripted(Vec::new()), scripted(Vec::new()), &env, &store)
        .with_endpoint_host(host.clone());
    let events = run(
        &mut agent,
        vec![edit(1, "work", EndpointEdit::default())],
        &host,
    )
    .await;
    assert_eq!(
        refusal(&events),
        (RequestId(1), NOTHING_TO_CHANGE.to_owned())
    );
    let shaped = EndpointEdit {
        key_env: VariableName::parse("abcdefghijCANARY0123456789").map(VariableEdit::Set),
        ..EndpointEdit::default()
    };
    let events = run(&mut agent, vec![edit(2, "work", shaped)], &host).await;
    assert_eq!(
        refusal(&events),
        (RequestId(2), key_shaped_variable("key_env"))
    );
    let rejected_key = "sk-ant-CANARYKEYREJECTEDEDIT0003";
    let invalid = EndpointEdit {
        key: Some(Secret::new(rejected_key)),
        ..EndpointEdit::default()
    };
    let events = run(&mut agent, vec![edit(3, "env", invalid)], &host).await;
    assert_eq!(
        crate::session::without_registered_secrets(rejected_key),
        rejected_key
    );
    assert_eq!(refusal(&events).0, RequestId(3));
    assert_eq!(host.calls(), 0);
    let log = serde_json::to_string(&store.read("s1").unwrap_or_default()).unwrap();
    no_canary(&log);
    assert!(!log.contains("CANARY"), "{log}");
}

#[tokio::test]
async fn outcomes_to_act_on_and_host_errors_answer_the_command() {
    let env = Env::new("endpoint-edit-outcomes");
    let store = MemoryStore::default();
    let why = NotSavedReason::KeychainFailed("isn't running".into());
    let host = EditHost::new(
        vec![
            Ok(EditOutcome::NotSaved(NotSaved {
                name: "work".into(),
                why: why.clone(),
            })),
            Err(HostError("config.toml is locked".into())),
        ],
        vec![Ok(RemoveOutcome::NotSaved(NotSaved {
            name: "work".into(),
            why: why.clone(),
        }))],
        None,
    );
    let mut agent = with_both(scripted(Vec::new()), scripted(Vec::new()), &env, &store)
        .with_endpoint_host(host.clone());
    let events = run(&mut agent, vec![edit(1, "work", key_edit())], &host).await;
    assert_eq!(
        events,
        [AgentEvent::EndpointNotSaved {
            answers: RequestId(1),
            name: "work".into(),
            why: why.clone(),
        }]
    );
    let events = run(&mut agent, vec![edit(2, "work", key_edit())], &host).await;
    assert_eq!(
        events,
        [AgentEvent::endpoint_refused(
            Some(RequestId(2)),
            "config.toml is locked"
        )]
    );
    let events = run(&mut agent, vec![remove(3, "work")], &host).await;
    assert_eq!(
        events,
        [AgentEvent::EndpointNotSaved {
            answers: RequestId(3),
            name: "work".into(),
            why,
        }]
    );
    assert_eq!(agent.connections.len(), 2);
}

#[tokio::test]
async fn a_removal_answers_announces_and_the_endpoint_can_no_longer_be_chosen() {
    let env = Env::new("endpoint-remove");
    let store = MemoryStore::default();
    let host = EditHost::new(Vec::new(), vec![removed_work()], only_direct());
    let mut agent = with_both(scripted(Vec::new()), scripted(Vec::new()), &env, &store)
        .with_endpoint_host(host.clone());
    let events = run(&mut agent, vec![remove(4, "work")], &host).await;
    let [
        AgentEvent::EndpointsChanged { endpoints },
        AgentEvent::EndpointRemoved {
            answers: RequestId(4),
            name,
            kept_item: false,
            default_cleared: false,
            notes,
        },
    ] = events.as_slice()
    else {
        panic!("{events:?}");
    };
    assert_eq!(name, "work");
    assert_eq!(notes, &[NOTE.to_owned()]);
    assert_eq!(endpoints, &[summary("direct", true)]);
    assert_eq!(*host.removes.lock().unwrap(), [("work".to_owned(), false)]);
    let refused = switch(&mut agent, "work").await;
    let [AgentEvent::Rejected { reason, .. }] = refused.as_slice() else {
        panic!("{refused:?}");
    };
    assert!(reason.contains("it was removed"), "{reason}");
    assert!(
        !agent.retry_endpoints().others,
        "a removed endpoint is never offered for a Retry"
    );
    assert!(log_events(&store).contains(&LogEvent::EndpointRemoved {
        turn: None,
        name: "work".into(),
        kept_item: false,
        default_cleared: false,
    }));
    let log = serde_json::to_string(&records(&store)).unwrap();
    assert!(!log.contains("dotfiles"), "{log}");
}

#[tokio::test]
async fn the_current_endpoint_and_the_next_turns_one_are_never_removed() {
    let env = Env::new("endpoint-remove-current");
    let store = MemoryStore::default();
    let host = EditHost::new(Vec::new(), Vec::new(), None);
    let mut agent = with_both(scripted(Vec::new()), scripted(Vec::new()), &env, &store)
        .with_endpoint_host(host.clone());
    let events = run(&mut agent, vec![remove(1, "direct")], &host).await;
    assert_eq!(refusal(&events), (RequestId(1), SWITCH_FIRST.to_owned()));
    switch(&mut agent, "work").await;
    let events = run(&mut agent, vec![remove(2, "work")], &host).await;
    assert_eq!(refusal(&events), (RequestId(2), NEXT_TURN.to_owned()));
    assert_eq!(host.calls(), 0);
}

#[tokio::test]
async fn an_endpoint_a_waiting_retry_names_is_never_removed() {
    let env = Env::new("endpoint-remove-retry");
    let store = MemoryStore::default();
    let host = EditHost::gated(
        0,
        vec![edited_work()],
        Vec::new(),
        with_work(scripted(Vec::new())),
    );
    let mut agent = with_both(scripted(Vec::new()), scripted(Vec::new()), &env, &store)
        .with_endpoint_host(host.clone());
    let events = run(
        &mut agent,
        vec![
            edit(1, "work", key_edit()),
            remove(2, "work"),
            ClientCommand::Retry {
                turn: 1,
                endpoint: Some("work".into()),
                request: RequestId(3),
            },
        ],
        &host,
    )
    .await;
    let refused: Vec<(RequestId, String)> = pick!(
        &events,
        AgentEvent::Rejected {
            reason,
            kind: Some(RejectKind::Endpoint),
            request: Some(request),
            ..
        } => (*request, reason.clone())
    );
    assert_eq!(refused, [(RequestId(2), WAITING_RETRY.to_owned())]);
    assert!(host.removes.lock().unwrap().is_empty());
}

fn offered(agent: &Agent) -> (Vec<String>, Vec<String>) {
    let snapshot = agent.snapshot(RequestId(80), None);
    let choices = snapshot
        .settings
        .iter()
        .find(|r| r.name.spec().name == "endpoint")
        .map(|r| r.choices.clone())
        .unwrap_or_default();
    let switches = agent
        .switch_targets(&agent.next_pair())
        .into_iter()
        .filter_map(|(target, _, _, _)| match target {
            wisp_protocol::settings::SwitchTarget::Endpoint(name) => Some(name),
            _ => None,
        })
        .collect();
    (choices, switches)
}

#[tokio::test]
async fn the_quoted_pairs_for_another_account_are_every_model_it_reaches() {
    let env = Env::new("endpoint-quoted-pairs");
    let store = MemoryStore::default();
    let agent = with_both(scripted(Vec::new()), scripted(Vec::new()), &env, &store);
    let pair = agent.next_pair();
    let quoted: Vec<(String, String)> = agent
        .switch_targets(&pair)
        .into_iter()
        .filter_map(|(target, _, _, _)| match target {
            wisp_protocol::settings::SwitchTarget::Pair { endpoint, model } => {
                Some((endpoint, model))
            }
            _ => None,
        })
        .collect();
    let shipped = crate::settings::shipped_models();
    let providers: Vec<Option<&'static str>> = shipped
        .iter()
        .map(|m| crate::endpoints::goes::model_provider(&m.id).map(|p| p.as_str()))
        .collect();
    let offers: Vec<wisp_protocol::settings::Offer<'_>> = shipped
        .iter()
        .zip(&providers)
        .map(|(m, provider)| wisp_protocol::settings::Offer {
            id: &m.id,
            provider: *provider,
            listed: m.listed,
        })
        .collect();
    let chat = wisp_protocol::settings::Chat {
        provider: crate::endpoints::goes::model_provider(&pair.model).map(|p| p.as_str()),
        model: &pair.model,
    };
    let rows = wisp_protocol::settings::reachable(
        &offers,
        wisp_protocol::settings::Landing::new("anthropic", &[], false, None),
        chat,
    );
    assert!(
        rows.iter()
            .any(|id| shipped.iter().any(|m| &m.id == id && m.listed.is_none())),
        "{rows:?}"
    );
    let expected: Vec<(String, String)> = rows
        .into_iter()
        .map(|model| ("work".to_owned(), model))
        .collect();
    assert_eq!(quoted, expected);
}

fn both() -> Vec<String> {
    vec!["direct".to_owned(), "work".to_owned()]
}

#[tokio::test]
async fn a_removed_endpoint_is_neither_a_choice_nor_a_switch_in_the_snapshot() {
    let env = Env::new("endpoint-remove-snapshot");
    let store = MemoryStore::default();
    let host = EditHost::new(Vec::new(), vec![removed_work()], only_direct());
    let mut agent = with_both(scripted(Vec::new()), scripted(Vec::new()), &env, &store)
        .with_endpoint_host(host.clone());
    assert_eq!(offered(&agent), (both(), both()));
    run(&mut agent, vec![remove(1, "work")], &host).await;
    let direct = vec!["direct".to_owned()];
    assert_eq!(offered(&agent), (direct.clone(), direct));
}

#[tokio::test]
async fn an_edit_that_breaks_an_endpoint_hides_it_until_a_later_save_brings_it_back() {
    let env = Env::new("endpoint-edit-breaks");
    let store = MemoryStore::default();
    let edited_direct = Ok(EditOutcome::Edited(Edited {
        name: "direct".into(),
        changed: vec![ConnectionField::Key],
        store: Some(StoreKind::Keyring),
        check: CheckResult::of(CheckKind::Ok),
        notes: Vec::new(),
    }));
    let host = EditHost::new(
        vec![edited_work(), edited_direct],
        Vec::new(),
        only_direct(),
    );
    let mut agent = with_both(scripted(Vec::new()), scripted(Vec::new()), &env, &store)
        .with_endpoint_host(host.clone());
    run(&mut agent, vec![edit(1, "work", key_edit())], &host).await;
    let direct = vec!["direct".to_owned()];
    assert_eq!(offered(&agent), (direct.clone(), direct));
    let refused = switch(&mut agent, "work").await;
    assert!(
        matches!(refused.as_slice(), [AgentEvent::Rejected { .. }]),
        "{refused:?}"
    );
    let fresh = scripted(vec![done("back")]);
    *host.reconnected.lock().unwrap() = with_work(fresh.clone());
    run(&mut agent, vec![edit(2, "direct", key_edit())], &host).await;
    assert_eq!(offered(&agent), (both(), both()));
    switch(&mut agent, "work").await;
    turn_with(&mut agent, "again", None).await;
    assert_eq!(fresh.sent().len(), 1, "the endpoint is back and used");
}

#[tokio::test]
async fn a_removal_whose_reload_fails_still_drops_the_endpoint() {
    let env = Env::new("endpoint-remove-reload-fails");
    let store = MemoryStore::default();
    let host = EditHost::new(
        Vec::new(),
        vec![removed_work()],
        Some(Err(HostError("config.toml is locked".into()))),
    );
    let mut agent = with_both(scripted(Vec::new()), scripted(Vec::new()), &env, &store)
        .with_endpoint_host(host.clone());
    agent.settings.layers.endpoints = vec![summary("direct", true), summary("work", false)];
    let events = run(&mut agent, vec![remove(1, "work")], &host).await;
    let [
        AgentEvent::EndpointsChanged { endpoints },
        AgentEvent::EndpointRemoved { notes, .. },
    ] = events.as_slice()
    else {
        panic!("{events:?}");
    };
    assert_eq!(notes, &[NOTE.to_owned()]);
    assert_eq!(endpoints, &[summary("direct", true)]);
    let refused = switch(&mut agent, "work").await;
    let [AgentEvent::Rejected { reason, .. }] = refused.as_slice() else {
        panic!("{refused:?}");
    };
    assert!(reason.contains("it was removed"), "{reason}");
    let direct = vec!["direct".to_owned()];
    assert_eq!(offered(&agent).0, direct);
}

#[tokio::test]
async fn an_edit_keeps_the_pricing_this_session_set_on_that_endpoint() {
    let env = Env::new("endpoint-edit-pricing");
    let store = MemoryStore::default();
    let edited_direct = Ok(EditOutcome::Edited(Edited {
        name: "direct".into(),
        changed: vec![ConnectionField::Key],
        store: Some(StoreKind::Keyring),
        check: CheckResult::of(CheckKind::Ok),
        notes: Vec::new(),
    }));
    let host = EditHost::new(
        vec![edited_direct],
        Vec::new(),
        with_work(scripted(vec![done("there")])),
    );
    let mut agent = with_both(
        scripted(Vec::new()),
        scripted(vec![done("there")]),
        &env,
        &store,
    )
    .with_endpoint_host(host.clone());
    let (tx, mut rx) = mpsc::channel(64);
    agent
        .enqueue(
            Queued::Change {
                request: Some(RequestId(1)),
                changes: vec![
                    SettingChange::named("pricing", SettingValue::Text("none".into())).unwrap(),
                ],
                save: false,
                projected: crate::agent::settings::Projected::default(),
                by: None,
            },
            &tx,
        )
        .await;
    drop(tx);
    drain(&mut rx);
    switch(&mut agent, "work").await;
    turn_with(&mut agent, "hello", None).await;
    let priced = |agent: &Agent| -> Vec<(String, bool)> {
        agent
            .connections
            .iter()
            .map(|c| (c.name.clone(), c.priced))
            .collect()
    };
    let unpriced = vec![("direct".to_owned(), false), ("work".to_owned(), true)];
    assert_eq!(priced(&agent), unpriced);
    run(&mut agent, vec![edit(7, "direct", key_edit())], &host).await;
    assert_eq!(host.edits.lock().unwrap().len(), 1);
    assert_eq!(priced(&agent), unpriced);
}

#[tokio::test]
async fn an_endpoint_that_drops_out_and_comes_back_keeps_this_sessions_pricing() {
    let env = Env::new("endpoint-back-pricing");
    let store = MemoryStore::default();
    let edited_direct = Ok(EditOutcome::Edited(Edited {
        name: "direct".into(),
        changed: vec![ConnectionField::Key],
        store: Some(StoreKind::Keyring),
        check: CheckResult::of(CheckKind::Ok),
        notes: Vec::new(),
    }));
    let host = EditHost::new(
        vec![edited_work(), edited_direct],
        Vec::new(),
        only_direct(),
    );
    let mut agent = with_both(
        scripted(vec![done("home")]),
        scripted(vec![done("there")]),
        &env,
        &store,
    )
    .with_endpoint_host(host.clone());
    switch(&mut agent, "work").await;
    turn_with(&mut agent, "hello", None).await;
    let (tx, mut rx) = mpsc::channel(64);
    agent
        .enqueue(
            Queued::Change {
                request: Some(RequestId(1)),
                changes: vec![
                    SettingChange::named("pricing", SettingValue::Text("none".into())).unwrap(),
                ],
                save: false,
                projected: crate::agent::settings::Projected::default(),
                by: None,
            },
            &tx,
        )
        .await;
    drop(tx);
    drain(&mut rx);
    switch(&mut agent, "direct").await;
    turn_with(&mut agent, "back", None).await;
    run(&mut agent, vec![edit(2, "work", key_edit())], &host).await;
    assert_eq!(offered(&agent).0, ["direct".to_owned()], "work dropped out");
    *host.reconnected.lock().unwrap() = with_work(scripted(Vec::new()));
    run(&mut agent, vec![edit(3, "direct", key_edit())], &host).await;
    let priced: Vec<(String, bool)> = agent
        .connections
        .iter()
        .map(|c| (c.name.clone(), c.priced))
        .collect();
    assert_eq!(
        priced,
        [("direct".to_owned(), true), ("work".to_owned(), false)]
    );
}
