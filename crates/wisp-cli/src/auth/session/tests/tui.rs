use std::cell::RefCell;
use std::rc::Rc;
use std::time::Duration;

use ratatui_core::layout::Size;
use tokio::sync::mpsc;
use wisp_core::agent::{Agent, default_session};
use wisp_core::context::ModelSettings;
use wisp_core::kit::Connect;
use wisp_core::models::{ConfigModels, ModelTable};
use wisp_core::session::SessionStore;
use wisp_protocol::{AgentEvent, CacheTtl};
use wisp_render::chrome::{Credential, Endpoint, SECRET_PLACEHOLDER, WriteMultipliers};
use wisp_render::theme::glyphs::GlyphSet;
use wisp_render::theme::{Depth, Mode as ThemeMode, Theme};
use wisp_render::transcript::copy;
use wisp_tui::app::{Options, Session, SessionFacts};
use wisp_tui::term::clipboard::Osc52;
use wisp_tui::term::{Console, Halted, Input, Key, KeyCode, Modes, Written};

use super::*;

#[derive(Clone)]
struct Screen(Rc<RefCell<vt100::Parser>>, Rc<tokio::sync::Notify>);
impl Screen {
    fn text(&self) -> String {
        self.0.borrow().screen().contents()
    }
}
impl Console for Screen {
    fn size(&self) -> std::io::Result<Size> {
        Ok(Size::new(120, 30))
    }
    fn suspend(&mut self) -> std::io::Result<()> {
        Ok(())
    }
    fn reapply(&mut self) -> std::io::Result<()> {
        Ok(())
    }
    fn halted(&self) -> Option<Halted> {
        None
    }
    fn frame(&mut self, body: &[u8], _: Size) -> std::io::Result<Written> {
        self.0.borrow_mut().process(body);
        self.1.notify_one();
        Ok(Written::Sent)
    }
    fn clipboard(&mut self, _: &Osc52) -> std::io::Result<Written> {
        panic!("auth must not write the clipboard")
    }
    fn hand_off(&mut self, _: &[u8]) -> std::io::Result<Written> {
        panic!("auth must not export a transcript")
    }
    fn take_back(&mut self) -> std::io::Result<()> {
        Ok(())
    }
    fn bell(&mut self) -> std::io::Result<Written> {
        Ok(Written::Sent)
    }
    fn modes(&self) -> Modes {
        Modes::COOKED
    }
    fn set_modes(&mut self, _: Modes) -> std::io::Result<()> {
        Ok(())
    }
}

struct Ui {
    screen: Screen,
    inputs: mpsc::UnboundedSender<Input>,
    events: mpsc::UnboundedReceiver<AgentEvent>,
}
impl Ui {
    fn key(&self, code: KeyCode) {
        self.inputs.send(Input::Key(Key::plain(code))).unwrap();
    }
    fn say(&self, text: &str) {
        self.inputs.send(Input::Paste(text.into())).unwrap();
        self.key(KeyCode::Enter);
    }
    async fn wait_screen(&self, ready: impl Fn(&str) -> bool) {
        tokio::time::timeout(Duration::from_secs(30), async {
            loop {
                let changed = self.screen.1.notified();
                if ready(&self.screen.text()) {
                    return;
                }
                changed.await;
            }
        })
        .await
        .unwrap_or_else(|_| {
            panic!(
                "screen did not reach expected state: {}",
                self.screen.text()
            )
        });
    }

    async fn shown(&self, text: &str) {
        self.wait_screen(|screen| screen.contains(text)).await;
    }
    async fn auth(&mut self) -> AuthEvent {
        loop {
            match tokio::time::timeout(Duration::from_secs(30), self.events.recv())
                .await
                .unwrap_or_else(|_| panic!("timed out: {}", self.screen.text()))
                .unwrap()
            {
                AgentEvent::SubscriptionAuth { event } => {
                    match &event.outcome {
                        AuthOutcome::Challenge { challenge }
                            if challenge.kind == ChallengeKind::ManualCode =>
                        {
                            self.shown(&format!("Code  {SECRET_PLACEHOLDER}")).await;
                        }
                        AuthOutcome::Ready { endpoint, .. } => {
                            self.shown(&format!(" \u{b7} \u{25cf} {}", endpoint.alias))
                                .await
                        }
                        AuthOutcome::SavedNotReady { .. } => {
                            self.shown("saved, not usable yet").await
                        }
                        AuthOutcome::LoggedOut { alias, .. } => {
                            self.shown("Signed out \u{b7} ").await;
                            self.shown(alias).await;
                        }
                        AuthOutcome::Cancelled { .. } => self.shown("Sign-in closed.").await,
                        AuthOutcome::Failed { message, .. } => {
                            let words =
                                |text: &str| text.split_whitespace().collect::<Vec<_>>().join(" ");
                            let message = words(message);
                            self.wait_screen(|screen| words(screen).contains(&message))
                                .await
                        }
                        _ => {}
                    }
                    return event;
                }
                AgentEvent::TurnStarted { .. } | AgentEvent::TurnEnded { .. } => {
                    panic!("auth created a model turn")
                }
                _ => {}
            }
        }
    }
    async fn login(&mut self, provider: SubscriptionProvider) -> AuthOutcome {
        let event = self.auth().await;
        assert!(
            matches!(event.outcome, AuthOutcome::Challenge { .. }),
            "{event:?}"
        );
        if provider == SubscriptionProvider::Claude {
            assert!(
                self.screen
                    .text()
                    .contains("\u{2192} example.invalid/authorize\u{2026}"),
                "{}",
                self.screen.text()
            );
            self.inputs
                .send(Input::Paste("CODE_CANARY#STATE_CANARY".into()))
                .unwrap();
            self.key(KeyCode::Enter);
        }
        self.auth().await.outcome
    }
    async fn selected(&mut self) {
        loop {
            match tokio::time::timeout(Duration::from_secs(30), self.events.recv())
                .await
                .unwrap_or_else(|_| panic!("timed out: {}", self.screen.text()))
                .unwrap()
            {
                AgentEvent::SettingsChanged { endpoint, .. } => {
                    self.shown(&format!("Switched to \u{25cf} {endpoint} "))
                        .await;
                    return;
                }
                AgentEvent::Rejected { reason, .. } => panic!("switch rejected: {reason}"),
                _ => {}
            }
        }
    }
}

fn facts(model: &str) -> SessionFacts {
    SessionFacts {
        version: "test".into(),
        provider: "openai".into(),
        model: model.into(),
        effort: "medium".into(),
        cache_ttl: CacheTtl::OneHour,
        credential: Some(Credential::ApiKey),
        host: None,
        log: None,
        home: None,
        cwd: "project".into(),
        endpoint: Endpoint::Official,
        models: vec![],
        on_accounts: Vec::new(),
        write_multipliers: WriteMultipliers {
            prefix: 1.0,
            tail: 1.0,
            five_minutes: 1.0,
            one_hour: 1.0,
        },
        next: None,
        max_tokens: 4096,
        endpoint_name: Some("base".into()),
        endpoints: vec![],
        endpoint_note: None,
        resume_refused: false,
        instructions: Default::default(),
        search: Default::default(),
        providers_off: Default::default(),
        resumed: None,
        claude_usage: false,
        start_pick: None,
    }
}

async fn run_ui<F: std::future::Future<Output = ()>>(
    fixture: &Fixture,
    drive: impl FnOnce(Ui) -> F,
) {
    let connected = fixture.host.catalog.as_ref().unwrap().connect("/").unwrap();
    let context = connected.ready[0].context.clone();
    let store = wisp_core::session::MemoryStore::default();
    let mut config = default_session();
    config.model = fixture.host.model.clone();
    let agent = Agent::new(connected.provider, config)
        .with_connections(connected.ready, connected.unavailable, &connected.active)
        .with_model_settings(ModelSettings::new(
            ModelTable::shipped().unwrap(),
            ConfigModels::default(),
        ))
        .with_log(
            wisp_core::session::SessionLog::new(Box::new(store.clone()), "auth-tui"),
            context,
        )
        .with_endpoint_host(fixture.host.clone());
    let (kernel_commands, mut kernel_events, task) = wisp_core::agent::spawn(agent);
    let (commands, mut received) = mpsc::channel(64);
    let (events_in, events) = mpsc::channel(256);
    let (seen, observed) = mpsc::unbounded_channel();
    let (inputs, input_rx) = mpsc::unbounded_channel();
    let screen = Screen(
        Rc::new(RefCell::new(vt100::Parser::new(
            std::num::NonZeroU16::new(30).unwrap(),
            std::num::NonZeroU16::new(120).unwrap(),
            0,
        ))),
        Rc::new(tokio::sync::Notify::new()),
    );
    let mut console = screen.clone();
    let mut options = Options::new(Theme::new(ThemeMode::Dark, Depth::None), GlyphSet::Unicode);
    options.code = wisp_tui::app::highlight::Start::Stalled;
    let session = Session {
        leave_note: None,
        attached: None,
        alerts: Vec::new(),
        facts: SessionFacts {
            endpoint_name: Some(connected.active.clone()),
            ..facts(&fixture.host.model)
        },
        events,
        commands,
        resumed: None,
    };
    let app = wisp_tui::app::run(&mut console, input_rx, session, options);
    let forward_commands = async move {
        while let Some(command) = received.recv().await {
            assert!(!format!("{command:?}").contains("CANARY"));
            kernel_commands.send(command).await.unwrap();
        }
    };
    let forward_events = async move {
        while let Some(event) = kernel_events.recv().await {
            assert!(!format!("{event:?}").contains("CANARY"));
            let _ = seen.send(event.clone());
            let _ = events_in.send(event).await;
        }
    };
    let driver = async move {
        let ui = Ui {
            screen,
            inputs,
            events: observed,
        };
        ui.shown(copy::PLACEHOLDER_SHORT).await;
        drive(ui).await;
    };
    let (exit, (), (), ()) = tokio::join!(app, forward_commands, forward_events, driver);
    assert!(exit.is_ok());
    assert!(task.join().await);
    let stored = serde_json::to_string(&store.read("auth-tui").unwrap()).unwrap();
    assert!(!stored.contains("CANARY"));
}

#[tokio::test]
async fn tui_and_real_host_activate_reauthorize_select_and_remove_both_providers_without_usage_opt_in()
 {
    for provider in [SubscriptionProvider::ChatGpt, SubscriptionProvider::Claude] {
        let fixture = Fixture::new_for(Mode::Stored, provider);
        let fixture = &fixture;
        run_ui(fixture, |mut ui| async move {
            let keyword = if provider == SubscriptionProvider::Claude {
                "claude"
            } else {
                "chatgpt"
            };
            ui.say(&format!("/login {keyword} work"));
            assert!(matches!(
                ui.login(provider).await,
                AuthOutcome::Ready { .. }
            ));
            ui.shown("Use \u{25cf} work for this chat").await;
            let catalog = fixture.host.catalog.as_ref().unwrap();
            let old = catalog
                .connect("/child")
                .unwrap()
                .ready
                .into_iter()
                .find(|connection| connection.name == "work")
                .unwrap()
                .provider;
            ui.key(KeyCode::Esc);
            ui.wait_screen(|screen| !screen.contains("Use \u{25cf} work for this chat"))
                .await;
            ui.say("/login work");
            assert!(matches!(
                ui.login(provider).await,
                AuthOutcome::Ready { .. }
            ));
            assert!(old.generation().unwrap().admit().is_err());
            ui.shown("Signed in again \u{b7} ").await;
            ui.say("/endpoint work");
            ui.selected().await;
            let current = catalog
                .connect("/child")
                .unwrap()
                .ready
                .into_iter()
                .find(|connection| connection.name == "work")
                .unwrap()
                .provider;
            let admission = current.generation().unwrap().admit().unwrap();
            ui.say("/logout work");
            assert!(matches!(
                ui.auth().await.outcome,
                AuthOutcome::LogoutNeeds { ref ways, .. } if ways.len() == 1
            ));
            ui.shown("This chat uses it. Where should it go?").await;
            ui.key(KeyCode::Enter);
            ui.key(KeyCode::Char('1'));
            ui.shown("enter sign out").await;
            ui.key(KeyCode::Enter);
            assert!(matches!(
                ui.auth().await.outcome,
                AuthOutcome::Failed {
                    kind: AuthFailureKind::Busy,
                    ..
                }
            ));
            ui.shown("work is in use. Try again in a moment.").await;
            ui.shown("Continuing on \u{25cf} base").await;
            let config = std::fs::read_to_string(fixture.host.config.as_ref().unwrap()).unwrap();
            assert!(config.contains("[endpoints.work]"), "{config}");
            drop(admission);
            let binding = current.generation().unwrap().bind("/x").unwrap();
            ui.say("/logout work");
            assert!(matches!(
                ui.auth().await.outcome,
                AuthOutcome::Failed {
                    kind: AuthFailureKind::ChildReference,
                    ..
                }
            ));
            assert!(
                ui.screen
                    .text()
                    .contains("x is working on work. Try again once it's done.")
            );
            drop(binding);
            ui.say("/logout work");
            assert!(matches!(
                ui.auth().await.outcome,
                AuthOutcome::LoggedOut {
                    cleanup_pending: false,
                    ..
                }
            ));
            assert!(!ui.screen.text().contains("Rejected"));
            assert!(
                !catalog
                    .view()
                    .ready
                    .iter()
                    .any(|connection| connection.name == "work")
            );
            assert!(current.generation().unwrap().admit().is_err());
            let config = std::fs::read_to_string(fixture.host.config.as_ref().unwrap()).unwrap();
            assert!(!config.contains("claude_usage = true"));
            let saved = wisp_core::endpoints::host::EndpointHost::save(
                fixture.host.as_ref(),
                    wisp_protocol::endpoint::EndpointSpec {
                        name: "keyed".to_owned(),
                        provider: "anthropic".to_owned(),
                        auth: "bearer".to_owned(),
                        url: None,
                        store: wisp_protocol::endpoint::StoreChoice::File,
                        default: false,
                    },
                    wisp_protocol::Secret::new("sk-ant-CANARYKEYEDa1b2"),
                    false,
                    tokio::sync::watch::channel(false).1,
                )
                .await;
            assert!(saved.is_ok());
            ui.say("/logout keyed");
            assert!(matches!(
                ui.auth().await.outcome,
                AuthOutcome::LogoutNeeds { ref deletes_keys_of, .. } if deletes_keys_of == &["keyed"]
            ));
            ui.shown("Its key is deleted from Wisp.").await;
            ui.key(KeyCode::Esc);
            ui.wait_screen(|screen| !screen.contains("Its key is deleted from Wisp."))
                .await;
            let config = std::fs::read_to_string(fixture.host.config.as_ref().unwrap()).unwrap();
            assert!(config.contains("[endpoints.keyed]"), "{config}");
            std::fs::write(
                fixture.host.env_file.as_ref().unwrap(),
                "FILED_KEY=sk-ant-CANARYFILEDa1b2\n",
            )
            .unwrap();
            std::fs::write(
                fixture.host.config.as_ref().unwrap(),
                format!("{config}\n[endpoints.filed]\nprovider = \"anthropic\"\nauth = \"bearer\"\nkey_env = \"FILED_KEY\"\n"),
            )
            .unwrap();
            ui.say("/logout filed");
            assert!(matches!(
                ui.auth().await.outcome,
                AuthOutcome::Failed {
                    kind: AuthFailureKind::InvalidAlias,
                    ..
                }
            ));
            let config = std::fs::read_to_string(fixture.host.config.as_ref().unwrap()).unwrap();
            assert!(config.contains("[endpoints.filed]"), "{config}");
            std::fs::write(fixture.host.env_file.as_ref().unwrap(), b"FILED_KEY=\xff\n").unwrap();
            ui.say("/logout filed");
            assert!(matches!(
                ui.auth().await.outcome,
                AuthOutcome::Failed { .. }
            ));
            let config = std::fs::read_to_string(fixture.host.config.as_ref().unwrap()).unwrap();
            assert!(config.contains("[endpoints.filed]"), "{config}");
        })
        .await;
    }
    use wisp_core::endpoints::watch::AccountsWatch;
    let fixture = Fixture::new_for(Mode::Stored, SubscriptionProvider::ChatGpt);
    for alias in ["work", "team", "spare"] {
        save(&fixture.host, alias).await;
    }
    let terminal = Arc::new(CliEndpointHost {
        catalog: None,
        ..(*fixture.host).clone()
    });
    let watch = Arc::new(terminal.paths());
    let first = watch.look(Default::default()).await.unwrap();
    let catalog = LiveCatalog::new(wisp_core::kit::Connected {
        provider: first.view.ready[0].provider.clone(),
        ready: first.view.ready,
        unavailable: first.view.unavailable,
        active: "work".to_owned(),
        env_account: None,
    })
    .with_watch(watch, first.prints);
    let window = Fixture {
        host: Arc::new(CliEndpointHost {
            catalog: Some(catalog),
            ..(*fixture.host).clone()
        }),
        ..Fixture::new_for(Mode::Stored, SubscriptionProvider::ChatGpt)
    };
    run_ui(&window, |ui| async move {
        for (request, alias) in [(10, "team"), (11, "spare")] {
            let command = AuthCommand::logout(
                RequestId(request),
                &[alias.to_owned()],
                true,
                None,
                Vec::new(),
            );
            let outcome = if alias == "team" {
                fixture.send(command).await
            } else {
                session_auth(&terminal, command).await.outcome
            };
            assert!(
                matches!(outcome, AuthOutcome::LoggedOut { .. }),
                "{outcome:?}"
            );
            ui.say("/effort low");
            ui.shown(&format!("{alias} was signed out in another window."))
                .await;
        }
    })
    .await;
}

#[tokio::test]
async fn tui_real_host_moves_nothing_before_the_chats_stored_key_is_answered() {
    let fixture = Fixture::new_for(Mode::Stored, SubscriptionProvider::Claude);
    let fixture = &fixture;
    run_ui(fixture, |mut ui| async move {
        let saved = wisp_core::endpoints::host::EndpointHost::save(
            fixture.host.as_ref(),
            wisp_protocol::endpoint::EndpointSpec {
                name: "keyed".to_owned(),
                provider: "anthropic".to_owned(),
                auth: "bearer".to_owned(),
                url: None,
                store: wisp_protocol::endpoint::StoreChoice::File,
                default: false,
            },
            wisp_protocol::Secret::new("sk-ant-CANARYKEYEDa1b2"),
            false,
            tokio::sync::watch::channel(false).1,
        )
        .await;
        assert!(saved.is_ok());
        while !matches!(
            ui.events.recv().await.unwrap(),
            AgentEvent::EndpointsChanged { .. }
        ) {}
        ui.say("/logout");
        ui.shown("Sign out of which account?").await;
        ui.key(KeyCode::Esc);
        ui.wait_screen(|screen| !screen.contains("Sign out of which account?"))
            .await;
        ui.say("/endpoint keyed");
        ui.selected().await;
        ui.say("/logout keyed");
        let needs = loop {
            match ui.events.recv().await.unwrap() {
                AgentEvent::SettingsChanged { endpoint, .. } => panic!("moved to {endpoint}"),
                AgentEvent::SubscriptionAuth { event } => break event.outcome,
                _ => {}
            }
        };
        assert!(matches!(
            needs,
            AuthOutcome::LogoutNeeds { ref deletes_keys_of, ref ways, .. }
                if deletes_keys_of == &["keyed"] && ways.is_empty()
        ));
        ui.shown("Its key is deleted from Wisp.").await;
        ui.key(KeyCode::Char('2'));
        ui.wait_screen(|screen| !screen.contains("Its key is deleted from Wisp."))
            .await;
        let footer = ui
            .screen
            .text()
            .lines()
            .rev()
            .find(|line| !line.starts_with('\u{2570}'))
            .unwrap_or_default()
            .to_owned();
        assert!(footer.contains("keyed"), "{footer}");
        let config = std::fs::read_to_string(fixture.host.config.as_ref().unwrap()).unwrap();
        assert!(config.contains("[endpoints.keyed]"), "{config}");
    })
    .await;
}

#[tokio::test]
async fn tui_real_host_saved_not_ready_reloads_without_another_login() {
    let fixture = Fixture::new(Mode::Absent);
    let fixture = &fixture;
    run_ui(fixture, |mut ui| async move {
        ui.say("/login chatgpt work");
        assert!(matches!(
            ui.login(SubscriptionProvider::ChatGpt).await,
            AuthOutcome::SavedNotReady { .. }
        ));
        assert!(ui.screen.text().contains("saved, not usable yet"));
        write_profile(
            &fixture.dir.root.join("config.toml"),
            "oauth:00000000000000000000000000000001",
        );
        ui.key(KeyCode::Enter);
        assert!(matches!(ui.auth().await.outcome, AuthOutcome::Ready { .. }));
        assert_eq!(fixture.fake.starts.load(Ordering::SeqCst), 1);
    })
    .await;
}

#[tokio::test]
async fn tui_real_host_cancellation_drops_pending_both_provider_logins_without_persisting() {
    for provider in [SubscriptionProvider::ChatGpt, SubscriptionProvider::Claude] {
        let fixture = Fixture::new_for(Mode::Waiting, provider);
        let fixture = &fixture;
        run_ui(fixture, |mut ui| async move {
            ui.say(if provider == SubscriptionProvider::Claude {
                "/login claude work"
            } else {
                "/login chatgpt work"
            });
            assert!(matches!(
                ui.auth().await.outcome,
                AuthOutcome::Challenge { .. }
            ));
            if provider == SubscriptionProvider::Claude {
                ui.inputs
                    .send(Input::Paste("CODE_CANARY#STATE_CANARY".into()))
                    .unwrap();
            }
            ui.key(KeyCode::Esc);
            assert!(matches!(
                ui.auth().await.outcome,
                AuthOutcome::Cancelled { .. }
            ));
            assert!(fixture.host.auth.pending().is_empty());
            assert!(!fixture.host.config.as_ref().unwrap().exists());
            assert!(!ui.screen.text().contains("CANARY"));
        })
        .await;
    }
}

#[test]
fn subscription_registry_always_contains_all_login_implementations() {
    let providers: Vec<_> = super::super::super::subscription_logins()
        .iter()
        .map(|login| login.descriptor().kind.provider())
        .collect();
    assert_eq!(
        providers,
        [
            ProviderKind::OpenAi,
            ProviderKind::Anthropic,
            ProviderKind::Gemini,
        ]
    );
}
