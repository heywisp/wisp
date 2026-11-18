use std::collections::VecDeque;
use std::sync::{Arc, Mutex};

use futures::FutureExt;
use futures::future::BoxFuture;
use tokio::sync::{Notify, mpsc, oneshot, watch};
use wisp_core::endpoints::host::{
    EndpointHost, HostError, NotSaved, Reconnected, SaveOutcome, Saved,
};
use wisp_protocol::endpoint::{
    CheckKind, CheckResult, EndpointSpec, NotSavedReason, SET_UP_FIRST, StoreChoice, StoreKind,
};
use wisp_protocol::subscription_auth::{
    AuthCommand, AuthEvent, AuthOutcome, ChallengeKind, LocalState, PrivateChallenge,
    SubscriptionEndpoint, SubscriptionProvider,
};
use wisp_protocol::voice::{AudioFormat, CallId, VoiceCommand, VoiceEvent};
use wisp_protocol::{
    AgentEvent, ClientCommand, DeliveryMode, Effort, ErrorKind, InputId, RejectKind, RequestId,
    Secret,
};
use wisp_render::chrome::Endpoint;
use wisp_tui::app::Session;
use wisp_tui::app::setup::SessionStart;

use super::serve;
use crate::pick::pick;
use crate::tui::{Chosen, facts};

const KEY: &str = "sk-ant-CANARYSERVERKEY77";

#[derive(Default)]
struct Scripted {
    outcomes: Mutex<VecDeque<Result<SaveOutcome, HostError>>>,
    gate: Option<Arc<Notify>>,
    cancelled: Mutex<Vec<bool>>,
    committing: Option<Notify>,
}

impl Scripted {
    fn with(outcomes: Vec<Result<SaveOutcome, HostError>>) -> Scripted {
        Scripted {
            outcomes: Mutex::new(outcomes.into()),
            ..Scripted::default()
        }
    }
}

impl EndpointHost for Scripted {
    fn cancel_auth(&self) {
        if let Some(committing) = &self.committing {
            committing.notify_one();
        }
    }

    fn auth(&self, command: AuthCommand) -> BoxFuture<'_, AuthEvent> {
        async move {
            let request = command.request();
            let outcome = match command {
                AuthCommand::Start { .. } => AuthOutcome::Challenge {
                    challenge: PrivateChallenge {
                        login: request,
                        provider: SubscriptionProvider::Claude,
                        alias: "manifest".into(),
                        kind: ChallengeKind::ManualCode,
                        authorization_url: Secret::new("https://claude.example/CANARYLINK"),
                        expires_in_secs: 600,
                        opened: false,
                    },
                },
                AuthCommand::Finish { .. } => {
                    if let Some(committing) = &self.committing {
                        committing.notified().await;
                    }
                    AuthOutcome::Ready {
                        endpoint: SubscriptionEndpoint {
                            alias: "manifest".into(),
                            provider: SubscriptionProvider::Claude,
                            storage: Some(StoreKind::File),
                            is_default: true,
                            state: LocalState::Working,
                        },
                        previous_cleanup_pending: false,
                    }
                }
                AuthCommand::Cancel { login, .. } => AuthOutcome::Cancelled { login },
                _ => wisp_core::endpoints::host::unavailable(),
            };
            AuthEvent { request, outcome }
        }
        .boxed()
    }

    fn save(
        &self,
        _spec: EndpointSpec,
        _key: Secret,
        _check: bool,
        cancel: watch::Receiver<bool>,
    ) -> BoxFuture<'_, Result<SaveOutcome, HostError>> {
        async move {
            if let Some(gate) = &self.gate {
                gate.notified().await;
            }
            self.cancelled.lock().unwrap().push(*cancel.borrow());
            let next = self.outcomes.lock().unwrap().pop_front();
            next.unwrap_or(Ok(SaveOutcome::Cancelled))
        }
        .boxed()
    }

    fn edit(
        &self,
        _name: String,
        _edit: wisp_protocol::endpoint::EndpointEdit,
        _store: Option<StoreChoice>,
        _check: bool,
    ) -> BoxFuture<'_, Result<wisp_core::endpoints::host::EditOutcome, HostError>> {
        async { Err(HostError("not scripted".into())) }.boxed()
    }

    fn remove(
        &self,
        _name: String,
        _keep_key: bool,
    ) -> BoxFuture<'_, Result<wisp_core::endpoints::host::RemoveOutcome, HostError>> {
        async { Err(HostError("not scripted".into())) }.boxed()
    }

    fn connections(&self) -> BoxFuture<'_, Result<Reconnected, HostError>> {
        async { Ok(Reconnected::default()) }.boxed()
    }
}

fn spec() -> EndpointSpec {
    EndpointSpec {
        name: "manifest".into(),
        provider: "anthropic".into(),
        auth: "bearer".into(),
        url: Some(Secret::new("https://gw.example.com/CANARYPATH")),
        store: StoreChoice::Auto,
        default: true,
    }
}

fn save(request: u64) -> ClientCommand {
    ClientCommand::SaveEndpoint {
        request: RequestId(request),
        endpoint: spec(),
        key: Secret::new(KEY),
        check: true,
    }
}

fn sign_in(command: AuthCommand) -> ClientCommand {
    ClientCommand::SubscriptionAuth { command }
}

fn finish(request: u64, login: u64) -> ClientCommand {
    sign_in(AuthCommand::Finish {
        request: RequestId(request),
        login: RequestId(login),
        response: Some(Secret::new("CODE_CANARY#STATE_CANARY")),
        alias: None,
    })
}

fn signed_in(events: &[AgentEvent]) -> Vec<(RequestId, bool)> {
    pick!(
        events,
        AgentEvent::SubscriptionAuth { event } => (
            event.request,
            matches!(event.outcome, AuthOutcome::Ready { .. }),
        )
    )
}

fn shown() -> ClientCommand {
    ClientCommand::SlotsShown {
        sections: vec!["context/window".into()],
        pages: vec!["context/window".into()],
    }
}

fn saved() -> SaveOutcome {
    SaveOutcome::Saved(Saved {
        name: "manifest".into(),
        store: StoreKind::File,
        check: CheckResult::of(CheckKind::Ok),
        notes: vec!["note: inside a git work tree".into()],
    })
}

fn session_start() -> SessionStart {
    let (commands, _) = mpsc::channel(1);
    let (_, events) = mpsc::channel(1);
    let chosen = Chosen {
        model: "claude-sonnet-5-5",
        effort: "low",
        cache_ttl: wisp_protocol::CacheTtl::OneHour,
        endpoint: Endpoint::Custom,
        max_tokens: 64_000,
        claude_usage: false,
    };
    SessionStart {
        session: Session {
            leave_note: None,
            attached: None,
            alerts: Vec::new(),
            facts: facts(chosen, None, None, None),
            events,
            commands,
            resumed: None,
        },
        started: None,
        warnings: vec!["wisp: a start warning".into()],
        quit: None,
    }
}

fn start_on(request: u64, follow: &[&str]) -> ClientCommand {
    ClientCommand::StartOn {
        request: RequestId(request),
        account: "manifest".to_owned(),
        follow: follow.iter().map(|name| (*name).to_owned()).collect(),
    }
}

async fn drain(mut events: mpsc::Receiver<AgentEvent>) -> Vec<AgentEvent> {
    let mut out = Vec::new();
    while let Some(event) = events.recv().await {
        out.push(event);
    }
    out
}

fn no_canary(events: &[AgentEvent]) {
    let text = format!("{events:?}");
    assert!(!text.contains("CANARY"), "{text}");
}

#[tokio::test]
async fn before_the_first_save_every_other_command_is_refused_with_its_request_but_slots_shown_gets_nothing()
 {
    let host = Scripted {
        committing: Some(Notify::new()),
        ..Scripted::default()
    };
    let (commands, server_commands) = mpsc::channel(32);
    let (server_events, mut events) = mpsc::channel(64);
    let (started, _start) = oneshot::channel();
    let others = vec![
        shown(),
        ClientCommand::queue("tui-1", "hello"),
        ClientCommand::ChangeSettings {
            request: RequestId(2),
            changes: Vec::new(),
            save: false,
        },
        ClientCommand::DescribeSettings {
            request: RequestId(3),
        },
        ClientCommand::shortcut(RequestId(4), None, None, Some(Effort::Low)),
        ClientCommand::Release,
        ClientCommand::Answer {
            call_id: "call".into(),
            answers: Vec::new(),
        },
        ClientCommand::Send {
            id: InputId("tui-5".into()),
            text: "steer".into(),
            images: Vec::new(),
            mode: DeliveryMode::Steer,
            turn: None,
            replaces: None,
        },
        ClientCommand::StopProcess { id: 1 },
        ClientCommand::Background {
            call_id: "call".into(),
        },
        ClientCommand::Compact {
            focus: Some("the parser".into()),
            request: RequestId(6),
        },
        ClientCommand::Retry {
            turn: 1,
            endpoint: Some("other".into()),
            request: RequestId(7),
        },
        ClientCommand::EditEndpoint {
            request: RequestId(8),
            name: "manifest".into(),
            edit: wisp_protocol::endpoint::EndpointEdit {
                key: Some(Secret::new(KEY)),
                ..wisp_protocol::endpoint::EndpointEdit::default()
            },
            store: None,
            check: true,
        },
        ClientCommand::RemoveEndpoint {
            request: RequestId(9),
            name: "manifest".into(),
            keep_key: false,
        },
    ];
    let refusals = others.len() - 1;
    let server = serve(
        &host,
        server_commands,
        server_events,
        started,
        async |_, _| panic!("no session starts before a save"),
    );
    let client = async {
        for command in others {
            commands.send(command).await.unwrap();
        }
        commands
            .send(sign_in(AuthCommand::Start {
                request: RequestId(10),
                provider: SubscriptionProvider::Claude,
                alias: None,
                reauthorize: false,
            }))
            .await
            .unwrap();
        commands.send(finish(11, 10)).await.unwrap();
        for command in [
            AuthCommand::Cancel {
                request: RequestId(12),
                login: RequestId(10),
            },
            AuthCommand::Open {
                request: RequestId(13),
                login: RequestId(10),
            },
        ] {
            commands.send(sign_in(command)).await.unwrap();
        }
        let mut seen = Vec::new();
        while seen.len() < refusals + 3 {
            seen.push(events.recv().await.unwrap());
        }
        drop(commands);
        seen.extend(drain(events).await);
        seen
    };
    let (served, events): (Option<()>, _) = tokio::join!(server, client);
    assert!(served.is_none());
    no_canary(&events);
    let answered = signed_in(&events);
    assert_eq!(answered.len(), 4, "{events:?}");
    assert!(
        answered[..3].contains(&(RequestId(10), false)),
        "{answered:?}"
    );
    assert!(
        answered[..3].contains(&(RequestId(12), false)),
        "{answered:?}"
    );
    assert!(
        answered[..3].contains(&(RequestId(13), false)),
        "{answered:?}"
    );
    assert_eq!(answered[3], (RequestId(11), true));
    let refused: Vec<(Option<InputId>, Option<RequestId>)> = events
        .iter()
        .filter(|e| !matches!(e, AgentEvent::SubscriptionAuth { .. }))
        .map(|e| match e {
            AgentEvent::Rejected {
                reason,
                id,
                kind: Some(RejectKind::Endpoint),
                request,
            } if reason == SET_UP_FIRST => (id.clone(), *request),
            other => panic!("{other:?}"),
        })
        .collect();
    assert_eq!(
        refused,
        [
            (Some(InputId("tui-1".into())), None),
            (None, Some(RequestId(2))),
            (None, Some(RequestId(3))),
            (None, Some(RequestId(4))),
            (None, None),
            (None, None),
            (Some(InputId("tui-5".into())), None),
            (None, None),
            (None, None),
            (None, Some(RequestId(6))),
            (None, Some(RequestId(7))),
            (None, Some(RequestId(8))),
            (None, Some(RequestId(9))),
        ]
    );
}

#[tokio::test]
async fn each_answer_echoes_its_request_and_the_first_save_starts_the_session() {
    let host = Scripted::with(vec![
        Ok(SaveOutcome::NotSaved(NotSaved {
            name: "manifest".into(),
            why: NotSavedReason::KeychainFailed("didn't answer".into()),
        })),
        Err(HostError(
            "an account named \"manifest\" already exists".into(),
        )),
        Ok(saved()),
    ]);
    let (commands, server_commands) = mpsc::channel(16);
    let (server_events, events) = mpsc::channel(64);
    let (started, mut start) = oneshot::channel();
    for request in 1..=3 {
        commands.send(save(request)).await.unwrap();
    }
    commands.send(finish(4, 1)).await.unwrap();
    commands.send(start_on(5, &["coder"])).await.unwrap();
    let served = serve(
        &host,
        server_commands,
        server_events,
        started,
        async |account, follow| {
            assert_eq!(
                (account.as_str(), follow.as_slice()),
                ("manifest", &["coder".to_owned()][..])
            );
            Ok((session_start(), 42u8))
        },
    )
    .await;
    assert_eq!(served, Some(42));
    let events = drain(events).await;
    no_canary(&events);
    assert_eq!(signed_in(&events), [(RequestId(4), true)]);
    let events: Vec<AgentEvent> = events
        .into_iter()
        .filter(|e| !matches!(e, AgentEvent::SubscriptionAuth { .. }))
        .collect();
    assert!(matches!(
        &events[0],
        AgentEvent::EndpointNotSaved {
            answers: RequestId(1),
            ..
        }
    ));
    assert!(matches!(
        &events[1],
        AgentEvent::Rejected {
            request: Some(RequestId(2)),
            kind: Some(RejectKind::Endpoint),
            ..
        }
    ));
    assert!(matches!(
        &events[2],
        AgentEvent::EndpointSaved { answers: RequestId(3), notes, .. } if notes.len() == 1
    ));
    assert_eq!(events.len(), 3);
    let session = start.try_recv().unwrap().unwrap();
    assert_eq!(session.warnings, ["wisp: a start warning"]);
    drop(commands);
}

#[tokio::test]
async fn a_start_that_fails_hands_its_error_over() {
    let host = Scripted::with(vec![Ok(saved())]);
    let (commands, server_commands) = mpsc::channel(16);
    let (server_events, events) = mpsc::channel(64);
    let (started, mut start) = oneshot::channel();
    commands.send(save(1)).await.unwrap();
    commands.send(start_on(2, &[])).await.unwrap();
    let served: Option<()> = serve(
        &host,
        server_commands,
        server_events,
        started,
        async |_, _| Err("wisp: the provider wouldn't connect".to_owned()),
    )
    .await;
    assert!(served.is_none());
    assert_eq!(drain(events).await.len(), 1);
    assert_eq!(
        start.try_recv().unwrap().err().as_deref(),
        Some("wisp: the provider wouldn't connect")
    );
    drop(commands);
}

#[tokio::test]
async fn a_quit_during_a_save_cancels_it_waits_for_it_and_starts_nothing() {
    for outcome in [SaveOutcome::Cancelled, saved()] {
        let gate = Arc::new(Notify::new());
        let host = Scripted {
            outcomes: Mutex::new(VecDeque::from([Ok(outcome.clone())])),
            gate: Some(Arc::clone(&gate)),
            ..Scripted::default()
        };
        let (commands, server_commands) = mpsc::channel(16);
        let (server_events, events) = mpsc::channel(64);
        let (started, mut start) = oneshot::channel();
        commands.send(save(1)).await.unwrap();
        let server = serve(
            &host,
            server_commands,
            server_events,
            started,
            async |_, _| panic!("a quit skips the start"),
        );
        let quit = async {
            tokio::task::yield_now().await;
            drop(commands);
            tokio::task::yield_now().await;
            gate.notify_one();
        };
        let (served, ()): (Option<()>, ()) = tokio::join!(server, quit);
        assert!(served.is_none());
        assert_eq!(*host.cancelled.lock().unwrap(), [true]);
        let events = drain(events).await;
        match outcome {
            SaveOutcome::Saved(_) => assert!(matches!(
                events.as_slice(),
                [AgentEvent::EndpointSaved { .. }]
            )),
            _ => assert!(events.is_empty(), "{events:?}"),
        }
        assert!(start.try_recv().is_err());
    }
}

#[tokio::test]
async fn slots_shown_during_a_save_gets_nothing_and_the_session_starts() {
    let gate = Arc::new(Notify::new());
    let host = Scripted {
        outcomes: Mutex::new(VecDeque::from([Ok(saved())])),
        gate: Some(Arc::clone(&gate)),
        ..Scripted::default()
    };
    let (commands, server_commands) = mpsc::channel(16);
    let (server_events, events) = mpsc::channel(64);
    let (started, mut start) = oneshot::channel();
    commands.send(save(1)).await.unwrap();
    let server = serve(
        &host,
        server_commands,
        server_events,
        started,
        async |_, _| Ok((session_start(), 7u8)),
    );
    let during = async {
        tokio::task::yield_now().await;
        commands.send(shown()).await.unwrap();
        for _ in 0..4 {
            tokio::task::yield_now().await;
        }
        gate.notify_one();
        commands.send(start_on(2, &[])).await.unwrap();
    };
    let (served, ()) = tokio::join!(server, during);
    assert_eq!(served, Some(7));
    drop(commands);
    let events = drain(events).await;
    assert!(
        matches!(events.as_slice(), [AgentEvent::EndpointSaved { .. }]),
        "{events:?}"
    );
    assert!(start.try_recv().unwrap().is_ok());
}

#[tokio::test]
async fn a_quit_with_nothing_in_flight_ends_the_server() {
    let host = Scripted::default();
    let (commands, server_commands) = mpsc::channel::<ClientCommand>(16);
    let (server_events, events) = mpsc::channel(64);
    let (started, mut start) = oneshot::channel();
    drop(commands);
    let served: Option<()> = serve(
        &host,
        server_commands,
        server_events,
        started,
        async |_, _| panic!("no start"),
    )
    .await;
    assert!(served.is_none());
    assert!(drain(events).await.is_empty());
    assert!(start.try_recv().is_err());
}

#[tokio::test]
async fn before_the_first_save_a_voice_start_ends_failed_and_the_other_voice_commands_are_refused()
{
    let host = Scripted::default();
    let (commands, server_commands) = mpsc::channel(16);
    let (server_events, events) = mpsc::channel(64);
    let (started, _start) = oneshot::channel();
    let call = || CallId("web-1".into());
    for command in [
        VoiceCommand::Start {
            call: call(),
            format: AudioFormat::Pcm16Mono48k,
        },
        VoiceCommand::Note {
            call: call(),
            text: "Tapped: the red circle".into(),
        },
        VoiceCommand::Stop { call: call() },
    ] {
        commands
            .send(ClientCommand::Voice { command })
            .await
            .unwrap();
    }
    drop(commands);
    let served: Option<()> = serve(
        &host,
        server_commands,
        server_events,
        started,
        async |_, _| panic!("no session starts before a save"),
    )
    .await;
    assert!(served.is_none());
    let refused = AgentEvent::Rejected {
        reason: SET_UP_FIRST.to_owned(),
        id: None,
        kind: Some(RejectKind::Endpoint),
        request: None,
    };
    assert_eq!(
        drain(events).await,
        [
            AgentEvent::Voice {
                event: VoiceEvent::failed(call(), ErrorKind::InvalidRequest, SET_UP_FIRST),
            },
            refused.clone(),
            refused,
        ]
    );
}
