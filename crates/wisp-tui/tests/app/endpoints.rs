use std::future::Future;
use std::time::Duration;

use tokio::sync::{mpsc, oneshot};
use tokio::time::sleep;
use wisp_protocol::endpoint::{CheckKind, CheckResult, NotSavedReason, StoreChoice, StoreKind};
use wisp_protocol::settings::{CredentialKind, EndpointSummary};
use wisp_protocol::subscription_auth::{
    AuthCommand, AuthEvent, AuthOutcome, ChallengeKind, LocalState, PrivateChallenge,
    SubscriptionEndpoint, SubscriptionProvider, Way,
};
use wisp_protocol::{AgentEvent, ClientCommand, RejectKind, RequestId, Secret, VariableName};
use wisp_render::chrome::{account, endpoint_card as words};
use wisp_render::theme::glyphs::GlyphSet;
use wisp_render::transcript::copy;
use wisp_tui::app::endpoint_card::{AuthEntry, CardSetup, ProviderEntry, endpoint_facts_of};
use wisp_tui::app::highlight::Start;
use wisp_tui::app::setup::{self, Ask, Opened, Opening, SessionStart, SetupChannels};
use wisp_tui::app::{Exit, Options, Session, SessionFacts, run};
use wisp_tui::term::{Input, Key, KeyCode, Mouse, MouseKind};

use super::{Screen, eventually, facts, key, options, real_wait};
use crate::Drive;

pub(super) const KEY: &str = "sk-ant-api03-CANARYCARDKEY0123456789";
pub(super) const FILE_WORDS: &str = "~/.config/wisp/credentials.json";
const UNCONFIRMED: &str =
    "the keychain didn't confirm the write, so manifest's key may have been saved there";
const AFTER_UNCONFIRMED: &str = "the keychain failed, so nothing was saved. An earlier keychain write this run wasn't confirmed, so Wisp won't keep the key in credentials.json instead";

pub(super) fn card_setup() -> CardSetup {
    CardSetup {
        catalogue: vec![ProviderEntry {
            name: "anthropic".to_owned(),
            title: "Anthropic".to_owned(),
            default_url: "https://api.anthropic.com".to_owned(),
            auths: vec![
                AuthEntry {
                    name: "api_key".to_owned(),
                    words: "x-api-key header (an API key)".to_owned(),
                },
                AuthEntry {
                    name: "bearer".to_owned(),
                    words: "Authorization: Bearer header (a token)".to_owned(),
                },
            ],
            official_auth: "api_key".to_owned(),
        }],
        presence: Vec::new(),
        credentials_shown: FILE_WORDS.to_owned(),
        keys_file: "~/.config/wisp/.env".to_owned(),
        lead: None,
    }
}

pub(super) fn saved(request: RequestId, name: &str, store: StoreKind) -> AgentEvent {
    AgentEvent::EndpointSaved {
        answers: request,
        name: name.to_owned(),
        store,
        check: CheckResult::of(CheckKind::Ok),
        notes: vec!["note: this folder is inside a git work tree".to_owned()],
    }
}

fn keychain_failed(request: RequestId, name: &str) -> AgentEvent {
    AgentEvent::EndpointNotSaved {
        answers: request,
        name: name.to_owned(),
        why: NotSavedReason::KeychainFailed("didn't answer".to_owned()),
    }
}

pub(super) fn summary(name: &str, default: bool) -> EndpointSummary {
    EndpointSummary {
        models: Vec::new(),
        mark: crate::mockup_mark(name),
        name: name.to_owned(),
        provider: "anthropic".to_owned(),
        credential: Some(CredentialKind::ApiKey),
        official: Some(true),
        host: None,
        unavailable: None,
        priced: true,
        default,
        keys_file: false,
        expired: false,
        key_var: None,
        voice_model: None,
        complete: false,
        default_model: None,
    }
}

struct Saved {
    request: RequestId,
    name: String,
    url: Option<String>,
    auth: String,
    store: StoreChoice,
    default: bool,
    key: Secret,
    check: bool,
}

fn save_of(command: ClientCommand) -> Saved {
    match command {
        ClientCommand::SaveEndpoint {
            request,
            endpoint,
            key,
            check,
        } => Saved {
            request,
            name: endpoint.name,
            url: endpoint.url.map(|u| u.expose().to_owned()),
            auth: endpoint.auth,
            store: endpoint.store,
            default: endpoint.default,
            key,
            check,
        },
        other => panic!("not a save: {other:?}"),
    }
}

struct FirstRun {
    screen: Screen,
    inputs: mpsc::UnboundedSender<Input>,
    events: Option<mpsc::Sender<AgentEvent>>,
    commands: mpsc::Receiver<ClientCommand>,
    start: Option<oneshot::Sender<Result<SessionStart, String>>>,
}

impl FirstRun {
    fn official(&self) {
        self.key(KeyCode::Enter);
        for c in KEY.chars() {
            self.key(KeyCode::Char(c));
        }
        self.key(KeyCode::Enter).key(KeyCode::Enter);
    }

    async fn command(&mut self) -> ClientCommand {
        settle().await;
        self.commands.try_recv().unwrap()
    }

    async fn next(&mut self) -> Saved {
        save_of(self.command().await)
    }

    async fn signed(&self, request: RequestId, outcome: AuthOutcome) {
        let event = AuthEvent { request, outcome };
        self.emit(AgentEvent::SubscriptionAuth { event }).await;
    }

    async fn signs_in(&mut self, alias: &str, again: bool) -> RequestId {
        let ClientCommand::SubscriptionAuth {
            command:
                AuthCommand::Start {
                    request: login,
                    provider: SubscriptionProvider::Claude,
                    alias: Some(named),
                    reauthorize,
                },
        } = self.command().await
        else {
            panic!("expected a sign-in to start")
        };
        assert_eq!((named.as_str(), reauthorize), (alias, again));
        let challenge = PrivateChallenge {
            login,
            provider: SubscriptionProvider::Claude,
            alias: alias.to_owned(),
            kind: ChallengeKind::ManualCode,
            authorization_url: Secret::new("https://claude.example/a?state=CANARY"),
            expires_in_secs: 600,
            opened: true,
        };
        self.signed(login, AuthOutcome::Challenge { challenge })
            .await;
        self.input(Input::Paste("CODE_CANARY#STATE_CANARY".to_owned()))
            .key(KeyCode::Enter);
        let ClientCommand::SubscriptionAuth {
            command: AuthCommand::Finish { request, .. },
        } = self.command().await
        else {
            panic!("expected the code sent")
        };
        request
    }

    fn ready(alias: &str) -> AuthOutcome {
        AuthOutcome::Ready {
            endpoint: SubscriptionEndpoint {
                alias: alias.to_owned(),
                provider: SubscriptionProvider::Claude,
                storage: Some(StoreKind::File),
                is_default: true,
                state: LocalState::Working,
            },
            previous_cleanup_pending: false,
        }
    }

    async fn starts_on(&mut self, account: &str, agents: &[&str]) {
        let ClientCommand::StartOn {
            account: on,
            follow,
            ..
        } = self.command().await
        else {
            panic!("expected the session started")
        };
        assert_eq!(
            (on.as_str(), follow),
            (
                account,
                agents.iter().map(|a| (*a).to_owned()).collect::<Vec<_>>()
            )
        );
    }

    async fn nothing_sent(&mut self) {
        settle().await;
        assert!(self.commands.try_recv().is_err());
    }

    async fn emit(&self, event: AgentEvent) {
        self.events.as_ref().unwrap().send(event).await.unwrap();
        settle().await;
    }

    fn end_events(&mut self) {
        self.events = None;
    }

    fn visible(&self) -> String {
        self.screen.visible().join("\n")
    }

    fn shows(&self, text: &str) -> bool {
        self.screen.visible().iter().any(|l| l.contains(text))
    }
}

impl Drive for FirstRun {
    fn inputs(&self) -> &mpsc::UnboundedSender<Input> {
        &self.inputs
    }
}

async fn settle() {
    sleep(Duration::from_millis(50)).await;
}

fn quiet(options: &mut Options) {
    options.code = Start::Stalled;
}

fn first_options() -> Options {
    let mut options = options(Start::Stalled);
    quiet(&mut options);
    options
}

pub(super) struct Started {
    pub(super) start: SessionStart,
    pub(super) events: mpsc::Sender<AgentEvent>,
    pub(super) commands: mpsc::Receiver<ClientCommand>,
}

pub(super) fn started(facts: SessionFacts, warnings: Vec<String>) -> Started {
    let (commands, sent) = super::without_terminal(64);
    let (events_in, events) = mpsc::channel(256);
    Started {
        start: SessionStart {
            session: Session {
                leave_note: None,
                attached: None,
                alerts: Vec::new(),
                facts,
                events,
                commands,
                resumed: None,
            },
            started: None,
            warnings,
            quit: None,
        },
        events: events_in,
        commands: sent,
    }
}

async fn first_run<D, F>(screen: Screen, options: Options, drive: D) -> Opened
where
    D: FnOnce(FirstRun) -> F,
    F: Future<Output = ()>,
{
    let mut options = options;
    options.typed.insert(0, b'4');
    opened(screen, options, drive, |setup, start| Opening::FirstRun {
        setup,
        card: card_setup(),
        start,
    })
    .await
}

async fn opened<D, F>(
    screen: Screen,
    options: Options,
    drive: D,
    opening: impl FnOnce(SetupChannels, oneshot::Receiver<Result<SessionStart, String>>) -> Opening,
) -> Opened
where
    D: FnOnce(FirstRun) -> F,
    F: Future<Output = ()>,
{
    let (commands, server_commands) = super::without_terminal(16);
    let (server_events, events) = mpsc::channel(64);
    let (started, start) = oneshot::channel();
    let (inputs, received) = mpsc::unbounded_channel();
    let opening = opening(SetupChannels { events, commands }, start);
    let mut console = screen.clone();
    let app = setup::open(&mut console, received, opening, options);
    let open_inputs = inputs.clone();
    let run = FirstRun {
        screen,
        inputs,
        events: Some(server_events),
        commands: server_commands,
        start: Some(started),
    };
    let driver = async move {
        settle().await;
        drive(run).await;
    };
    let (opened, ()) = tokio::join!(app, driver);
    drop(open_inputs);
    opened
}

fn canary_free(screen: &Screen) {
    let seen = screen.seen().join("\n");
    assert!(!seen.contains("CANARY"), "{seen}");
    let main = screen.main_before().join("\n");
    assert!(!main.contains("CANARY"), "{main}");
    let clipboard = format!("{:?}", screen.sent_to_clipboard());
    assert!(!clipboard.contains("CANARY"), "{clipboard}");
}

#[tokio::test(start_paused = true)]
async fn a_first_run_quit_with_nothing_sent_cancels_setup() {
    for quit in [
        key(KeyCode::Esc),
        Input::Key(Key::ctrl('c')),
        Input::Key(Key::ctrl('d')),
    ] {
        let screen = Screen::new(30, 100);
        let opened = first_run(screen.clone(), first_options(), |mut s| async move {
            assert!(s.shows(words::SETUP_TITLE), "{}", s.visible());
            assert!(
                s.shows(&copy::esc(copy::QUIT_WORD).text()),
                "{}",
                s.visible()
            );
            s.key(KeyCode::Enter).input(Input::Paste(KEY.to_owned()));
            if quit == key(KeyCode::Esc) {
                s.input(key(KeyCode::Esc));
                settle().await;
                assert!(s.shows("Sign in to start"), "{}", s.visible());
            }
            s.input(quit);
            s.nothing_sent().await;
        })
        .await;
        assert_eq!(opened.exit.unwrap(), Exit::SetupCancelled);
        assert!(opened.warnings.is_empty());
        canary_free(&screen);
    }
}

#[tokio::test(start_paused = true)]
async fn the_first_save_starts_the_session_whichever_comes_first_the_start_or_the_answer() {
    for start_first in [true, false] {
        let screen = Screen::new(30, 100);
        let opened = first_run(screen.clone(), first_options(), |mut s| async move {
            s.official();
            let save = s.next().await;
            assert_eq!(save.request, RequestId(1));
            assert_eq!(save.name, "anthropic");
            assert_eq!(save.auth, "api_key");
            assert_eq!(save.url, None);
            assert_eq!(save.store, StoreChoice::Auto);
            assert!(save.default && save.check);
            assert_eq!(save.key.expose(), KEY);
            assert!(
                s.shows(&words::saving_words(GlyphSet::Unicode)),
                "{}",
                s.visible()
            );
            let Started {
                start,
                events,
                commands,
            } = started(facts(), vec!["wisp: a start warning".to_owned()]);
            let deliver = s.start.take().unwrap();
            if start_first {
                assert!(deliver.send(Ok(start)).is_ok());
                settle().await;
                assert!(s.shows(words::SETUP_TITLE), "{}", s.visible());
                s.emit(saved(RequestId(1), "anthropic", StoreKind::Keyring))
                    .await;
            } else {
                s.emit(saved(RequestId(1), "anthropic", StoreKind::Keyring))
                    .await;
                assert!(s.shows("saved in the keychain"), "{}", s.visible());
                assert!(s.shows("inside a git work tree"), "{}", s.visible());
                assert!(deliver.send(Ok(start)).is_ok());
            }
            s.end_events();
            settle().await;
            assert!(!s.shows(words::SETUP_TITLE), "{}", s.visible());
            assert!(s.shows("wisp: a start warning"), "{}", s.visible());
            drop((events, commands));
        })
        .await;
        assert!(
            screen
                .seen()
                .iter()
                .any(|l| l.contains("saved in the keychain"))
        );
        assert_eq!(
            opened.warnings,
            ["note: this folder is inside a git work tree"]
        );
        canary_free(&screen);
    }
    let first_lines = |s: &FirstRun, done: &str| {
        let lines = s.screen.visible();
        let at = |text: &str| lines.iter().position(|l| l.contains(text));
        let signed = at(&format!("{done} \u{b7} Claude subscription \u{b7} claude"));
        assert!(signed.is_some(), "{}", s.visible());
        assert!(signed < at("wisp: a start warning"), "{}", s.visible());
    };
    for quit in [false, true] {
        let screen = Screen::new(30, 100);
        let opened = first_run(screen.clone(), first_options(), |mut s| async move {
            s.key(KeyCode::Esc).key(KeyCode::Char('1'));
            let finish = s.signs_in("claude", false).await;
            if quit {
                s.key(KeyCode::Esc);
                settle().await;
                assert!(s.commands.recv().await.is_none());
                s.signed(finish, FirstRun::ready("claude")).await;
                s.key(KeyCode::Esc);
                settle().await;
                drop(s.events.take());
                return;
            }
            s.signed(finish, FirstRun::ready("claude")).await;
            s.starts_on("claude", &[]).await;
            let Started {
                start,
                events,
                commands,
            } = started(facts(), vec!["wisp: a start warning".to_owned()]);
            assert!(s.start.take().unwrap().send(Ok(start)).is_ok());
            s.end_events();
            settle().await;
            first_lines(&s, "Signed in");
            drop((events, commands));
        })
        .await;
        let saved = Exit::SetupSaved {
            name: "claude".to_owned(),
            forced: true,
        };
        assert_eq!(opened.exit.is_ok_and(|exit| exit == saved), quit);
        canary_free(&screen);
    }
    let expired = EndpointSummary {
        credential: Some(CredentialKind::Other("anthropic_subscription".to_owned())),
        expired: true,
        ..summary("claude", true)
    };
    for (resumed, quit) in [(false, false), (true, false), (true, true)] {
        let ask = Ask {
            lead: account::Said::SignInExpired(account::Who::Account(
                "claude".to_owned(),
                crate::mockup_mark("claude"),
            )),
            resumed,
            from: None,
            ways: Vec::new(),
            facts: SessionFacts {
                endpoint_name: Some("claude".to_owned()),
                endpoints: vec![endpoint_facts_of(&expired)],
                ..facts()
            },
            size: None,
            back: None,
            again: Some("claude".to_owned()),
        };
        let screen = Screen::new(30, 100);
        let drive = move |mut s: FirstRun| async move {
            settle().await;
            assert!(s.shows("Sign in again to ● claude"), "{}", s.visible());
            assert!(s.shows("● claude's sign-in expired"), "{}", s.visible());
            assert_eq!(s.shows(account::RAN_ON_IT), resumed, "{}", s.visible());
            s.key(KeyCode::Enter);
            let finish = s.signs_in("claude", true).await;
            if quit {
                s.key(KeyCode::Esc);
                settle().await;
                assert!(s.commands.recv().await.is_none());
                s.signed(finish, FirstRun::ready("claude")).await;
                s.key(KeyCode::Esc);
                settle().await;
                drop(s.events.take());
                return;
            }
            s.signed(finish, FirstRun::ready("claude")).await;
            s.starts_on("claude", &[]).await;
            let Started {
                start,
                events,
                commands,
            } = started(facts(), vec!["wisp: a start warning".to_owned()]);
            assert!(s.start.take().unwrap().send(Ok(start)).is_ok());
            s.end_events();
            settle().await;
            first_lines(&s, "Signed in again");
            drop((events, commands));
        };
        let opened = opened(screen.clone(), first_options(), drive, |setup, start| {
            Opening::Choose {
                setup,
                card: card_setup(),
                start,
                ask: Box::new(ask),
            }
        })
        .await;
        let saved = Exit::SetupSaved {
            name: "claude".to_owned(),
            forced: true,
        };
        assert_eq!(opened.exit.is_ok_and(|exit| exit == saved), quit);
        canary_free(&screen);
    }
}

#[tokio::test(start_paused = true)]
async fn on_the_start_and_resume_cards_a_digit_or_click_only_marks_a_row_that_starts_charging() {
    let account = |name: &str, credential: CredentialKind| {
        endpoint_facts_of(&EndpointSummary {
            credential: Some(credential),
            ..summary(name, false)
        })
    };
    let subscription = || CredentialKind::Other("anthropic_subscription".to_owned());
    let runs = [
        (false, true, false),
        (false, false, false),
        (true, true, false),
        (true, false, false),
        (true, false, true),
    ];
    for (resumed, charging, picked) in runs {
        let ask = Ask {
            lead: account::Said::SignInExpired(account::Who::Account(
                "claude".to_owned(),
                crate::mockup_mark("claude"),
            )),
            resumed,
            from: Some(wisp_protocol::Billing::AnthropicPlan),
            ways: vec![
                Way {
                    chat: Some("claude-api".to_owned()),
                    follow: vec!["coder".to_owned()],
                    stay: Vec::new(),
                    charges: true,
                },
                Way {
                    chat: Some("claude-work".to_owned()),
                    ..Way::default()
                },
            ],
            facts: SessionFacts {
                endpoint_name: Some("claude".to_owned()),
                endpoints: vec![
                    account("claude", subscription()),
                    account("claude-api", CredentialKind::ApiKey),
                    account("claude-work", subscription()),
                ],
                ..facts()
            },
            size: None,
            back: picked.then(|| (super::chats::two(), "b".to_owned())),
            again: None,
        };
        let drive = move |mut s: FirstRun| async move {
            settle().await;
            let esc = copy::esc(if picked {
                copy::BACK_WORD
            } else {
                copy::QUIT_WORD
            });
            assert!(s.shows(&esc.text()), "{}", s.visible());
            if picked {
                s.key(KeyCode::Esc);
                settle().await;
                assert!(s.shows("Draft the release notes"), "{}", s.visible());
                s.key(KeyCode::Enter);
                return;
            }
            if !charging {
                s.key(KeyCode::Enter);
                s.starts_on("claude-work", &[]).await;
                return;
            }
            let lines = s.screen.visible();
            let row = lines.iter().position(|l| l.contains("1 ● claude-api"));
            let row = u16::try_from(row.unwrap()).unwrap();
            s.key(KeyCode::Char('1'));
            for kind in [MouseKind::Press, MouseKind::Release] {
                s.input(Input::Mouse(Mouse {
                    kind,
                    column: 4,
                    row,
                    modified: false,
                }));
            }
            s.nothing_sent().await;
            s.key(KeyCode::Enter);
            s.starts_on("claude-api", &["coder"]).await;
        };
        let screen = Screen::new(30, 100);
        let opened = opened(screen, first_options(), drive, |setup, start| {
            Opening::Choose {
                setup,
                card: card_setup(),
                start,
                ask: Box::new(ask),
            }
        })
        .await;
        if picked {
            assert_eq!(opened.exit.ok(), Some(Exit::Resume("b".to_owned())));
        }
    }
}

#[tokio::test(start_paused = true)]
async fn the_started_session_gets_the_commands_and_typed_ahead_keys_stay_in_the_card() {
    let screen = Screen::new(30, 100);
    let mut options = first_options();
    options.typed = b"2".to_vec();
    let (handed, mut take) = oneshot::channel::<mpsc::Receiver<ClientCommand>>();
    let opened = first_run(screen.clone(), options, |mut s| async move {
        assert!(s.shows(words::URL_LABEL), "{}", s.visible());
        s.key(KeyCode::Enter);
        s.input(Input::Paste("https://gw.example.com/anthropic".to_owned()));
        s.key(KeyCode::Enter).key(KeyCode::Down).key(KeyCode::Enter);
        s.input(Input::Paste(KEY.to_owned()));
        s.key(KeyCode::Enter).key(KeyCode::Enter);
        let save = s.next().await;
        assert_eq!(save.name, "gw");
        assert_eq!(save.auth, "bearer");
        assert_eq!(
            save.url.as_deref(),
            Some("https://gw.example.com/anthropic")
        );
        s.emit(saved(save.request, "gw", StoreKind::File)).await;
        assert!(
            s.shows(&format!("saved in {FILE_WORDS}")),
            "{}",
            s.visible()
        );
        let session = started(facts(), Vec::new());
        assert!(s.start.take().unwrap().send(Ok(session.start)).is_ok());
        s.end_events();
        settle().await;
        s.input(Input::Paste("hello".to_owned()));
        s.key(KeyCode::Enter);
        settle().await;
        assert!(handed.send(session.commands).is_ok());
        drop(session.events);
    })
    .await;
    assert_eq!(opened.exit.unwrap(), Exit::Ended);
    let mut commands = take.try_recv().unwrap();
    match commands.try_recv().unwrap() {
        ClientCommand::Send { text, .. } => assert_eq!(text, "hello"),
        other => panic!("{other:?}"),
    }
    canary_free(&screen);
}

#[tokio::test(start_paused = true)]
async fn quitting_during_a_save_waits_for_its_answer() {
    let cases: [(AgentEvent, Exit); 3] = [
        (
            saved(RequestId(1), "anthropic", StoreKind::Keyring),
            Exit::SetupSaved {
                name: "anthropic".to_owned(),
                forced: false,
            },
        ),
        (
            keychain_failed(RequestId(1), "anthropic"),
            Exit::SetupCancelled,
        ),
        (
            AgentEvent::endpoint_refused(Some(RequestId(1)), "the lock wait ran out".to_owned()),
            Exit::SetupCancelled,
        ),
    ];
    for (answer, exit) in cases {
        let screen = Screen::new(30, 100);
        let opened = first_run(screen.clone(), first_options(), |mut s| async move {
            s.official();
            s.next().await;
            s.key(KeyCode::Esc);
            settle().await;
            assert!(s.shows(words::QUIT_AFTER_SAVE), "{}", s.visible());
            assert!(s.commands.recv().await.is_none());
            s.emit(answer).await;
            s.end_events();
            drop(s.start.take());
        })
        .await;
        assert_eq!(opened.exit.unwrap(), exit);
    }
}

#[tokio::test(start_paused = true)]
async fn a_second_quit_ends_at_once() {
    let screen = Screen::new(30, 100);
    let opened = first_run(screen.clone(), first_options(), |mut s| async move {
        s.official();
        s.next().await;
        s.ctrl('c');
        settle().await;
        s.ctrl('c');
        settle().await;
        drop(s.events.take());
    })
    .await;
    assert_eq!(opened.exit.unwrap(), Exit::SaveAbandoned);

    let screen = Screen::new(30, 100);
    let opened = first_run(screen.clone(), first_options(), |mut s| async move {
        s.official();
        s.next().await;
        s.key(KeyCode::Esc);
        s.emit(saved(RequestId(1), "anthropic", StoreKind::Keyring))
            .await;
        s.key(KeyCode::Esc);
        settle().await;
        drop(s.events.take());
    })
    .await;
    assert_eq!(
        opened.exit.unwrap(),
        Exit::SetupSaved {
            name: "anthropic".to_owned(),
            forced: true,
        }
    );

    let screen = Screen::new(30, 100);
    let opened = first_run(screen.clone(), first_options(), |mut s| async move {
        s.official();
        s.next().await;
        s.emit(saved(RequestId(1), "anthropic", StoreKind::Keyring))
            .await;
        s.starts_on("anthropic", &[]).await;
        s.key(KeyCode::Esc);
        settle().await;
        s.ctrl('c');
        settle().await;
        drop(s.events.take());
    })
    .await;
    assert_eq!(
        opened.exit.unwrap(),
        Exit::SetupSaved {
            name: "anthropic".to_owned(),
            forced: true,
        }
    );
}

#[tokio::test(start_paused = true)]
async fn a_save_that_never_answers_is_abandoned_when_the_wait_ends() {
    let screen = Screen::new(30, 100);
    let opened = first_run(screen.clone(), first_options(), |mut s| async move {
        s.official();
        s.next().await;
        s.key(KeyCode::Esc);
        sleep(setup::SAVE_WAIT + Duration::from_secs(1)).await;
        drop(s.events.take());
    })
    .await;
    assert_eq!(opened.exit.unwrap(), Exit::SaveAbandoned);
}

#[tokio::test(start_paused = true)]
async fn setup_ends_as_the_server_does_when_nothing_quit() {
    let screen = Screen::new(30, 100);
    let opened = first_run(screen, first_options(), |mut s| async move {
        s.end_events();
        drop(s.start.take());
    })
    .await;
    assert_eq!(opened.exit.unwrap(), Exit::Ended);

    let screen = Screen::new(30, 100);
    let opened = first_run(screen, first_options(), |mut s| async move {
        s.official();
        s.next().await;
        s.emit(saved(RequestId(1), "anthropic", StoreKind::Keyring))
            .await;
        let start = s.start.take().unwrap();
        assert!(
            start
                .send(Err("the provider wouldn't connect".to_owned()))
                .is_ok()
        );
        s.end_events();
    })
    .await;
    assert_eq!(
        opened.exit.unwrap(),
        Exit::StartFailed("the provider wouldn't connect".to_owned())
    );
    assert_eq!(
        opened.warnings,
        ["note: this folder is inside a git work tree"]
    );
}

#[tokio::test(start_paused = true)]
async fn a_keychain_that_is_down_offers_the_file_once_per_run_across_setup_and_the_session() {
    let screen = Screen::new(30, 100);
    let opened = first_run(screen.clone(), first_options(), |mut s| async move {
        s.official();
        let first = s.next().await;
        s.emit(keychain_failed(first.request, "anthropic")).await;
        let question = words::keychain_question("didn't answer", FILE_WORDS, false);
        assert!(s.shows("Keep this key in"), "{}", s.visible());
        assert!(question.contains(FILE_WORDS));
        s.key(KeyCode::Char('1'));
        let again = s.next().await;
        assert_eq!(again.request, RequestId(2));
        assert_eq!(again.store, StoreChoice::File);
        assert!(!again.check);
        assert_eq!(again.key.expose(), KEY);
        s.emit(saved(again.request, "anthropic", StoreKind::File))
            .await;
        let Started {
            start,
            events,
            mut commands,
        } = started(on(&["anthropic"]), Vec::new());
        assert!(s.start.take().unwrap().send(Ok(start)).is_ok());
        s.end_events();
        settle().await;
        s.input(Input::Paste("/endpoint add".to_owned()));
        s.key(KeyCode::Enter);
        settle().await;
        assert!(s.shows(words::ADD_TITLE), "{}", s.visible());
        s.key(KeyCode::Enter);
        s.input(Input::Paste(KEY.to_owned()));
        s.key(KeyCode::Enter).key(KeyCode::Enter);
        settle().await;
        let add = save_of(commands.try_recv().unwrap());
        assert_eq!(add.name, "anthropic-2");
        assert!(!add.default);
        events
            .send(keychain_failed(add.request, "anthropic-2"))
            .await
            .unwrap();
        settle().await;
        assert!(!s.shows("Keep this key in"), "{}", s.visible());
        assert!(
            s.shows(&words::keychain_stopped("didn't answer")),
            "{}",
            s.visible()
        );
        drop((events, commands));
    })
    .await;
    assert_eq!(opened.exit.unwrap(), Exit::Ended);
    canary_free(&screen);
}

#[tokio::test(start_paused = true)]
async fn an_unconfirmed_keychain_write_shows_its_reason_and_never_offers_the_file() {
    let screen = Screen::new(30, 100);
    let opened = first_run(screen.clone(), first_options(), |mut s| async move {
        s.official();
        let save = s.next().await;
        s.emit(AgentEvent::Rejected {
            reason: UNCONFIRMED.to_owned(),
            id: None,
            kind: Some(RejectKind::Endpoint),
            request: Some(save.request),
        })
        .await;
        assert!(s.shows(UNCONFIRMED), "{}", s.visible());
        assert!(!s.shows("Keep this key in"), "{}", s.visible());
        assert!(!s.shows(copy::YES), "{}", s.visible());
        s.key(KeyCode::Enter);
        let again = s.next().await;
        assert_eq!(again.request, RequestId(2));
        assert_eq!(again.store, StoreChoice::Auto);
        s.emit(AgentEvent::Rejected {
            reason: AFTER_UNCONFIRMED.to_owned(),
            id: None,
            kind: Some(RejectKind::Endpoint),
            request: Some(again.request),
        })
        .await;
        assert!(s.shows("An earlier keychain write"), "{}", s.visible());
        assert!(!s.shows("Keep this key in"), "{}", s.visible());
        assert!(!s.shows(copy::YES), "{}", s.visible());
        s.key(KeyCode::Enter);
        let third = s.next().await;
        assert_eq!(third.store, StoreChoice::Auto);
        s.key(KeyCode::Esc);
        s.emit(AgentEvent::Rejected {
            reason: AFTER_UNCONFIRMED.to_owned(),
            id: None,
            kind: Some(RejectKind::Endpoint),
            request: Some(third.request),
        })
        .await;
        s.end_events();
        drop(s.start.take());
    })
    .await;
    assert_eq!(opened.exit.unwrap(), Exit::SetupCancelled);
}

#[tokio::test(start_paused = true)]
async fn keys_typed_while_a_save_waits_on_the_lock_change_nothing() {
    let screen = Screen::new(30, 100);
    let opened = first_run(screen.clone(), first_options(), |mut s| async move {
        s.official();
        s.next().await;
        let before = s.visible();
        for c in "typed while waiting".chars() {
            s.key(KeyCode::Char(c));
        }
        s.input(Input::Paste("sk-ant-CANARYPASTEDLATE".to_owned()));
        s.key(KeyCode::Enter).key(KeyCode::Tab);
        s.nothing_sent().await;
        assert_eq!(s.visible(), before);
        assert!(
            s.shows(&words::saving_words(GlyphSet::Unicode)),
            "{}",
            s.visible()
        );
        s.emit(saved(RequestId(1), "anthropic", StoreKind::Keyring))
            .await;
        s.end_events();
        drop(s.start.take());
    })
    .await;
    assert_eq!(opened.exit.unwrap(), Exit::Ended);
    canary_free(&screen);
}

async fn adding<D, F>(screen: Screen, facts: SessionFacts, drive: D) -> Exit
where
    D: FnOnce(Adding) -> F,
    F: Future<Output = ()>,
{
    adding_with(screen, facts, options(Start::Stalled), drive).await
}

async fn adding_with<D, F>(
    screen: Screen,
    facts: SessionFacts,
    mut options: Options,
    drive: D,
) -> Exit
where
    D: FnOnce(Adding) -> F,
    F: Future<Output = ()>,
{
    let (commands, sent) = super::without_terminal(64);
    let (events_in, events) = mpsc::channel(256);
    let (inputs, received) = mpsc::unbounded_channel();
    let session = Session {
        leave_note: None,
        attached: None,
        alerts: Vec::new(),
        facts,
        events,
        commands,
        resumed: None,
    };
    options.card_setup.get_or_insert_with(card_setup);
    let mut console = screen.clone();
    let app = run(&mut console, received, session, options);
    let open_inputs = inputs.clone();
    let agent = Adding {
        screen,
        inputs,
        events: events_in,
        commands: sent,
    };
    let driver = async move {
        settle().await;
        drive(agent).await;
    };
    let (exit, ()) = tokio::join!(app, driver);
    drop(open_inputs);
    exit.unwrap()
}

struct Adding {
    screen: Screen,
    inputs: mpsc::UnboundedSender<Input>,
    events: mpsc::Sender<AgentEvent>,
    commands: mpsc::Receiver<ClientCommand>,
}

impl Adding {
    async fn emit(&self, event: AgentEvent) {
        self.events.send(event).await.unwrap();
        settle().await;
    }

    async fn next(&mut self) -> ClientCommand {
        settle().await;
        self.commands.try_recv().unwrap()
    }

    async fn describe(&mut self) {
        let ClientCommand::DescribeSettings { request } = self.next().await else {
            panic!("expected DescribeSettings");
        };
        self.emit(super::described(request.0)).await;
        settle().await;
    }

    fn shows(&self, text: &str) -> bool {
        self.screen.visible().iter().any(|l| l.contains(text))
    }

    fn visible(&self) -> String {
        self.screen.visible().join("\n")
    }
}

impl Drive for Adding {
    fn inputs(&self) -> &mpsc::UnboundedSender<Input> {
        &self.inputs
    }
}

fn on(names: &[&str]) -> SessionFacts {
    SessionFacts {
        endpoint_name: names.first().map(|n| (*n).to_owned()),
        endpoints: names
            .iter()
            .enumerate()
            .map(|(i, name)| endpoint_facts_of(&summary(name, i == 0)))
            .collect(),
        ..facts()
    }
}

#[tokio::test(start_paused = true)]
async fn endpoint_add_opens_the_card_and_the_key_never_reaches_the_composer_or_history() {
    let screen = Screen::new(30, 100);
    let exit = adding(screen.clone(), facts(), |s| async move {
        s.say("/endpoint add");
        settle().await;
        assert!(s.shows(words::ADD_TITLE), "{}", s.visible());
        assert!(
            s.shows(&copy::esc(copy::CLOSE_WORD).text()),
            "{}",
            s.visible()
        );
        assert!(!s.shows(words::SETUP_TITLE), "{}", s.visible());
        s.key(KeyCode::Enter);
        for c in KEY.chars() {
            s.key(KeyCode::Char(c));
        }
        s.input(Input::Paste(KEY.to_owned()));
        settle().await;
        assert!(
            s.shows("sk-ant-api03-\u{2026}************\u{2026}6789  36 characters"),
            "{}",
            s.visible()
        );
        s.key(KeyCode::Esc);
        settle().await;
        assert!(!s.shows(words::ADD_TITLE), "{}", s.visible());
        s.key(KeyCode::Up);
        settle().await;
        assert!(s.shows("/endpoint add"), "{}", s.visible());
        s.key(KeyCode::Up);
        settle().await;
    })
    .await;
    assert_eq!(exit, Exit::Ended);
    canary_free(&screen);
}

#[tokio::test(start_paused = true)]
async fn account_add_opens_the_card_and_a_save_updates_the_picker() {
    let screen = Screen::new(30, 100);
    let exit = adding(
        screen.clone(),
        on(&["direct", "work"]),
        |mut s| async move {
            s.say("/account add");
            settle().await;
            assert!(s.shows(words::ADD_TITLE), "{}", s.visible());
            s.key(KeyCode::Enter);
            s.input(Input::Paste(KEY.to_owned()));
            s.key(KeyCode::Enter).key(KeyCode::Enter);
            let save = save_of(s.next().await);
            assert_eq!(save.name, "anthropic");
            assert!(!save.default);
            s.emit(AgentEvent::EndpointsChanged {
                endpoints: vec![
                    summary("direct", true),
                    summary("work", false),
                    summary("anthropic", false),
                ],
            })
            .await;
            s.emit(saved(save.request, "anthropic", StoreKind::Keyring))
                .await;
            assert!(!s.shows(words::ADD_TITLE), "{}", s.visible());
            assert!(
                s.shows(
                    "Saved \"anthropic\" in the keychain. Switch to it with /account anthropic."
                ),
                "{}",
                s.visible()
            );
            assert!(s.shows("inside a git work tree"), "{}", s.visible());
            s.say("/endpoint");
            s.describe().await;
            for c in "anth".chars() {
                s.key(KeyCode::Char(c));
            }
            settle().await;
            assert!(
                s.screen.visible().iter().any(|l| l.contains("anthropic")
                    && !l.contains("Saved")
                    && !l.contains("/endpoint")),
                "{}",
                s.visible()
            );
            s.key(KeyCode::Esc);
            settle().await;
            s.input(Input::Key(Key::ctrl('d')));
        },
    )
    .await;
    assert_eq!(exit, Exit::Quit);
    canary_free(&screen);
}

#[tokio::test(start_paused = true)]
async fn closing_the_card_during_a_save_keeps_its_answer_as_a_note() {
    let screen = Screen::new(30, 100);
    let exit = adding(screen.clone(), facts(), |mut s| async move {
        s.say("/endpoint add");
        settle().await;
        s.key(KeyCode::Enter);
        s.input(Input::Paste(KEY.to_owned()));
        s.key(KeyCode::Enter).key(KeyCode::Enter);
        let first = save_of(s.next().await);
        s.key(KeyCode::Esc);
        settle().await;
        assert!(!s.shows(words::ADD_TITLE), "{}", s.visible());
        s.emit(keychain_failed(first.request, "anthropic")).await;
        assert!(
            s.shows("\"anthropic\" wasn't saved: The keychain didn't answer"),
            "{}",
            s.visible()
        );
        s.say("/endpoint add");
        settle().await;
        s.key(KeyCode::Enter);
        s.input(Input::Paste(KEY.to_owned()));
        s.key(KeyCode::Enter).key(KeyCode::Enter);
        let second = save_of(s.next().await);
        assert_ne!(second.request, first.request);
        s.key(KeyCode::Esc);
        settle().await;
        s.emit(saved(second.request, "anthropic", StoreKind::Keyring))
            .await;
        assert!(
            s.shows("Saved \"anthropic\" in the keychain."),
            "{}",
            s.visible()
        );
        s.say("/endpoint add");
        settle().await;
        s.key(KeyCode::Enter);
        s.input(Input::Paste(KEY.to_owned()));
        s.key(KeyCode::Enter).key(KeyCode::Enter);
        let third = save_of(s.next().await);
        s.key(KeyCode::Esc);
        settle().await;
        s.emit(AgentEvent::Rejected {
            reason: "the name is taken".to_owned(),
            id: None,
            kind: Some(RejectKind::Endpoint),
            request: Some(third.request),
        })
        .await;
        assert!(
            s.shows("\"anthropic\" wasn't saved: the name is taken"),
            "{}",
            s.visible()
        );
        assert!(!s.shows("This message wasn't sent"), "{}", s.visible());
        s.input(Input::Key(Key::ctrl('d')));
    })
    .await;
    assert_eq!(exit, Exit::Quit);
    canary_free(&screen);
}

#[tokio::test(start_paused = true)]
async fn the_card_names_keys_found_where_wisp_will_not_read_them() {
    let mut options = options(Start::Stalled);
    let mut setup = card_setup();
    setup.presence = vec![VariableName::parse("ANTHROPIC_API_KEY").unwrap()];
    options.card_setup = Some(setup);
    adding_with(Screen::new(30, 100), facts(), options, |s| async move {
        s.say("/endpoint add");
        settle().await;
        let shown = s.screen.visible().join("\n");
        assert!(shown.contains("ANTHROPIC_API_KEY"), "{shown}");
        assert!(shown.contains("wisp account import"), "{shown}");
        s.ctrl('d').ctrl('d');
    })
    .await;
}

#[tokio::test(start_paused = true)]
async fn right_click_never_pastes_into_the_add_an_endpoint_card() {
    let dir = super::images::Dir::new("right-card");
    dir.put("text", b"pasted words");
    let options = super::images::right_pasting(&dir, true);
    let screen = Screen::new(30, 100);
    let exit = adding_with(screen.clone(), facts(), options, |s| async move {
        s.say("/endpoint add");
        settle().await;
        assert!(s.shows(words::ADD_TITLE), "{}", s.visible());
        s.input(super::images::right_click());
        real_wait(300).await;
        assert!(super::images::calls(&dir).is_empty());
        assert!(!s.shows("pasted words"), "{}", s.visible());
        s.key(KeyCode::Esc);
        settle().await;
        s.input(super::images::right_click());
        eventually(|| s.screen.caret_line().contains("pasted words")).await;
        assert!(
            s.screen.caret_line().contains("pasted words"),
            "{}",
            s.visible()
        );
        s.input(Input::Key(Key::ctrl('c')));
        s.input(Input::Key(Key::ctrl('c')));
        s.input(Input::Key(Key::ctrl('c')));
    })
    .await;
    assert_eq!(exit, Exit::Quit);
}

#[tokio::test(start_paused = true)]
async fn the_agent_tree_is_asked_of_the_started_session_never_of_setup() {
    let screen = Screen::new(30, 100);
    let mut options = first_options();
    options.list_agents = true;
    let (handed, mut take) = oneshot::channel::<mpsc::Receiver<ClientCommand>>();
    let opened = first_run(screen.clone(), options, |mut s| async move {
        s.official();
        let save = s.next().await;
        s.emit(saved(save.request, "anthropic", StoreKind::Keyring))
            .await;
        settle().await;
        assert!(matches!(
            s.commands.try_recv(),
            Ok(ClientCommand::StartOn { account, follow, .. }) if account == "anthropic" && follow.is_empty()
        ));
        let session = started(facts(), Vec::new());
        assert!(s.start.take().unwrap().send(Ok(session.start)).is_ok());
        s.end_events();
        settle().await;
        s.nothing_sent().await;
        assert!(handed.send(session.commands).is_ok());
        drop(session.events);
    })
    .await;
    assert_eq!(opened.exit.unwrap(), Exit::Ended);
    let mut commands = take.try_recv().unwrap();
    assert!(
        matches!(
            commands.try_recv().unwrap(),
            ClientCommand::ListAgents { .. }
        ),
        "the started session is asked for the tree"
    );
}
