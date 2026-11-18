use super::*;
use wisp_protocol::endpoint::StoreKind;
use wisp_protocol::settings::{
    CredentialKind, EndpointSummary, Place, SavedValue, SettingValue, SitePath,
};
use wisp_protocol::subscription_auth::{
    AuthCommand, AuthEvent, AuthFailureKind, AuthOutcome, ChallengeKind, LocalState,
    PrivateChallenge, SubscriptionEndpoint, SubscriptionProvider, Way,
};
use wisp_protocol::{AgentEntry, AgentState, ClientCommand, RequestId, Secret};
use wisp_render::chrome::account::{NO_BROWSER, TRIED_BROWSER};
use wisp_tui::app::endpoint_card::endpoint_facts_of;
use wisp_tui::term::clipboard::{Clipboard, Route, SCREEN_BLOCKS, Tools};

const URL: &str = "https://example.invalid/authorize?state=PRIVATE_URL_STATE_CANARY";
const CODE: &str = "CODE_CANARY#STATE_CANARY";
const LINK_TEXT: &str = "\u{2192} example.invalid/";
const DONE: &str = "Signed in \u{b7} Claude subscription \u{b7} \u{25cf} work";

struct AuthRun {
    screen: Screen,
    inputs: mpsc::UnboundedSender<Input>,
    events: mpsc::Sender<AgentEvent>,
    commands: mpsc::Receiver<ClientCommand>,
}

impl AuthRun {
    fn shown(&self) -> String {
        self.screen.visible().join("\n")
    }
    async fn settle(&self) {
        sleep(Duration::from_millis(60)).await;
    }
    async fn next(&mut self) -> AuthCommand {
        loop {
            match self.commands.recv().await.unwrap() {
                ClientCommand::SubscriptionAuth { command } => return command,
                ClientCommand::PlanUsage { .. } | ClientCommand::DescribeSettings { .. } => {}
                other => panic!("unexpected command {other:?}"),
            }
        }
    }
    async fn emit(&self, request: RequestId, outcome: AuthOutcome) {
        if let AuthOutcome::Ready { endpoint, .. } = &outcome {
            let signed_in = account(&endpoint.alias, "subscription", false, false);
            let endpoints = vec![account("claude-api", "key", false, false), signed_in];
            self.events
                .send(AgentEvent::EndpointsChanged { endpoints })
                .await
                .unwrap();
        }
        self.events
            .send(AgentEvent::SubscriptionAuth {
                event: AuthEvent { request, outcome },
            })
            .await
            .unwrap();
        self.settle().await;
    }
    async fn challenge(
        &self,
        login: RequestId,
        provider: SubscriptionProvider,
        url: &str,
        seconds: u64,
    ) {
        self.reply(login, login, provider, url, seconds, false)
            .await;
    }
    async fn reply(
        &self,
        request: RequestId,
        login: RequestId,
        provider: SubscriptionProvider,
        url: &str,
        seconds: u64,
        opened: bool,
    ) {
        self.emit(
            request,
            AuthOutcome::Challenge {
                challenge: PrivateChallenge {
                    login,
                    provider,
                    alias: "work".into(),
                    kind: match provider {
                        SubscriptionProvider::ChatGpt | SubscriptionProvider::Antigravity => {
                            ChallengeKind::BrowserLoopback
                        }
                        SubscriptionProvider::Claude => ChallengeKind::ManualCode,
                    },
                    authorization_url: Secret::new(url),
                    expires_in_secs: seconds,
                    opened,
                },
            },
        )
        .await;
    }
}

impl Drive for AuthRun {
    fn inputs(&self) -> &mpsc::UnboundedSender<Input> {
        &self.inputs
    }
}

impl AuthRun {
    fn link_row(&self) -> u16 {
        let row = self
            .screen
            .visible()
            .iter()
            .position(|line| line.contains(LINK_TEXT))
            .unwrap_or_else(|| panic!("no link row: {}", self.shown()));
        u16::try_from(row).unwrap()
    }
    fn mouse(&self, kind: MouseKind, row: u16) {
        self.press(kind, row, false);
    }
    fn press(&self, kind: MouseKind, row: u16, modified: bool) {
        self.inputs
            .send(Input::Mouse(Mouse {
                kind,
                column: 2,
                row,
                modified,
            }))
            .unwrap();
    }
    fn click_link(&self) {
        let row = self.link_row();
        self.mouse(MouseKind::Press, row);
        self.mouse(MouseKind::Release, row);
    }
    fn words(&self) -> String {
        self.screen
            .visible()
            .into_iter()
            .find(|line| line.contains(TRIED_BROWSER) || line.contains(NO_BROWSER))
            .and_then(|line| {
                line.trim()
                    .rsplit_once("   ")
                    .map(|(_, words)| words.to_owned())
            })
            .unwrap_or_default()
    }
    fn status(&self) -> String {
        super::status_line(&self.screen.visible())
    }
}

fn terminal() -> Options {
    let mut opts = options(Start::Stalled);
    opts.clipboard = Clipboard {
        route: Route::Terminal,
        ..Clipboard::none()
    };
    opts
}

fn osc52(text: &str) -> Vec<u8> {
    Osc52::new(text).unwrap().bytes().to_vec()
}

fn endpoint(provider: SubscriptionProvider) -> SubscriptionEndpoint {
    SubscriptionEndpoint {
        alias: "work".into(),
        provider,
        storage: Some(StoreKind::File),
        is_default: false,
        state: LocalState::Working,
    }
}

async fn scenario<F: Future<Output = ()>>(screen: Screen, drive: impl FnOnce(AuthRun) -> F) {
    scenario_with(screen, options(Start::Stalled), drive).await;
}

async fn scenario_with<F: Future<Output = ()>>(
    screen: Screen,
    opts: Options,
    drive: impl FnOnce(AuthRun) -> F,
) {
    scenario_on(screen, opts, facts(), drive).await;
}

async fn scenario_on<F: Future<Output = ()>>(
    screen: Screen,
    opts: Options,
    facts: SessionFacts,
    drive: impl FnOnce(AuthRun) -> F,
) {
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
    let mut console = screen.clone();
    let app = run(&mut console, received, session, opts);
    let driver = async move {
        let s = AuthRun {
            screen,
            inputs,
            events: events_in,
            commands: sent,
        };
        s.settle().await;
        drive(s).await;
    };
    let (exit, ()) = tokio::join!(app, driver);
    assert!(exit.is_ok());
}

#[tokio::test(start_paused = true)]
async fn both_providers_use_real_envelopes_and_reload_never_restarts_oauth() {
    for provider in [SubscriptionProvider::ChatGpt, SubscriptionProvider::Claude] {
        let screen = Screen::new(24, 80);
        scenario(screen, |mut s| async move {
            s.say(if provider == SubscriptionProvider::Claude {
                "/login claude work"
            } else {
                "/login chatgpt work"
            });
            let AuthCommand::Start {
                request: login,
                provider: sent,
                reauthorize,
                ..
            } = s.next().await
            else {
                panic!("start")
            };
            assert_eq!(provider, sent);
            assert!(!reauthorize);
            s.challenge(login, provider, URL, 600).await;
            assert!(s.shown().contains(LINK_TEXT), "{}", s.shown());
            assert!(!s.shown().contains("URL_STATE_CANARY"), "{}", s.shown());
            if provider == SubscriptionProvider::Claude {
                s.paste(CODE);
                s.settle().await;
                assert!(!s.shown().contains("CODE_CANARY"));
                s.key(KeyCode::Enter);
            }
            let AuthCommand::Finish {
                request,
                login: finishing,
                response,
                ..
            } = s.next().await
            else {
                panic!("finish")
            };
            assert_eq!(finishing, login);
            assert_eq!(
                response.as_ref().map(Secret::expose),
                if provider == SubscriptionProvider::Claude {
                    Some(CODE)
                } else {
                    None
                }
            );
            assert!(!format!("{response:?}").contains("CANARY"));
            s.emit(
                request,
                AuthOutcome::SavedNotReady {
                    endpoint: endpoint(provider),
                    previous_cleanup_pending: true,
                },
            )
            .await;
            assert!(s.shown().contains("saved, not usable yet"));
            assert!(!s.shown().contains("URL_STATE_CANARY"));
            s.key(KeyCode::Enter);
            let AuthCommand::Reload { request, alias } = s.next().await else {
                panic!("reload")
            };
            assert_eq!(alias, "work");
            s.emit(
                request,
                AuthOutcome::Ready {
                    endpoint: endpoint(provider),
                    previous_cleanup_pending: false,
                },
            )
            .await;
            let title = match provider {
                SubscriptionProvider::Claude => "Claude subscription",
                SubscriptionProvider::ChatGpt => "ChatGPT subscription",
                SubscriptionProvider::Antigravity => "Antigravity subscription",
            };
            assert!(
                s.shown()
                    .contains(&format!("Signed in \u{b7} {title} \u{b7} \u{25cf} work")),
                "{}",
                s.shown()
            );
            assert!(
                !s.shown()
                    .contains(wisp_render::chrome::account::CLEANUP_WARNING)
            );
            s.key(KeyCode::Enter);
            s.settle().await;
            assert!(
                s.shown()
                    .contains(wisp_render::chrome::account::CLEANUP_WARNING),
                "{}",
                s.shown()
            );
        })
        .await;
    }
}

#[tokio::test(start_paused = true)]
async fn protected_auth_input_never_reaches_history_transcript_or_clipboard_and_stale_replies_are_ignored()
 {
    let screen = Screen::new(24, 80);
    let inspect = screen.clone();
    scenario_with(screen, terminal(), |mut s| async move {
        s.say("/login claude work");
        let AuthCommand::Start { request: login, .. } = s.next().await else {
            panic!("start")
        };
        s.challenge(login, SubscriptionProvider::Claude, URL, 600)
            .await;
        s.paste(CODE);
        s.settle().await;
        assert!(s.shown().contains("24 characters"), "{}", s.shown());
        assert!(!s.shown().contains("CODE_CANARY"));
        s.inputs.send(ctrl('o')).unwrap();
        s.inputs.send(ctrl('y')).unwrap();
        s.inputs.send(wheel(MouseKind::Press)).unwrap();
        s.inputs.send(wheel(MouseKind::Drag)).unwrap();
        s.inputs.send(wheel(MouseKind::Release)).unwrap();
        s.key(KeyCode::Esc);
        let AuthCommand::Cancel {
            request,
            login: cancelled,
        } = s.next().await
        else {
            panic!("cancel")
        };
        assert_eq!(cancelled, login);
        s.emit(request, AuthOutcome::Cancelled { login }).await;
        s.challenge(login, SubscriptionProvider::Claude, "STALE_URL_CANARY", 600)
            .await;
        s.emit(
            login,
            AuthOutcome::Ready {
                endpoint: endpoint(SubscriptionProvider::Claude),
                previous_cleanup_pending: false,
            },
        )
        .await;
        assert!(!s.shown().contains("Signed in"));
        assert!(!s.shown().contains("CANARY"));
        s.key(KeyCode::Enter);
        s.key(KeyCode::Up);
        s.settle().await;
        assert!(!s.shown().contains("CANARY"));
        s.inputs.send(ctrl('u')).unwrap();
        s.inputs.send(ctrl('o')).unwrap();
        s.settle().await;
        if picker_shown(&s.screen.visible()) {
            s.inputs.send(ctrl('o')).unwrap();
            s.settle().await;
        }
        assert!(!s.shown().contains("CANARY"));
    })
    .await;
    assert!(!inspect.seen().join("\n").contains("CODE_CANARY"));
    assert_eq!(inspect.sent_to_clipboard(), vec![osc52(URL)]);
    assert!(!inspect.main_before().join("\n").contains("CANARY"));
}

pub(super) fn account(
    name: &str,
    credential: &str,
    default: bool,
    keys_file: bool,
) -> EndpointSummary {
    EndpointSummary {
        models: Vec::new(),
        mark: crate::mockup_mark(name),
        name: name.to_owned(),
        provider: "anthropic".to_owned(),
        credential: Some(match credential {
            "key" => CredentialKind::ApiKey,
            other => CredentialKind::Other(other.to_owned()),
        }),
        official: Some(true),
        host: None,
        unavailable: None,
        priced: true,
        default,
        keys_file,
        expired: false,
        key_var: None,
        voice_model: None,
        complete: false,
        default_model: None,
    }
}

fn accounts() -> Vec<EndpointSummary> {
    vec![
        account("claude-subscription", "anthropic_subscription", true, false),
        account("claude-work", "anthropic_subscription", false, false),
        account("work-api", "key", false, false),
        account("claude-api", "key", false, true),
    ]
}

fn way(chat: &str, follow: &[&str], stay: &[&str]) -> Way {
    let names = |list: &[&str]| list.iter().map(|n| (*n).to_owned()).collect();
    Way {
        chat: Some(chat.to_owned()),
        follow: names(follow),
        stay: names(stay),
        charges: false,
    }
}

pub(super) fn logged_out(alias: &str, also: &[&str]) -> AuthOutcome {
    serde_json::from_value(serde_json::json!({
        "outcome": "logged_out",
        "alias": alias,
        "also": also,
        "remote_revoked": false,
        "cleanup_pending": false,
    }))
    .unwrap()
}

impl AuthRun {
    async fn described(&mut self, endpoints: Vec<EndpointSummary>) {
        self.settle().await;
        let mut asked = None;
        while let Ok(command) = self.commands.try_recv() {
            if let ClientCommand::DescribeSettings { request } = command {
                asked = Some(request);
            }
        }
        let request = asked.unwrap();
        let AgentEvent::SettingsSnapshot(mut snapshot) = super::described(request.0) else {
            panic!("snapshot")
        };
        let store = wisp_protocol::settings::find("key_store").unwrap();
        snapshot.saved = vec![SavedValue::of(
            SitePath::new(Place::Endpoint("work-api".to_owned()), store),
            SettingValue::Text("file".to_owned()),
            None,
        )];
        snapshot.endpoints = endpoints;
        self.events
            .send(AgentEvent::SettingsSnapshot(snapshot))
            .await
            .unwrap();
        self.settle().await;
    }
    async fn sends_no_logout(&mut self) {
        self.settle().await;
        while let Ok(command) = self.commands.try_recv() {
            assert!(
                !matches!(
                    command,
                    ClientCommand::SubscriptionAuth {
                        command: AuthCommand::Logout { .. }
                    }
                ),
                "{command:?}"
            );
        }
    }
    fn click_on(&self, text: &str) {
        let row = self
            .screen
            .visible()
            .iter()
            .position(|line| line.contains(text))
            .unwrap_or_else(|| panic!("no {text}: {}", self.shown()));
        self.mouse(MouseKind::Press, u16::try_from(row).unwrap());
    }
}

#[tokio::test(start_paused = true)]
async fn logout_refusals_are_verbatim_and_login_status_opens_the_model_list() {
    let facts = SessionFacts {
        endpoint_name: Some("claude-subscription".to_owned()),
        endpoints: accounts().iter().map(endpoint_facts_of).collect(),
        ..super::facts()
    };
    scenario_on(
        Screen::new(30, 120),
        options(Start::Stalled),
        facts,
        |mut s| async move {
            let message =
                "there's no account called work; yours are claude-subscription and claude-work.";
            s.say("/logout work");
            let AuthCommand::Logout {
                request,
                alias,
                confirmed: false,
                ..
            } = s.next().await
            else {
                panic!("logout")
            };
            assert_eq!(alias, "work");
            s.emit(
                request,
                AuthOutcome::Failed {
                    kind: AuthFailureKind::InvalidAlias,
                    message: message.into(),
                    problem: None,
                    same_account: None,
                },
            )
            .await;
            assert!(s.shown().contains(message), "{}", s.shown());
            s.say("/login status");
            s.settle().await;
            let shown = s.shown();
            assert!(
                shown
                    .lines()
                    .any(|l| l.trim_start().starts_with(copy::MODEL_TITLE)
                        && l.contains(wisp_render::chrome::account::REBUILD_TITLE)),
                "{shown}"
            );
            s.key(KeyCode::Up);
            s.settle().await;
            let shown = s.shown();
            let folded = |l: &&str| l.contains("claude-work") && l.contains("opus-5-5");
            assert!(shown.lines().any(|l| folded(&l)), "{shown}");
            s.key(KeyCode::Esc);
            s.settle().await;

            s.say("/logout");
            s.described(accounts()).await;
            let shown = s.shown();
            assert!(shown.contains("Sign out of which account?"), "{shown}");
            assert!(
                shown.contains(
                    "claude-subscription   Claude subscription · this chat · new chats start here"
                ),
                "{shown}"
            );
            let expected = format!(
                "claude-api            {} · from ~/.config/wisp/.env",
                AccountKind::ClaudeApi.label()
            );
            assert!(shown.contains(&expected), "{shown}");
            assert!(shown.contains("↑↓ move · esc cancel"), "{shown}");
            s.events.send(AgentEvent::turn_started(1)).await.unwrap();
            s.events.send(completed()).await.unwrap();
            s.settle().await;
            assert!(s.shown().contains("Sign out of which account?"), "{}", s.shown());
            s.paste("claude-work");
            s.key(KeyCode::Enter);
            s.sends_no_logout().await;
            s.key(KeyCode::Down);
            s.settle().await;
            assert!(
                s.shown().contains("enter choose · esc cancel"),
                "{}",
                s.shown()
            );
            s.key(KeyCode::Enter);
            let AuthCommand::Logout {
                request,
                alias,
                confirmed: false,
                chat: None,
                follow,
                ..
            } = s.next().await
            else {
                panic!("a picked logout")
            };
            assert_eq!((alias.as_str(), follow.len()), ("claude-subscription", 0));
            s.emit(
                request,
                AuthOutcome::LogoutNeeds {
                    presets: Vec::new(),
                    deletes_keys_of: Vec::new(),
                    new_start: Some("claude-work".to_owned()),
                    ways: vec![
                        way("claude-work", &["coder"], &[]),
                        way("work-api", &[], &["coder"]),
                    ],
                },
            )
            .await;
            let shown = s.shown();
            assert!(
                shown.contains("Sign out of \u{25cf} claude-subscription?"),
                "{shown}"
            );
            assert!(
                shown.contains("This chat uses it. Where should it go?"),
                "{shown}"
            );
            assert!(shown.contains("↑↓ move · esc cancel"), "{shown}");
            assert!(!shown.contains("enter sign out"), "{shown}");
            s.key(KeyCode::Char('y'));
            s.click_on("1 \u{25cf} claude-work");
            s.sends_no_logout().await;
            assert!(
                s.shown().contains("enter sign out · ↑↓ move · esc cancel"),
                "{}",
                s.shown()
            );
            s.key(KeyCode::Enter);
            let AuthCommand::Logout {
                request,
                chat,
                follow,
                confirmed: true,
                ..
            } = s.next().await
            else {
                panic!("logout with a way")
            };
            assert_eq!(chat.as_deref(), Some("claude-work"));
            assert_eq!(follow, vec!["coder".to_owned()]);
            s.emit(request, logged_out("claude-subscription", &[]))
                .await;
            assert!(
                s.shown()
                    .contains("✓ Signed out · Claude subscription · ○ claude-subscription"),
                "{}",
                s.shown()
            );

            s.say("/logout");
            s.described(accounts()).await;
            s.key(KeyCode::Char('2'));
            let AuthCommand::Logout {
                request,
                alias,
                confirmed: false,
                ..
            } = s.next().await
            else {
                panic!("a picked subscription")
            };
            assert_eq!(alias, "claude-work");
            s.emit(request, logged_out("claude-work", &[])).await;

            s.say("/logout");
            s.described(accounts()).await;
            s.key(KeyCode::Char('3'));
            let AuthCommand::Logout {
                request,
                alias,
                confirmed: false,
                ..
            } = s.next().await
            else {
                panic!("a picked key")
            };
            assert_eq!(alias, "work-api");
            for agent in [
                super::agents::on_account("/scout", "work-api", 40_000),
                AgentEntry {
                    state: AgentState::Ended,
                    ..super::agents::on_account("/finished", "work-api", 40_000)
                },
            ] {
                s.events.send(super::agents::spawned(agent)).await.unwrap();
            }
            s.emit(
                request,
                AuthOutcome::LogoutNeeds {
                    presets: Vec::new(),
                    deletes_keys_of: vec!["work-api".to_owned()],
                    new_start: None,
                    ways: Vec::new(),
                },
            )
            .await;
            let question = "Its key is deleted from Wisp.";
            let shown = s.shown().replace('\u{a0}', " ");
            assert!(shown.contains(question), "{shown}");
            assert!(
                shown.contains("Moving to \u{25cf} claude-subscription"),
                "{shown}"
            );
            let scout = shown.lines().find(|l| l.trim_start().starts_with("scout "));
            let amount = scout.and_then(|l| l.split_whitespace().last());
            assert!(amount.is_some_and(|a| a.starts_with("~$")), "{shown}");
            assert!(!shown.contains("finished"), "{shown}");
            s.key(KeyCode::Char('1'));
            s.key(KeyCode::Esc);
            s.sends_no_logout().await;
            assert!(!s.shown().contains(question), "{}", s.shown());

            s.say("/logout all");
            s.described(vec![accounts()[0].clone(), accounts()[3].clone()])
                .await;
            assert!(
                s.shown().contains("Sign out of \u{25cf} claude-subscription?"),
                "{}",
                s.shown()
            );
            s.key(KeyCode::Esc);
            s.sends_no_logout().await;

            s.say("/logout all");
            s.described(vec![accounts()[3].clone()]).await;
            let AuthCommand::Logout {
                request,
                alias,
                confirmed: false,
                ..
            } = s.next().await
            else {
                panic!("the keys-file account alone")
            };
            assert_eq!(alias, "claude-api");
            let line =
                "claude-api's key comes from Wisp's keys file: remove ANTHROPIC_API_KEY there.";
            s.emit(
                request,
                AuthOutcome::Failed {
                    kind: AuthFailureKind::InvalidAlias,
                    message: line.into(),
                    problem: None,
                    same_account: None,
                },
            )
            .await;
            assert!(s.shown().contains(line), "{}", s.shown());
            assert!(
                !s.shown().contains("No account is set up."),
                "{}",
                s.shown()
            );

            let coder = super::agents::on_account("/coder", "claude-subscription", 40_000);
            s.events.send(super::agents::spawned(coder)).await.unwrap();
            s.say("/logout all");
            s.described(accounts()).await;
            let AuthCommand::Logout {
                request,
                alias,
                also,
                confirmed: false,
                ..
            } = s.next().await
            else {
                panic!("all")
            };
            assert_eq!((alias.as_str(), also.len()), ("claude-subscription", 2));
            s.emit(
                request,
                AuthOutcome::LogoutNeeds {
                    presets: Vec::new(),
                    deletes_keys_of: vec!["work-api".to_owned()],
                    new_start: Some("claude-api".to_owned()),
                    ways: vec![way("claude-api", &["coder"], &["reviewer"])],
                },
            )
            .await;
            let shown = s.shown().replace('\u{a0}', " ");
            for line in [
                "? Sign out of 3 accounts?",
                "\u{25cb} claude-subscription · \u{25cb} claude-work · \u{25cb} work-api (key deleted)",
                "Moving to \u{25cf} claude-api · API   cache rebuild",
                "Staying behind",
            ] {
                assert!(shown.contains(line), "{line}\n{shown}");
            }
            assert!(!shown.contains("stays: its key comes from"), "{shown}");
            assert!(!shown.contains("New chats start on"), "{shown}");
            let row = shown.lines().find(|l| l.trim_start().starts_with("coder "));
            let amount = row
                .and_then(|l| l.split_whitespace().last())
                .unwrap_or_default();
            assert!(amount.starts_with("~$"), "{shown}");
            let amount = amount.to_owned();
            let reviewer = shown
                .lines()
                .find(|l| l.trim_start().starts_with("reviewer "));
            assert!(
                reviewer.is_some_and(|l| l.trim_end().ends_with("can't move to Claude")),
                "{shown}"
            );
            s.key(KeyCode::Char('1'));
            s.sends_no_logout().await;
            s.key(KeyCode::Enter);
            let AuthCommand::Logout {
                request,
                chat,
                follow,
                confirmed: true,
                also,
                ..
            } = s.next().await
            else {
                panic!("all confirmed")
            };
            assert_eq!(
                (chat.as_deref(), follow, also.len()),
                (Some("claude-api"), vec!["coder".to_owned()], 2)
            );
            let mut out = logged_out("claude-subscription", &["claude-work", "work-api"]);
            if let AuthOutcome::LoggedOut { moved, .. } = &mut out {
                moved.push(wisp_protocol::subscription_auth::Followed {
                    agent: "coder".to_owned(),
                    model: "claude-opus-5-5".to_owned(),
                    effort: wisp_protocol::Effort::High,
                    rebuild_usd: Some(9.99),
                    from: None,
                    to: None,
                });
            }
            s.emit(request, out).await;
            let shown = s.shown();
            let line = shown.lines().find(|l| l.contains("coder switched to"));
            assert!(
                line.is_some_and(|l| l.ends_with(&amount)),
                "{amount}\n{shown}"
            );
            assert!(!shown.contains("9.99"), "{shown}");

            let two = vec![
                account("claude", "anthropic_subscription", true, false),
                account("chatgpt", "chatgpt_subscription", false, false),
            ];
            s.say("/logout all");
            s.described(two.clone()).await;
            let AuthCommand::Logout { request, .. } = s.next().await else {
                panic!("nothing left")
            };
            s.emit(
                request,
                AuthOutcome::LogoutNeeds {
                    presets: Vec::new(),
                    deletes_keys_of: Vec::new(),
                    new_start: None,
                    ways: Vec::new(),
                },
            )
            .await;
            let shown = s.shown();
            assert!(
                shown.contains("? Sign out of \u{25cb} claude and \u{25cb} chatgpt?"),
                "{shown}"
            );
            assert!(!shown.contains("Nothing would be left"), "{shown}");
            s.key(KeyCode::Char('1'));
            s.key(KeyCode::Enter);
            let AuthCommand::Logout {
                request,
                chat: None,
                confirmed: true,
                ..
            } = s.next().await
            else {
                panic!("last")
            };
            let in_use = "chatgpt is in use. Try again in a moment.";
            let mut partial = logged_out("claude", &[]);
            if let AuthOutcome::LoggedOut { stopped, .. } = &mut partial {
                *stopped = Some(in_use.to_owned());
            }
            s.emit(request, partial).await;
            let shown = s.shown();
            assert!(
                shown.contains("✓ Signed out · Claude subscription · ○ claude"),
                "{shown}"
            );
            assert!(shown.contains(in_use), "{shown}");
            assert!(!shown.contains("Run /login to sign in again."), "{shown}");
            assert!(!shown.contains(" ? Sign in"), "{shown}");

            s.say("/logout all");
            s.described(two.clone()).await;
            let AuthCommand::Logout { request, .. } = s.next().await else {
                panic!("nothing left")
            };
            s.emit(
                request,
                AuthOutcome::LogoutNeeds {
                    presets: Vec::new(),
                    deletes_keys_of: Vec::new(),
                    new_start: None,
                    ways: Vec::new(),
                },
            )
            .await;
            let shown = s.shown();
            assert!(
                shown.contains("? Sign out of \u{25cb} claude and \u{25cb} chatgpt?"),
                "{shown}"
            );
            assert!(!shown.contains("Nothing would be left"), "{shown}");
            s.key(KeyCode::Char('1'));
            s.key(KeyCode::Enter);
            let AuthCommand::Logout {
                request,
                chat: None,
                confirmed: true,
                ..
            } = s.next().await
            else {
                panic!("last")
            };
            s.events
                .send(AgentEvent::EndpointsChanged {
                    endpoints: Vec::new(),
                })
                .await
                .unwrap();
            s.emit(request, logged_out("claude", &["chatgpt"])).await;
            assert!(s.shown().contains(" ? Sign in"), "{}", s.shown());
            s.key(KeyCode::Esc);
            s.settle().await;
            let shown = s.shown();
            assert!(!shown.contains("Run /login to sign in again."), "{shown}");
            assert!(!shown.contains("unavailable"), "{shown}");
            s.say("hello");
            s.settle().await;
            assert!(s.shown().contains(" ? Sign in"), "{}", s.shown());
        },
    )
    .await;
}

#[tokio::test(start_paused = true)]
async fn an_oversized_private_url_shows_only_its_host_and_path_and_expiry_cancels_without_echoing_paste()
 {
    let screen = Screen::new(12, 40);
    let inspect = screen.clone();
    scenario(screen, |mut s| async move {
        s.say("/login claude work");
        let AuthCommand::Start { request: login, .. } = s.next().await else {
            panic!("start")
        };
        let url = format!(
            "https://example.invalid/{}?last=END_OF_URL",
            "x".repeat(800)
        );
        s.challenge(login, SubscriptionProvider::Claude, &url, 5)
            .await;
        let row = s.link_row();
        assert_eq!(
            s.screen.visible()[usize::from(row)].trim(),
            "\u{2192} example.invalid/\u{2026}"
        );
        assert!(!s.shown().contains("more below"), "{}", s.shown());
        s.paste(CODE);
        sleep(Duration::from_secs(5)).await;
        let AuthCommand::Cancel { login: expired, .. } = s.next().await else {
            panic!("expiry")
        };
        assert_eq!(expired, login);
        s.settle().await;
        assert!(!s.shown().contains("CODE_CANARY"));
        s.key(KeyCode::Enter);
    })
    .await;
    let seen = inspect.seen().join("\n");
    assert!(!seen.contains("END_OF_URL") && !seen.contains("xxxxxxxxxx"));
}

#[tokio::test(start_paused = true)]
async fn a_chatgpt_sign_in_that_cannot_finish_releases_its_callback() {
    scenario(Screen::new(24, 80), |mut s| async move {
        s.say("/login chatgpt work");
        let AuthCommand::Start { request: login, .. } = s.next().await else {
            panic!("start")
        };
        s.challenge(login, SubscriptionProvider::ChatGpt, URL, 600)
            .await;
        let AuthCommand::Finish { request, .. } = s.next().await else {
            panic!("finish")
        };
        s.emit(
            request,
            AuthOutcome::Failed {
                kind: AuthFailureKind::Unreachable,
                message: "could not reach OpenAI".into(),
                problem: None,
                same_account: None,
            },
        )
        .await;
        let AuthCommand::Cancel {
            login: released, ..
        } = s.next().await
        else {
            panic!("release")
        };
        assert_eq!(released, login);
        assert!(
            s.shown().contains("could not reach OpenAI"),
            "{}",
            s.shown()
        );
        s.key(KeyCode::Enter);
        s.settle().await;
    })
    .await;
}

#[tokio::test(start_paused = true)]
async fn the_name_typed_at_the_step_is_the_alias_signed_in() {
    scenario(Screen::new(24, 80), |mut s| async move {
        s.say("/login claude");
        s.settle().await;
        assert!(
            s.shown().contains("Name for this account: claude"),
            "{}",
            s.shown()
        );
        s.inputs.send(ctrl_key(KeyCode::Char('u'))).unwrap();
        for c in "Claude1".chars() {
            s.key(KeyCode::Char(c));
        }
        s.key(KeyCode::Enter);
        let AuthCommand::Start {
            request: login,
            alias,
            reauthorize,
            ..
        } = s.next().await
        else {
            panic!("start")
        };
        assert_eq!(alias.as_deref(), Some("claude1"));
        assert!(!reauthorize);
        s.emit(
            login,
            AuthOutcome::Challenge {
                challenge: PrivateChallenge {
                    login,
                    provider: SubscriptionProvider::Claude,
                    alias: "claude1".into(),
                    kind: ChallengeKind::ManualCode,
                    authorization_url: Secret::new(URL),
                    expires_in_secs: 600,
                    opened: false,
                },
            },
        )
        .await;
        s.paste(CODE);
        s.key(KeyCode::Enter);
        let AuthCommand::Finish {
            request,
            login: finishing,
            alias,
            ..
        } = s.next().await
        else {
            panic!("finish")
        };
        assert_eq!(finishing, login);
        assert_eq!(alias, None);
        s.emit(
            request,
            AuthOutcome::Ready {
                endpoint: SubscriptionEndpoint {
                    alias: "claude1".into(),
                    ..endpoint(SubscriptionProvider::Claude)
                },
                previous_cleanup_pending: false,
            },
        )
        .await;
        assert!(
            s.shown()
                .contains("Signed in \u{b7} Claude subscription \u{b7} \u{25cf} claude1"),
            "{}",
            s.shown()
        );
        s.key(KeyCode::Enter);
        s.settle().await;
    })
    .await;
}

async fn signs_in(s: &mut AuthRun, request: RequestId, provider: SubscriptionProvider) {
    s.emit(
        request,
        AuthOutcome::Ready {
            endpoint: endpoint(provider),
            previous_cleanup_pending: false,
        },
    )
    .await;
    let done = DONE.replace(
        "Claude",
        match provider {
            SubscriptionProvider::Claude => "Claude",
            SubscriptionProvider::ChatGpt => "ChatGPT",
            SubscriptionProvider::Antigravity => "Antigravity",
        },
    );
    assert!(s.shown().contains(&done), "{}", s.shown());
    s.key(KeyCode::Esc);
    s.settle().await;
}

#[tokio::test(start_paused = true)]
async fn a_link_click_during_chatgpts_wait_opens_it_again_beside_the_finish() {
    let screen = Screen::new(24, 80);
    let inspect = screen.clone();
    scenario_with(screen, terminal(), |mut s| async move {
        s.say("/login chatgpt work");
        let AuthCommand::Start { request: login, .. } = s.next().await else {
            panic!("start")
        };
        s.reply(login, login, SubscriptionProvider::ChatGpt, URL, 600, true)
            .await;
        let AuthCommand::Finish { request, .. } = s.next().await else {
            panic!("finish")
        };
        assert_eq!(
            s.words(),
            format!("{TRIED_BROWSER} \u{b7} link sent to your terminal")
        );
        assert!(!s.status().contains("Sent"), "{}", s.status());
        s.click_link();
        let AuthCommand::Open {
            request: open,
            login: opening,
        } = s.next().await
        else {
            panic!("open")
        };
        assert_eq!(opening, login);
        let other = "https://example.invalid/OTHER_URL_CANARY";
        s.reply(open, login, SubscriptionProvider::ChatGpt, other, 1, false)
            .await;
        assert_eq!(
            s.words(),
            "no browser here \u{b7} link sent to your terminal"
        );
        assert!(s.shown().contains(LINK_TEXT) && !s.shown().contains("OTHER_URL"));
        sleep(Duration::from_secs(2)).await;
        assert_eq!(
            s.words(),
            "no browser here \u{b7} link sent to your terminal"
        );
        signs_in(&mut s, request, SubscriptionProvider::ChatGpt).await;
    })
    .await;
    assert_eq!(inspect.sent_to_clipboard(), vec![osc52(URL)]);
    assert!(!inspect.seen().join("\n").contains("OTHER_URL"));
}

#[tokio::test(start_paused = true)]
async fn a_refused_open_or_a_turn_in_the_code_step_keeps_the_card_and_the_paste_signs_in() {
    let screen = Screen::new(24, 80);
    let inspect = screen.clone();
    scenario_with(screen, terminal(), |mut s| async move {
        s.say("/login claude work");
        let AuthCommand::Start { request: login, .. } = s.next().await else {
            panic!("start")
        };
        s.reply(login, login, SubscriptionProvider::Claude, URL, 600, true)
            .await;
        assert_eq!(
            s.words(),
            format!("{TRIED_BROWSER} \u{b7} link sent to your terminal")
        );
        s.click_link();
        let AuthCommand::Open { request: open, .. } = s.next().await else {
            panic!("open")
        };
        s.emit(
            open,
            AuthOutcome::Failed {
                kind: AuthFailureKind::StaleRequest,
                message: "OPEN_REFUSED_CANARY".into(),
                problem: None,
                same_account: None,
            },
        )
        .await;
        assert_eq!(
            s.words(),
            "no browser here \u{b7} link sent to your terminal"
        );
        s.events.send(AgentEvent::turn_started(1)).await.unwrap();
        s.events.send(completed()).await.unwrap();
        s.settle().await;
        assert_eq!(
            s.words(),
            "no browser here \u{b7} link sent to your terminal"
        );
        s.paste(CODE);
        s.key(KeyCode::Enter);
        let AuthCommand::Finish {
            request,
            login: finishing,
            ..
        } = s.next().await
        else {
            panic!("finish")
        };
        assert_eq!(finishing, login);
        signs_in(&mut s, request, SubscriptionProvider::Claude).await;
    })
    .await;
    assert!(!inspect.seen().join("\n").contains("OPEN_REFUSED_CANARY"));
}

#[tokio::test(start_paused = true)]
async fn a_drag_across_the_link_opens_nothing() {
    scenario(Screen::new(24, 80), |mut s| async move {
        s.say("/login claude work");
        let AuthCommand::Start { request: login, .. } = s.next().await else {
            panic!("start")
        };
        s.reply(login, login, SubscriptionProvider::Claude, URL, 600, true)
            .await;
        let row = s.link_row();
        s.mouse(MouseKind::Drag, row);
        s.mouse(MouseKind::Release, row);
        s.settle().await;
        s.key(KeyCode::Esc);
        assert!(matches!(s.next().await, AuthCommand::Cancel { .. }));
    })
    .await;
}

#[tokio::test(start_paused = true)]
async fn a_double_click_opens_the_link_once() {
    scenario(Screen::new(24, 80), |mut s| async move {
        s.say("/login claude work");
        let AuthCommand::Start { request: login, .. } = s.next().await else {
            panic!("start")
        };
        s.reply(login, login, SubscriptionProvider::Claude, URL, 600, true)
            .await;
        s.click_link();
        s.click_link();
        s.settle().await;
        assert!(matches!(s.next().await, AuthCommand::Open { .. }));
        s.key(KeyCode::Esc);
        assert!(matches!(s.next().await, AuthCommand::Cancel { .. }));
    })
    .await;
}

#[tokio::test(start_paused = true)]
async fn with_no_browser_a_link_click_copies_it_again_and_sends_no_open() {
    let screen = Screen::new(24, 80);
    let inspect = screen.clone();
    scenario_with(screen, terminal(), |mut s| async move {
        s.say("/login claude work");
        let AuthCommand::Start { request: login, .. } = s.next().await else {
            panic!("start")
        };
        s.challenge(login, SubscriptionProvider::Claude, URL, 600)
            .await;
        s.click_link();
        s.settle().await;
        s.key(KeyCode::Esc);
        assert!(matches!(s.next().await, AuthCommand::Cancel { .. }));
    })
    .await;
    assert_eq!(inspect.sent_to_clipboard(), vec![osc52(URL), osc52(URL)]);
}

fn tmux_only(dir: &std::path::Path) -> Clipboard {
    std::fs::create_dir_all(dir).unwrap();
    fake_tool::install(&dir.join("tmux"), "#!/bin/sh\n/bin/cat > /dev/null\n");
    Clipboard {
        route: Route::Local {
            tmux: true,
            natives: vec![],
        },
        tools: Tools {
            path: Some(dir.as_os_str().to_owned()),
            powershell: vec![],
            setsid: None,
            timeout: Duration::from_secs(5),
        },
    }
}

#[tokio::test(start_paused = true)]
async fn the_link_copy_reports_on_the_card_on_every_route_and_never_in_a_toast() {
    let fake = super::select::Fake::new("login-link");
    let tmux = scratch::ScratchDir::new(SCRATCH, "login-tmux");
    let routes = [
        (terminal().clipboard, "link sent to your terminal"),
        (fake.clipboard(), "link copied"),
        (tmux_only(&tmux.root), "link sent to your terminal"),
        (
            Clipboard {
                route: Route::Blocked(SCREEN_BLOCKS),
                ..Clipboard::none()
            },
            "couldn't copy the link",
        ),
        (Clipboard::none(), "couldn't copy the link"),
    ];
    for (clipboard, second) in routes {
        let mut opts = options(Start::Stalled);
        opts.clipboard = clipboard;
        scenario_with(Screen::new(24, 80), opts, |mut s| async move {
            s.say("/login claude work");
            let AuthCommand::Start { request: login, .. } = s.next().await else {
                panic!("start")
            };
            s.reply(login, login, SubscriptionProvider::Claude, URL, 600, true)
                .await;
            let want = format!("{TRIED_BROWSER} \u{b7} {second}");
            eventually(|| s.words() == want).await;
            assert_eq!(s.words(), want);
            s.click_link();
            let AuthCommand::Open { request: open, .. } = s.next().await else {
                panic!("open")
            };
            s.reply(open, login, SubscriptionProvider::Claude, URL, 600, true)
                .await;
            real_wait(200).await;
            s.settle().await;
            let status = s.status();
            assert!(
                ["Copied", "Couldn't copy", "Sent", "example.invalid"]
                    .iter()
                    .all(|toast| !status.contains(toast)),
                "{status}"
            );
            s.key(KeyCode::Esc);
            s.settle().await;
        })
        .await;
    }
    assert_eq!(fake.copies(), vec![URL.to_owned()]);
}

#[tokio::test(start_paused = true)]
async fn a_modified_press_on_the_link_row_sends_nothing_copies_nothing_and_changes_no_word() {
    for opened in [true, false] {
        let screen = Screen::new(24, 80);
        let inspect = screen.clone();
        scenario_with(screen, terminal(), |mut s| async move {
            s.say("/login claude work");
            let AuthCommand::Start { request: login, .. } = s.next().await else {
                panic!("start")
            };
            s.reply(login, login, SubscriptionProvider::Claude, URL, 600, opened)
                .await;
            let words = s.words();
            let row = s.link_row();
            s.press(MouseKind::Press, row, true);
            s.settle().await;
            assert_eq!(s.words(), words);
            s.key(KeyCode::Esc);
            assert!(matches!(s.next().await, AuthCommand::Cancel { .. }));
        })
        .await;
        assert_eq!(inspect.sent_to_clipboard(), vec![osc52(URL)]);
    }
}

#[tokio::test(start_paused = true)]
async fn the_api_key_row_opens_the_key_form_with_its_name_filled_in() {
    let mut opts = options(Start::Stalled);
    opts.card_setup = Some(super::endpoints::card_setup());
    scenario_with(Screen::new(24, 80), opts, |s| async move {
        s.say("/login");
        s.settle().await;
        assert!(s.shown().contains("4 API key"), "{}", s.shown());
        s.key(KeyCode::Char('4'));
        s.settle().await;
        let shown = s.shown();
        assert!(shown.contains("Add an API key"), "{shown}");
        assert!(shown.contains("anthropic"), "{shown}");
        assert!(!shown.contains("Sign in"), "{shown}");
        s.key(KeyCode::Esc);
        s.settle().await;
    })
    .await;
}

async fn signed_in_card(s: &mut AuthRun) {
    s.say("/login claude work");
    let AuthCommand::Start { request: login, .. } = s.next().await else {
        panic!("start")
    };
    s.challenge(login, SubscriptionProvider::Claude, URL, 600)
        .await;
    s.paste(CODE);
    s.key(KeyCode::Enter);
    let AuthCommand::Finish { request, .. } = s.next().await else {
        panic!("finish")
    };
    s.emit(
        request,
        AuthOutcome::Ready {
            endpoint: endpoint(SubscriptionProvider::Claude),
            previous_cleanup_pending: false,
        },
    )
    .await;
    assert!(s.shown().contains(DONE), "{}", s.shown());
    assert!(
        s.shown().contains("1 Use \u{25cf} work for this chat"),
        "{}",
        s.shown()
    );
}

fn settings_sent(s: &mut AuthRun) -> Vec<ClientCommand> {
    std::iter::from_fn(|| s.commands.try_recv().ok())
        .filter(|command| matches!(command, ClientCommand::ChangeSettings { .. }))
        .collect()
}

#[tokio::test(start_paused = true)]
async fn using_the_new_account_switches_the_chat_and_not_now_keeps_it() {
    scenario(Screen::new(24, 80), |mut s| async move {
        signed_in_card(&mut s).await;
        s.key(KeyCode::Enter);
        s.settle().await;
        let sent = settings_sent(&mut s);
        let [ClientCommand::ChangeSettings { changes, .. }] = sent.as_slice() else {
            panic!("one switch: {sent:?}")
        };
        assert_eq!(
            changes,
            &ClientCommand::shortcut_changes(Some("work".to_owned()), None, None)
        );
        assert!(s.shown().contains(DONE), "{}", s.shown());
        assert!(!s.shown().contains("Use \u{25cf} work"), "{}", s.shown());
    })
    .await;
    scenario(Screen::new(24, 80), |mut s| async move {
        signed_in_card(&mut s).await;
        s.key(KeyCode::Down);
        s.key(KeyCode::Enter);
        s.settle().await;
        assert!(settings_sent(&mut s).is_empty());
        assert!(s.shown().contains(DONE), "{}", s.shown());
        assert!(!s.shown().contains("Use \u{25cf} work"), "{}", s.shown());
    })
    .await;
}

#[tokio::test(start_paused = true)]
async fn a_sign_in_another_name_holds_is_refused_on_the_card_and_kept_in_the_transcript() {
    scenario(Screen::new(30, 80), |mut s| async move {
        let line = "Not signed in: this is the same ChatGPT account as \u{25cf} chatgpt.";
        let endpoints = vec![
            account("claude-api", "key", false, false),
            account("chatgpt", "chatgpt_subscription", false, false),
        ];
        s.events
            .send(AgentEvent::EndpointsChanged { endpoints })
            .await
            .unwrap();
        s.settle().await;
        for (round, close) in ["enter", "esc", "turn"].into_iter().enumerate() {
            s.say("/login chatgpt work");
            let AuthCommand::Start { request: login, .. } = s.next().await else {
                panic!("start")
            };
            s.challenge(login, SubscriptionProvider::ChatGpt, URL, 600)
                .await;
            let AuthCommand::Finish { request, .. } = s.next().await else {
                panic!("finish")
            };
            s.emit(
                request,
                AuthOutcome::Failed {
                    kind: AuthFailureKind::Declined,
                    message: "this is the same ChatGPT account as chatgpt".into(),
                    problem: None,
                    same_account: Some("chatgpt".into()),
                },
            )
            .await;
            let AuthCommand::Cancel {
                login: released, ..
            } = s.next().await
            else {
                panic!("release")
            };
            assert_eq!(released, login);
            assert!(s.shown().contains(line), "{}", s.shown());
            assert!(
                s.shown()
                    .contains("To sign it in again: /login --name chatgpt")
            );
            match close {
                "enter" => {
                    s.key(KeyCode::Enter);
                }
                "esc" => {
                    s.key(KeyCode::Esc);
                }
                _ => {
                    s.events.send(AgentEvent::turn_started(1)).await.unwrap();
                    s.events.send(completed()).await.unwrap();
                    s.settle().await;
                    assert!(s.shown().contains("enter close"), "{}", s.shown());
                    s.key(KeyCode::Enter);
                }
            }
            s.settle().await;
            assert!(!s.shown().contains("enter close"), "{}", s.shown());
            assert_eq!(s.shown().matches(line).count(), round + 1, "{}", s.shown());
            assert!(
                s.shown()
                    .contains("To sign it in again: /login --name chatgpt")
            );
        }
    })
    .await;
}
