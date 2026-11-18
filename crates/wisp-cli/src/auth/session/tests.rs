use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};

use futures::future::BoxFuture;
use wisp_core::oauth::{AuthError, AuthStatus};
use wisp_core::provider::Provider;
use wisp_core::provider::credential::SubscriptionKind;
use wisp_core::secrets::StoreKind;
use wisp_core::secrets::keyring::Budget;
use wisp_core::subscription_login::{Approved, Descriptor, OauthLogin, PendingSubscriptionLogin};
use wisp_protocol::Secret;

use super::*;

#[derive(Clone, Copy)]
enum Mode {
    Stored,
    Absent,
    Waiting,
    Unreachable,
    UnreachableTwice,
    Refused,
}

type During = Arc<std::sync::Mutex<Option<Box<dyn FnOnce() + Send>>>>;

#[derive(Clone, Default)]
struct Held {
    profiles: Arc<std::sync::Mutex<Vec<PathBuf>>>,
    own: Arc<std::sync::Mutex<Vec<Option<PathBuf>>>>,
    during: During,
}

struct Fake {
    provider: SubscriptionProvider,
    mode: Mode,
    starts: Arc<AtomicUsize>,
    drops: Arc<AtomicUsize>,
    entered: Arc<tokio::sync::Notify>,
    held: Held,
}

impl Fake {
    fn real(&self) -> OauthLogin {
        OauthLogin(match self.provider {
            SubscriptionProvider::ChatGpt => SubscriptionKind::ChatGpt,
            SubscriptionProvider::Claude => SubscriptionKind::Anthropic,
            SubscriptionProvider::Antigravity => SubscriptionKind::Antigravity,
        })
    }
}

impl SubscriptionLogin for Fake {
    fn descriptor(&self) -> Descriptor {
        self.real().descriptor()
    }
    fn start(
        &self,
        allow_loopback: bool,
    ) -> BoxFuture<'_, Result<Box<dyn PendingSubscriptionLogin>, AuthError>> {
        let id = self.starts.fetch_add(1, Ordering::SeqCst) + 1;
        let uses_loopback = match self.provider {
            SubscriptionProvider::ChatGpt => true,
            SubscriptionProvider::Claude => false,
            SubscriptionProvider::Antigravity => allow_loopback,
        };
        let pending = FakePending {
            provider: self.provider,
            mode: self.mode,
            uses_loopback,
            handle: format!("oauth:{id:032x}"),
            drops: self.drops.clone(),
            entered: self.entered.clone(),
            held: self.held.clone(),
        };
        Box::pin(async move { Ok(Box::new(pending) as Box<dyn PendingSubscriptionLogin>) })
    }
    fn status(&self, path: &Path) -> Result<AuthStatus, AuthError> {
        self.real().status(path)
    }
    fn forget<'a>(&'a self, path: &'a Path) -> BoxFuture<'a, Result<(), AuthError>> {
        Box::pin(oauth::forget(path))
    }
    fn logout<'a>(&'a self, path: &'a Path) -> BoxFuture<'a, Result<bool, AuthError>> {
        Box::pin(async move {
            oauth::forget(path).await?;
            Ok(false)
        })
    }
}

struct FakePending {
    provider: SubscriptionProvider,
    mode: Mode,
    uses_loopback: bool,
    handle: String,
    drops: Arc<AtomicUsize>,
    entered: Arc<tokio::sync::Notify>,
    held: Held,
}

struct FakeApproved {
    provider: SubscriptionProvider,
    mode: Mode,
    handle: String,
    held: Held,
}

impl Approved for FakeApproved {
    fn held_by(&self, _: &Path, own: Option<&Path>) -> Vec<PathBuf> {
        self.held.own.lock().unwrap().push(own.map(Path::to_owned));
        self.held.profiles.lock().unwrap().clone()
    }
    fn persist(self: Box<Self>, config: &Path, _: StoreKind) -> Result<Secret, AuthError> {
        if matches!(self.mode, Mode::Stored) {
            write_profile_for(config, &self.handle, self.provider);
        }
        Ok(Secret::new(&self.handle))
    }
}

impl Drop for FakePending {
    fn drop(&mut self) {
        self.drops.fetch_add(1, Ordering::SeqCst);
    }
}

impl PendingSubscriptionLogin for FakePending {
    fn authorization_url(&self) -> &str {
        "https://example.invalid/authorize?state=STATE_CANARY"
    }
    fn uses_loopback(&self) -> bool {
        self.uses_loopback
    }
    fn check(&self, response: Option<&Secret>) -> Result<(), AuthError> {
        if self.uses_loopback {
            return Ok(());
        }
        if self.provider == SubscriptionProvider::Antigravity {
            return oauth::parse_redirect_url(
                response.map(Secret::expose).unwrap_or_default(),
                "STATE_CANARY",
                "/oauth-callback",
            )
            .map(drop);
        }
        let (_, state) =
            wisp_protocol::split_code_state(response.map(Secret::expose).unwrap_or_default())
                .map_err(AuthError::Pasted)?;
        if state == "STATE_CANARY" {
            Ok(())
        } else {
            Err(AuthError::State)
        }
    }
    fn retry(&self) -> Option<Box<dyn PendingSubscriptionLogin>> {
        Some(Box::new(FakePending {
            provider: self.provider,
            mode: match self.mode {
                Mode::UnreachableTwice => Mode::Unreachable,
                Mode::Unreachable => Mode::Stored,
                mode => mode,
            },
            uses_loopback: self.uses_loopback,
            handle: self.handle.clone(),
            drops: self.drops.clone(),
            entered: self.entered.clone(),
            held: self.held.clone(),
        }))
    }
    fn finish(
        self: Box<Self>,
        response: Option<Secret>,
        _: PathBuf,
    ) -> BoxFuture<'static, Result<Box<dyn Approved>, AuthError>> {
        Box::pin(async move {
            if self.uses_loopback {
                assert!(response.is_none());
            } else if self.provider == SubscriptionProvider::Antigravity {
                let code = oauth::parse_redirect_url(
                    response.as_ref().map(Secret::expose).unwrap_or_default(),
                    "STATE_CANARY",
                    "/oauth-callback",
                )?;
                assert_eq!(code, "CODE_CANARY");
            } else {
                assert_eq!(
                    response.as_ref().map(Secret::expose),
                    Some("CODE_CANARY#STATE_CANARY")
                );
            }
            self.entered.notify_one();
            match self.mode {
                Mode::Waiting => std::future::pending::<()>().await,
                Mode::Unreachable | Mode::UnreachableTwice => return Err(AuthError::Network),
                Mode::Refused => return Err(AuthError::Refused(400)),
                Mode::Absent | Mode::Stored => {}
            }
            if let Some(during) = self.held.during.lock().unwrap().take() {
                during();
            }
            Ok(Box::new(FakeApproved {
                provider: self.provider,
                mode: self.mode,
                handle: self.handle.clone(),
                held: self.held.clone(),
            }) as Box<dyn Approved>)
        })
    }
}

pub(crate) fn write_profile(config: &Path, handle: &str) {
    write_profile_for(config, handle, SubscriptionProvider::ChatGpt);
}

fn write_profile_for(config: &Path, handle: &str, provider: SubscriptionProvider) {
    let profile = oauth::profile_path(config, handle).unwrap();
    let mut directories = std::fs::DirBuilder::new();
    directories.recursive(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::DirBuilderExt;
        directories.mode(0o700);
    }
    directories.create(profile.parent().unwrap()).unwrap();
    let kind = match provider {
        SubscriptionProvider::ChatGpt => "chat_gpt",
        SubscriptionProvider::Claude => "anthropic",
        SubscriptionProvider::Antigravity => "antigravity",
    };
    let bytes = serde_json::to_vec(&serde_json::json!({"version":1,"kind":kind,"access_token":"synthetic-access","refresh_token":"synthetic-refresh","expires_at":4102444800_u64,"generation":0})).unwrap();
    wisp_core::secrets::write_atomic(&profile, &bytes, 0o600).unwrap();
}

struct Quiet;
impl Provider for Quiet {
    fn stream(
        &self,
        _: &wisp_core::provider::Request<'_>,
    ) -> futures::stream::BoxStream<
        'static,
        Result<wisp_core::provider::ProviderEvent, wisp_core::provider::ProviderError>,
    > {
        Box::pin(futures::stream::pending())
    }
    fn cache_prefix(&self, _: &wisp_core::provider::Request<'_>) -> Option<String> {
        None
    }
}

struct Fixture {
    dir: crate::scratch::ScratchDir,
    host: Arc<CliEndpointHost>,
    fake: Arc<Fake>,
}

impl Fixture {
    fn new(mode: Mode) -> Self {
        Self::new_for(mode, SubscriptionProvider::ChatGpt)
    }

    fn new_for(mode: Mode, kind: SubscriptionProvider) -> Self {
        let dir = crate::scratch_dir("session-auth");
        let fake = Arc::new(Fake {
            provider: kind,
            mode,
            starts: Arc::default(),
            drops: Arc::default(),
            entered: Arc::default(),
            held: Held::default(),
        });
        let provider: Arc<dyn Provider> = Arc::new(Quiet);
        let connection = wisp_core::agent::Connection {
            name: "base".to_owned(),
            provider: provider.clone(),
            context: wisp_core::agent::LogContext {
                endpoint: "https://example.invalid".to_owned(),
                endpoint_name: "base".to_owned(),
                credential_source: wisp_protocol::log::CredentialSource::Environment,
                redactions: Default::default(),
            },
            priced: true,
        };
        let catalog = LiveCatalog::new(wisp_core::kit::Connected {
            provider,
            ready: vec![connection],
            unavailable: Vec::new(),
            active: "base".to_owned(),
            env_account: None,
        })
        .with_summaries(vec![wisp_protocol::settings::EndpointSummary {
            models: Vec::new(),
            mark: Some(1),
            name: "base".into(),
            provider: "openai".into(),
            credential: Some(wisp_protocol::settings::CredentialKind::ApiKey),
            official: Some(true),
            host: None,
            unavailable: None,
            priced: true,
            default: false,
            keys_file: false,
            expired: false,
            key_var: None,
            voice_model: None,
            complete: false,
            default_model: None,
        }]);
        let env_file = dir.root.join("synthetic.env");
        std::fs::write(&env_file, "").unwrap();
        let host = CliEndpointHost {
            catalog: Some(catalog),
            auth: super::super::SessionAuth::new(vec![fake.clone()], Arc::new(|_: &str| false)),
            env_file: Some(env_file),
            config: Some(dir.root.join("config.toml")),
            endpoint: None,
            model: match kind {
                SubscriptionProvider::ChatGpt => "gpt-6.1-sol",
                SubscriptionProvider::Claude => "claude-sonnet-5-5",
                SubscriptionProvider::Antigravity => {
                    wisp_core::provider::gemini::antigravity_default_model()
                }
            }
            .to_owned(),
            xdg_config_home: None,
            home: None,
            keychain: Arc::new(crate::keychain::Keychain::off()),
            budget: Budget::Short,
            set: Vec::new(),
            models: None,
        };
        Self {
            dir,
            host: Arc::new(host),
            fake,
        }
    }

    async fn start(&self, request: u64, alias: &str, reauthorize: bool) -> AuthOutcome {
        session_auth(
            &self.host,
            AuthCommand::Start {
                request: RequestId(request),
                provider: SubscriptionProvider::ChatGpt,
                alias: Some(alias.to_owned()),
                reauthorize,
            },
        )
        .await
        .outcome
    }
    async fn finish(&self, login: u64) -> AuthOutcome {
        session_auth(
            &self.host,
            AuthCommand::Finish {
                request: RequestId(login + 100),
                login: RequestId(login),
                response: None,
                alias: None,
            },
        )
        .await
        .outcome
    }
}

#[test]
fn provider_kind_agrees_with_each_subscription_kind() {
    for kind in SubscriptionKind::ALL {
        assert_eq!(
            provider_kind(SubscriptionProvider::from(kind)),
            kind.provider()
        );
        assert_eq!(
            wire_provider(kind.provider()),
            SubscriptionProvider::from(kind)
        );
    }
}

#[tokio::test]
async fn saved_not_ready_can_reload_without_restarting_oauth() {
    let fixture = Fixture::new(Mode::Absent);
    assert!(matches!(
        fixture.start(1, "plan", false).await,
        AuthOutcome::Challenge { .. }
    ));
    assert!(matches!(
        fixture.finish(1).await,
        AuthOutcome::SavedNotReady {
            endpoint: SubscriptionEndpoint {
                state: LocalState::Unavailable,
                ..
            },
            ..
        }
    ));
    let outcome = session_auth(
        &fixture.host,
        AuthCommand::Reload {
            request: RequestId(3),
            alias: "plan".to_owned(),
        },
    )
    .await
    .outcome;
    assert!(matches!(
        outcome,
        AuthOutcome::SavedNotReady {
            endpoint: SubscriptionEndpoint {
                state: LocalState::Unavailable,
                ..
            },
            previous_cleanup_pending: false,
        }
    ));
    assert_eq!(fixture.fake.starts.load(Ordering::SeqCst), 1);
    assert!(fixture.host.auth.pending().is_empty());
    write_profile(
        &fixture.dir.root.join("config.toml"),
        "oauth:00000000000000000000000000000001",
    );
    let recovered = session_auth(
        &fixture.host,
        AuthCommand::Reload {
            request: RequestId(4),
            alias: "plan".to_owned(),
        },
    )
    .await
    .outcome;
    assert!(matches!(
        recovered,
        AuthOutcome::Ready {
            endpoint: SubscriptionEndpoint {
                state: LocalState::Working,
                ..
            },
            previous_cleanup_pending: false,
        }
    ));
    let catalog = fixture.host.catalog.as_ref().unwrap();
    assert!(
        catalog
            .view()
            .ready
            .iter()
            .any(|connection| connection.name == "plan")
    );
    assert_eq!(fixture.fake.starts.load(Ordering::SeqCst), 1);
    let ready = catalog
        .view()
        .ready
        .into_iter()
        .find(|connection| connection.name == "plan")
        .unwrap();
    assert!(ready.provider.generation().unwrap().admit().is_ok());
}

#[tokio::test]
async fn synthetic_login_activates_immediately_and_reauth_retires_old_handle() {
    use wisp_core::kit::Connect;
    let fixture = Fixture::new(Mode::Stored);
    assert!(matches!(
        fixture.start(1, "plan", false).await,
        AuthOutcome::Challenge { .. }
    ));
    assert!(matches!(fixture.finish(1).await, AuthOutcome::Ready { .. }));
    let defaults = || {
        let config = std::fs::read_to_string(fixture.dir.root.join("config.toml")).unwrap();
        config
            .lines()
            .filter(|line| line.starts_with("default = "))
            .map(str::to_owned)
            .collect::<Vec<_>>()
    };
    assert_eq!(defaults(), ["default = \"plan\""]);
    let catalog = fixture.host.catalog.as_ref().unwrap();
    let old = catalog
        .connect("/child")
        .unwrap()
        .ready
        .into_iter()
        .find(|connection| connection.name == "plan")
        .unwrap()
        .provider;
    assert!(matches!(
        fixture.start(2, "plan", true).await,
        AuthOutcome::Challenge { .. }
    ));
    let first = oauth::profile_path(
        &fixture.dir.root.join("config.toml"),
        "oauth:00000000000000000000000000000001",
    )
    .unwrap();
    std::fs::remove_file(&first).unwrap();
    std::fs::create_dir(&first).unwrap();
    assert!(matches!(
        fixture.finish(2).await,
        AuthOutcome::Ready {
            previous_cleanup_pending: true,
            ..
        }
    ));
    assert!(old.generation().unwrap().admit().is_err());
    let current = catalog
        .connect("/resumed")
        .unwrap()
        .ready
        .into_iter()
        .find(|connection| connection.name == "plan")
        .unwrap()
        .provider;
    assert!(current.generation().unwrap().admit().is_ok());
    let outcome = session_auth(&fixture.host, logout(3, "plan")).await.outcome;
    assert!(matches!(
        outcome,
        AuthOutcome::LoggedOut {
            cleanup_pending: false,
            ..
        }
    ));
    assert!(current.generation().unwrap().admit().is_err());
    assert!(
        !catalog
            .view()
            .ready
            .iter()
            .any(|connection| connection.name == "plan")
    );
    assert!(matches!(
        fixture.start(4, "plan", false).await,
        AuthOutcome::Challenge { .. }
    ));
    assert!(matches!(
        fixture.finish(4).await,
        AuthOutcome::Ready {
            previous_cleanup_pending: false,
            ..
        }
    ));
    assert_eq!(
        *fixture.fake.held.own.lock().unwrap(),
        vec![Some(first), None]
    );
    fixture.start(5, "work", false).await;
    assert!(matches!(fixture.finish(5).await, AuthOutcome::Ready { .. }));
    assert_eq!(defaults(), ["default = \"plan\""]);
}

#[tokio::test]
async fn cancellation_drops_inflight_oauth_without_saving() {
    let fixture = Fixture::new(Mode::Waiting);
    fixture.start(1, "plan", false).await;
    let host = fixture.host.clone();
    let finishing = tokio::spawn(async move {
        session_auth(
            &host,
            AuthCommand::Finish {
                request: RequestId(2),
                login: RequestId(1),
                response: None,
                alias: None,
            },
        )
        .await
    });
    fixture.fake.entered.notified().await;
    let duplicate = session_auth(
        &fixture.host,
        AuthCommand::Finish {
            request: RequestId(4),
            login: RequestId(1),
            response: None,
            alias: None,
        },
    )
    .await;
    assert!(matches!(
        duplicate.outcome,
        AuthOutcome::Failed {
            kind: AuthFailureKind::Busy,
            ..
        }
    ));
    assert!(fixture.host.auth.pending().contains_key(&RequestId(1)));
    assert_eq!(fixture.fake.drops.load(Ordering::SeqCst), 0);
    let cancelled = session_auth(
        &fixture.host,
        AuthCommand::Cancel {
            request: RequestId(3),
            login: RequestId(1),
        },
    )
    .await;
    assert!(matches!(cancelled.outcome, AuthOutcome::Cancelled { .. }));
    assert!(matches!(
        finishing.await.unwrap().outcome,
        AuthOutcome::Cancelled { .. }
    ));
    assert_eq!(fixture.fake.drops.load(Ordering::SeqCst), 2);
    assert!(!fixture.dir.root.join("config.toml").exists());
    assert!(
        !fixture
            .files()
            .iter()
            .any(|(path, _)| path.to_string_lossy().contains("oauth-"))
    );
}

#[tokio::test(start_paused = true)]
async fn pending_login_timeout_releases_challenge_without_finish() {
    let fixture = Fixture::new(Mode::Waiting);
    fixture.start(1, "plan", false).await;
    tokio::task::yield_now().await;
    tokio::time::advance(std::time::Duration::from_secs(600)).await;
    tokio::task::yield_now().await;
    assert!(fixture.host.auth.pending().is_empty());
    assert_eq!(fixture.fake.drops.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn unregistered_claude_is_refused_without_starting_oauth() {
    let fixture = Fixture::new(Mode::Stored);
    let result = session_auth(
        &fixture.host,
        AuthCommand::Start {
            request: RequestId(1),
            provider: SubscriptionProvider::Claude,
            alias: None,
            reauthorize: false,
        },
    )
    .await;
    assert!(matches!(
        result.outcome,
        AuthOutcome::Failed {
            kind: AuthFailureKind::UnsupportedProvider,
            ..
        }
    ));
    assert_eq!(fixture.fake.starts.load(Ordering::SeqCst), 0);
}

#[tokio::test]
async fn the_default_and_a_hidden_preset_no_longer_refuse_and_an_unknown_name_lists_the_accounts() {
    for preset in [false, true] {
        let fixture = Fixture::new(Mode::Stored);
        fixture.start(1, "plan", false).await;
        assert!(matches!(fixture.finish(1).await, AuthOutcome::Ready { .. }));
        let path = fixture.dir.root.join("config.toml");
        if preset {
            let text = std::fs::read_to_string(&path).unwrap();
            std::fs::write(&path, format!("{text}\n[agents.presets.abcdefghij0123456789xyz]\nendpoint = \"plan\"\neffort = \"invalid\"\n")).unwrap();
        } else {
            wisp_core::secrets::apply(
                &path,
                wisp_core::endpoints::PathShown::Given,
                wisp_core::secrets::Change::SetDefault {
                    name: EndpointName::parse("plan").unwrap(),
                },
                fixture.host.keychain.access(),
                wisp_core::secrets::LOCK_WAIT,
            )
            .unwrap();
        }
        let unknown = session_auth(&fixture.host, logout(2, "work")).await;
        let AuthOutcome::Failed { kind, message, .. } = unknown.outcome else {
            panic!("expected the unknown name refused")
        };
        assert_eq!(kind, AuthFailureKind::InvalidAlias);
        assert_eq!(message, "there's no account called work; yours are plan.");
        let result = session_auth(&fixture.host, logout(3, "plan")).await;
        assert!(matches!(
            result.outcome,
            AuthOutcome::LoggedOut {
                new_start: None,
                ..
            }
        ));
        let text = std::fs::read_to_string(&path).unwrap();
        assert!(!text.contains("[endpoints.plan]"));
    }
}

#[tokio::test]
async fn cancelled_login_request_ids_cannot_be_reused_for_a_different_challenge() {
    let fixture = Fixture::new(Mode::Waiting);
    fixture.start(1, "first", false).await;
    session_auth(
        &fixture.host,
        AuthCommand::Cancel {
            request: RequestId(2),
            login: RequestId(1),
        },
    )
    .await;
    assert!(matches!(
        fixture.start(1, "second", false).await,
        AuthOutcome::Failed {
            kind: AuthFailureKind::StaleRequest,
            ..
        }
    ));
    assert_eq!(fixture.fake.starts.load(Ordering::SeqCst), 1);
}

mod tui;

impl Fixture {
    async fn send(&self, command: AuthCommand) -> AuthOutcome {
        session_auth(&self.host, command).await.outcome
    }

    async fn claude_start(
        &self,
        request: u64,
        alias: Option<&str>,
        reauthorize: bool,
    ) -> AuthOutcome {
        self.send(AuthCommand::Start {
            request: RequestId(request),
            provider: SubscriptionProvider::Claude,
            alias: alias.map(str::to_owned),
            reauthorize,
        })
        .await
    }

    async fn paste(
        &self,
        request: u64,
        login: u64,
        code: &str,
        alias: Option<&str>,
    ) -> AuthOutcome {
        self.send(AuthCommand::Finish {
            request: RequestId(request),
            login: RequestId(login),
            response: Some(Secret::new(code)),
            alias: alias.map(str::to_owned),
        })
        .await
    }

    fn files(&self) -> Vec<(PathBuf, Vec<u8>)> {
        let mut out = Vec::new();
        let mut stack = vec![self.dir.root.clone()];
        while let Some(dir) = stack.pop() {
            for entry in std::fs::read_dir(&dir).unwrap() {
                let path = entry.unwrap().path();
                if path.is_dir() {
                    stack.push(path);
                } else {
                    out.push((path.clone(), std::fs::read(&path).unwrap()));
                }
            }
        }
        out.sort();
        out
    }
}

fn rejected(outcome: &AuthOutcome) -> Option<Option<wisp_protocol::CodeProblem>> {
    match outcome {
        AuthOutcome::Failed {
            kind: AuthFailureKind::CodeRejected,
            problem,
            message,
            ..
        } => {
            assert!(!message.contains("CODE_CANARY"), "{message}");
            assert!(!message.contains("STATE_CANARY"), "{message}");
            Some(*problem)
        }
        _ => None,
    }
}

#[tokio::test]
async fn a_foreign_state_keeps_the_pending_login() {
    let fixture = Fixture::new_for(Mode::Stored, SubscriptionProvider::Claude);
    assert!(matches!(
        fixture.claude_start(1, None, false).await,
        AuthOutcome::Challenge { ref challenge } if challenge.alias == "claude" && !challenge.opened
    ));
    let foreign = fixture.paste(2, 1, "CODE_CANARY#OTHER_SIGN_IN", None).await;
    assert_eq!(rejected(&foreign), Some(None));
    let doubled = fixture
        .paste(
            3,
            1,
            "CODE_CANARY#STATE_CANARYCODE_CANARY#STATE_CANARY",
            None,
        )
        .await;
    assert_eq!(
        rejected(&doubled),
        Some(Some(wisp_protocol::CodeProblem::Repeated))
    );
    let empty = fixture.paste(4, 1, "\r\n", None).await;
    assert_eq!(
        rejected(&empty),
        Some(Some(wisp_protocol::CodeProblem::Empty))
    );
    assert!(fixture.host.auth.pending().contains_key(&RequestId(1)));
    let done = fixture.paste(5, 1, "CODE_CANARY#STATE_CANARY", None).await;
    assert!(
        matches!(done, AuthOutcome::Ready { ref endpoint, .. } if endpoint.alias == "claude"),
        "{done:?}"
    );
    assert_eq!(fixture.fake.starts.load(Ordering::SeqCst), 1);
    assert!(fixture.host.auth.pending().is_empty());
}

#[tokio::test]
async fn a_transport_failure_keeps_the_login_for_one_retry() {
    let fixture = Fixture::new_for(Mode::Unreachable, SubscriptionProvider::Claude);
    fixture.claude_start(1, None, false).await;
    let first = fixture.paste(2, 1, "CODE_CANARY#STATE_CANARY", None).await;
    assert!(
        matches!(
            first,
            AuthOutcome::Failed {
                kind: AuthFailureKind::Unreachable,
                ..
            }
        ),
        "{first:?}"
    );
    assert!(fixture.host.auth.pending().contains_key(&RequestId(1)));
    let second = fixture.paste(3, 1, "CODE_CANARY#STATE_CANARY", None).await;
    assert!(matches!(second, AuthOutcome::Ready { .. }), "{second:?}");

    let fixture = Fixture::new_for(Mode::UnreachableTwice, SubscriptionProvider::Claude);
    fixture.claude_start(1, None, false).await;
    for request in [2, 3] {
        let failed = fixture
            .paste(request, 1, "CODE_CANARY#STATE_CANARY", None)
            .await;
        assert!(
            matches!(
                failed,
                AuthOutcome::Failed {
                    kind: AuthFailureKind::Unreachable,
                    ..
                }
            ),
            "{failed:?}"
        );
    }
    assert!(fixture.host.auth.pending().is_empty());
    let gone = fixture.paste(4, 1, "CODE_CANARY#STATE_CANARY", None).await;
    assert!(matches!(
        gone,
        AuthOutcome::Failed {
            kind: AuthFailureKind::StaleRequest,
            ..
        }
    ));
}

#[tokio::test]
async fn a_code_the_provider_refuses_ends_the_login() {
    let fixture = Fixture::new_for(Mode::Refused, SubscriptionProvider::Claude);
    fixture.claude_start(1, None, false).await;
    let refused = fixture.paste(2, 1, "CODE_CANARY#STATE_CANARY", None).await;
    assert!(
        matches!(
            refused,
            AuthOutcome::Failed {
                kind: AuthFailureKind::Unavailable,
                ..
            }
        ),
        "{refused:?}"
    );
    assert!(fixture.host.auth.pending().is_empty());
}

#[tokio::test]
async fn a_name_another_window_took_asks_again_and_keeps_the_login() {
    let fixture = Fixture::new_for(Mode::Stored, SubscriptionProvider::Claude);
    let other = Fixture::new_for(Mode::Stored, SubscriptionProvider::Claude);
    let host = CliEndpointHost {
        catalog: None,
        auth: other.host.auth.clone(),
        ..(*fixture.host).clone()
    };
    fixture.claude_start(1, None, false).await;
    session_auth(
        &host,
        AuthCommand::Start {
            request: RequestId(10),
            provider: SubscriptionProvider::Claude,
            alias: None,
            reauthorize: false,
        },
    )
    .await;
    let taken = session_auth(
        &host,
        AuthCommand::Finish {
            request: RequestId(11),
            login: RequestId(10),
            response: Some(Secret::new("CODE_CANARY#STATE_CANARY")),
            alias: None,
        },
    )
    .await
    .outcome;
    assert!(matches!(taken, AuthOutcome::Ready { .. }), "{taken:?}");
    let conflict = fixture.paste(2, 1, "CODE_CANARY#STATE_CANARY", None).await;
    assert!(
        matches!(conflict, AuthOutcome::Failed { kind: AuthFailureKind::AliasConflict, ref message, .. } if message == "claude is taken."),
        "{conflict:?}"
    );
    assert!(fixture.host.auth.pending().contains_key(&RequestId(1)));
    let renamed = fixture
        .paste(3, 1, "CODE_CANARY#STATE_CANARY", Some("claude-home"))
        .await;
    assert!(
        matches!(
            renamed,
            AuthOutcome::Ready { ref endpoint, .. } | AuthOutcome::SavedNotReady { ref endpoint, .. }
                if endpoint.alias == "claude-home"
        ),
        "{renamed:?}"
    );
    assert_eq!(fixture.fake.starts.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn a_start_without_a_name_never_numbers_or_replaces_an_account() {
    let fixture = Fixture::new_for(Mode::Stored, SubscriptionProvider::Claude);
    fixture.claude_start(1, None, false).await;
    fixture.paste(2, 1, "CODE_CANARY#STATE_CANARY", None).await;
    let before = fixture.files();
    let again = fixture.claude_start(3, None, false).await;
    assert!(
        matches!(again, AuthOutcome::Failed { kind: AuthFailureKind::AliasConflict, ref message, .. } if message == "claude is taken."),
        "{again:?}"
    );
    assert_eq!(fixture.files(), before);
}

#[tokio::test]
async fn open_shows_the_page_again_through_the_opener() {
    let opened = Arc::new(AtomicUsize::new(0));
    let fixture = Fixture::new_for(Mode::Stored, SubscriptionProvider::Claude);
    let seen = opened.clone();
    let host = CliEndpointHost {
        auth: super::super::SessionAuth::new(
            vec![fixture.fake.clone()],
            Arc::new(move |url: &str| {
                assert!(url.contains("STATE_CANARY"));
                seen.fetch_add(1, Ordering::SeqCst);
                true
            }),
        ),
        ..(*fixture.host).clone()
    };
    let start = session_auth(
        &host,
        AuthCommand::Start {
            request: RequestId(1),
            provider: SubscriptionProvider::Claude,
            alias: None,
            reauthorize: false,
        },
    )
    .await
    .outcome;
    assert!(matches!(start, AuthOutcome::Challenge { ref challenge } if challenge.opened));
    let again = session_auth(
        &host,
        AuthCommand::Open {
            request: RequestId(2),
            login: RequestId(1),
        },
    )
    .await;
    assert!(again.is_private());
    assert!(
        matches!(again.outcome, AuthOutcome::Challenge { ref challenge } if challenge.opened && challenge.login == RequestId(1))
    );
    assert_eq!(opened.load(Ordering::SeqCst), 2);
    let stale = session_auth(
        &host,
        AuthCommand::Open {
            request: RequestId(3),
            login: RequestId(9),
        },
    )
    .await;
    assert!(matches!(
        stale.outcome,
        AuthOutcome::Failed {
            kind: AuthFailureKind::StaleRequest,
            ..
        }
    ));
}

struct Answers(std::collections::VecDeque<&'static str>);

impl crate::auth::login::Front for Answers {
    fn say(&mut self, _: &str) {}
    fn choose(
        &mut self,
        rows: &[crate::auth::login::Row],
    ) -> Result<usize, crate::auth::login::Stop> {
        let answer = self
            .0
            .pop_front()
            .ok_or(crate::auth::login::Stop::Cancelled)?;
        crate::auth::login::pick_answer(answer, rows).ok_or(crate::auth::login::Stop::Cancelled)
    }
    fn line(&mut self, _: &str, _: &str) -> Result<String, crate::auth::login::Stop> {
        self.0
            .pop_front()
            .map(str::to_owned)
            .ok_or(crate::auth::login::Stop::Cancelled)
    }
    fn code(&mut self, question: &str) -> Result<String, crate::auth::login::Stop> {
        self.line(question, "")
    }
    fn enter(&mut self) -> Result<(), crate::auth::login::Stop> {
        Err(crate::auth::login::Stop::Cancelled)
    }
    fn wait(&mut self, done: &dyn Fn() -> bool) -> Result<(), crate::auth::login::Stop> {
        while !done() {
            std::thread::sleep(std::time::Duration::from_millis(5));
        }
        Ok(())
    }
}

async fn existing_claude() -> (Fixture, Vec<(PathBuf, Vec<u8>)>) {
    let fixture = Fixture::new_for(Mode::Stored, SubscriptionProvider::Claude);
    fixture
        .claude_start(1000, Some("claude-subscription"), false)
        .await;
    let done = fixture
        .paste(1001, 1000, "CODE_CANARY#STATE_CANARY", None)
        .await;
    assert!(matches!(done, AuthOutcome::Ready { .. }), "{done:?}");
    let files = fixture.files();
    assert!(
        files
            .iter()
            .any(|(path, _)| path.to_string_lossy().contains("oauth-"))
    );
    (fixture, files)
}

#[tokio::test]
async fn a_second_name_for_one_account_is_refused_and_writes_nothing() {
    let (fixture, before) = existing_claude().await;
    let config = fixture.dir.root.join("config.toml");
    let first = oauth::profile_path(&config, "oauth:00000000000000000000000000000001").unwrap();
    *fixture.fake.held.profiles.lock().unwrap() = vec![first.clone()];
    fixture.claude_start(1, Some("claude"), false).await;
    let refused = fixture.paste(2, 1, "CODE_CANARY#STATE_CANARY", None).await;
    assert_eq!(
        refused,
        AuthOutcome::Failed {
            kind: AuthFailureKind::Declined,
            message: same_account_reason(SubscriptionProvider::Claude, "claude-subscription"),
            problem: None,
            same_account: Some("claude-subscription".to_owned()),
        }
    );
    assert_eq!(fixture.files(), before);
    assert!(fixture.host.auth.pending().is_empty());
    let late = "oauth:00000000000000000000000000000009";
    *fixture.fake.held.profiles.lock().unwrap() = vec![oauth::profile_path(&config, late).unwrap()];
    let keychain = fixture.host.keychain.clone();
    let path = config.clone();
    *fixture.fake.held.during.lock().unwrap() = Some(Box::new(move || {
        use wisp_core::subscription_login::LoginPersistence;
        let current = super::super::login_config(&path, true).unwrap();
        Destination {
            path: &path,
            shown: wisp_core::endpoints::PathShown::Given,
            config: current.as_ref(),
            name: EndpointName::parse("late").unwrap(),
            endpoint: subscription_login::subscription_endpoint(
                SubscriptionKind::Anthropic,
                StoreKind::File,
            ),
            make_default: false,
            previous: None,
            keychain: keychain.access(),
        }
        .persist(wisp_protocol::Secret::new(late))
        .unwrap();
    }));
    fixture.claude_start(3, Some("claude"), false).await;
    let refused = fixture.paste(4, 3, "CODE_CANARY#STATE_CANARY", None).await;
    assert!(
        matches!(refused, AuthOutcome::Failed { ref same_account, .. } if same_account.as_deref() == Some("late")),
        "{refused:?}"
    );
    let saved = std::fs::read(&config).unwrap();
    *fixture.fake.held.profiles.lock().unwrap() = vec![first];
    let broken = config.clone();
    *fixture.fake.held.during.lock().unwrap() =
        Some(Box::new(move || std::fs::write(&broken, "[").unwrap()));
    fixture.claude_start(5, Some("claude"), false).await;
    let refused = fixture.paste(6, 5, "CODE_CANARY#STATE_CANARY", None).await;
    assert!(
        matches!(refused, AuthOutcome::Failed { ref same_account, .. } if same_account.as_deref() == Some("claude-subscription")),
        "{refused:?}"
    );
    std::fs::write(&config, saved).unwrap();
    let credentials = wisp_core::secrets::credentials_path(&config);
    *fixture.fake.held.during.lock().unwrap() = Some(Box::new(move || {
        std::fs::remove_file(&credentials).unwrap()
    }));
    let asked = fixture.fake.held.own.lock().unwrap().len();
    fixture
        .claude_start(7, Some("claude-subscription"), true)
        .await;
    fixture.paste(8, 7, "CODE_CANARY#STATE_CANARY", None).await;
    assert_eq!(fixture.fake.held.own.lock().unwrap().len(), asked);
}

#[tokio::test]
async fn the_provider_flag_without_a_terminal_refuses_and_changes_nothing() {
    let (fixture, before) = existing_claude().await;
    let host = fixture.host.clone();
    let code = tokio::task::spawn_blocking(move || {
        crate::auth::login::login(
            &host.context(),
            host.keychain.clone(),
            &crate::auth::login::Args {
                what: None,
                more: None,
                name: None,
                provider: Some(ProviderKind::Anthropic),
                store: None,
                make_default: false,
            },
        )
    })
    .await
    .unwrap();
    assert_eq!(code, std::process::ExitCode::FAILURE);
    assert_eq!(fixture.files(), before);
}

#[tokio::test]
async fn a_key_named_after_a_subscription_refuses_before_any_sign_in() {
    let (fixture, before) = existing_claude().await;
    let starts = fixture.fake.starts.load(Ordering::SeqCst);
    let host = fixture.host.clone();
    let code = tokio::task::spawn_blocking(move || {
        crate::auth::login::login(
            &host.context(),
            host.keychain.clone(),
            &crate::auth::login::Args {
                what: Some("key"),
                more: None,
                name: Some("claude-subscription"),
                provider: None,
                store: None,
                make_default: false,
            },
        )
    })
    .await
    .unwrap();
    assert_eq!(code, std::process::ExitCode::FAILURE);
    assert_eq!(fixture.fake.starts.load(Ordering::SeqCst), starts);
    assert!(fixture.host.auth.pending().is_empty());
    assert_eq!(fixture.files(), before);
}

#[tokio::test]
async fn the_provider_word_asks_and_a_cancel_changes_nothing() {
    for answers in [vec!["^C"], vec!["1", "^C"], vec!["2", "^C"]] {
        let (fixture, before) = existing_claude().await;
        let accounts = crate::auth::accounts::gather(&fixture.host.context()).unwrap();
        assert_eq!(accounts.of(SubscriptionProvider::Claude).len(), 1);
        let mut front = Answers(answers.iter().copied().filter(|a| *a != "^C").collect());
        let planned = crate::auth::login::plan_subscription(
            &mut front,
            &accounts,
            SubscriptionProvider::Claude,
            None,
            true,
        );
        if let Ok(want) = planned {
            assert_eq!(want.again, answers[0] == "1");
            let result = crate::auth::login::subscribe(
                &*fixture.host,
                &mut front,
                &accounts,
                want,
                crate::auth::login::Prefs::default(),
            )
            .await;
            assert_eq!(result, Err(crate::auth::login::Stop::Cancelled));
        }
        assert_eq!(fixture.files(), before, "{answers:?}");
        assert!(fixture.host.auth.pending().is_empty());
    }
}

#[tokio::test]
async fn naming_the_existing_account_signs_in_to_it_again() {
    let (fixture, _) = existing_claude().await;
    let accounts = crate::auth::accounts::gather(&fixture.host.context()).unwrap();
    let mut front = Answers(["CODE_CANARY#STATE_CANARY"].into_iter().collect());
    let want = crate::auth::login::plan_subscription(
        &mut front,
        &accounts,
        SubscriptionProvider::Claude,
        Some("claude-subscription".to_owned()),
        false,
    )
    .unwrap();
    assert!(want.again);
    let done = crate::auth::login::subscribe(
        &*fixture.host,
        &mut front,
        &accounts,
        want,
        crate::auth::login::Prefs::default(),
    )
    .await
    .unwrap();
    assert_eq!(done.name, "claude-subscription");
    assert_eq!(fixture.fake.starts.load(Ordering::SeqCst), 2);
}

#[tokio::test]
async fn signing_in_again_works_on_the_host_wisp_login_builds() {
    let (fixture, _) = existing_claude().await;
    let host = CliEndpointHost {
        auth: fixture.host.auth.clone(),
        ..CliEndpointHost::of(
            &fixture.host.context(),
            None,
            fixture.host.keychain.clone(),
            wisp_core::secrets::keyring::Budget::Fresh(
                wisp_core::secrets::keyring::TERMINAL_DEADLINE,
            ),
            &[],
        )
    };
    assert!(host.catalog.is_none());
    let send = |command| session_auth(&host, command);
    let started = send(AuthCommand::Start {
        request: RequestId(2000),
        provider: SubscriptionProvider::Claude,
        alias: Some("claude-subscription".to_owned()),
        reauthorize: true,
    })
    .await
    .outcome;
    assert!(
        matches!(started, AuthOutcome::Challenge { .. }),
        "{started:?}"
    );
    let done = send(AuthCommand::Finish {
        request: RequestId(2001),
        login: RequestId(2000),
        response: Some(Secret::new("CODE_CANARY#STATE_CANARY")),
        alias: None,
    })
    .await
    .outcome;
    assert!(
        matches!(
            done,
            AuthOutcome::Ready { ref endpoint, .. } | AuthOutcome::SavedNotReady { ref endpoint, .. }
                if endpoint.alias == "claude-subscription"
        ),
        "{done:?}"
    );
    assert_eq!(fixture.fake.starts.load(Ordering::SeqCst), 2);
}

#[tokio::test]
async fn a_missing_key_and_an_unset_variable_each_show_their_own_state() {
    let (fixture, _) = existing_claude().await;
    let config = fixture.dir.root.join("config.toml");
    let mut text = std::fs::read_to_string(&config).unwrap();
    text.push_str(
        "\n[endpoints.gone]\nauth = \"api_key\"\nkey_store = \"file\"\n\n[endpoints.shell]\nauth = \"api_key\"\nkey_env = \"WISP_TEST_ACCOUNTS_NEVER_SET\"\n",
    );
    std::fs::write(&config, text).unwrap();
    let accounts = crate::auth::accounts::gather(&fixture.host.context()).unwrap();
    let health = |name: &str| accounts.get(name).map(|account| account.health);
    assert_eq!(
        health("claude-subscription"),
        Some(crate::auth::accounts::Health::SignedIn)
    );
    assert_eq!(
        health("gone"),
        Some(crate::auth::accounts::Health::KeyMissing)
    );
    assert_eq!(health("shell"), Some(crate::auth::accounts::Health::Unset));
    let profile = oauth::profile_path(&config, "oauth:00000000000000000000000000000001").unwrap();
    let lapsed = serde_json::json!({"version":1,"kind":"anthropic","access_token":"synthetic-access","refresh_token":"synthetic-refresh","expires_at":1_u64,"generation":0});
    wisp_core::secrets::write_atomic(&profile, &serde_json::to_vec(&lapsed).unwrap(), 0o600)
        .unwrap();
    let state = |outcome: AuthOutcome| match outcome {
        AuthOutcome::Status { endpoints } => endpoints
            .into_iter()
            .find(|e| e.alias == "claude-subscription")
            .map(|e| e.state),
        other => panic!("{other:?}"),
    };
    let status = || AuthCommand::Status {
        request: RequestId(3000),
        alias: None,
    };
    assert_eq!(
        state(fixture.send(status()).await),
        Some(LocalState::Refreshable)
    );
    wisp_core::secrets::write_atomic(&profile.with_extension("refused"), &[], 0o600).unwrap();
    assert_eq!(
        state(fixture.send(status()).await),
        Some(LocalState::Expired)
    );
    let accounts = crate::auth::accounts::gather(&fixture.host.context()).unwrap();
    assert_eq!(
        accounts.get("claude-subscription").map(|a| a.health),
        Some(crate::auth::accounts::Health::Expired)
    );
    let book = fixture.host.context().sources().book().unwrap();
    let expired = |name: &str| {
        crate::endpoints::summaries(&book.entries, &book.marks, &[], None, &[])
            .into_iter()
            .find(|e| e.name == name)
            .map(|e| e.expired)
    };
    assert_eq!(expired("claude-subscription"), Some(true));
    assert_eq!(expired("gone"), Some(false));
    let panel: String = crate::auth::accounts::panel(
        &accounts,
        Some("claude-subscription"),
        &Default::default(),
        80,
        &wisp_render::theme::glyphs::Glyphs::of(wisp_render::theme::glyphs::GlyphSet::Unicode),
        true,
        0,
    )
    .iter()
    .flat_map(|line| line.spans.iter().map(|span| span.text.clone()))
    .collect();
    assert!(panel.contains("sign-in expired"), "{panel}");
    assert!(panel.contains("wisp login"), "{panel}");
}

#[tokio::test]
async fn a_finish_naming_the_account_being_signed_in_again_is_still_a_change_to_it() {
    let (fixture, _) = existing_claude().await;
    fixture
        .claude_start(2000, Some("claude-subscription"), true)
        .await;
    let finish = |alias: &str| AuthCommand::Finish {
        request: RequestId(2001),
        login: RequestId(2000),
        response: None,
        alias: Some(alias.to_owned()),
    };
    let catalog = fixture.host.catalog.as_ref();
    assert_eq!(
        fixture
            .host
            .auth
            .alias(&finish("claude-subscription"), catalog),
        Some(("claude-subscription".to_owned(), true))
    );
    assert_eq!(
        fixture.host.auth.alias(&finish("claude-new"), catalog),
        Some(("claude-new".to_owned(), false))
    );
    let start = |alias: &str| AuthCommand::Start {
        request: RequestId(2002),
        provider: SubscriptionProvider::Claude,
        alias: Some(alias.to_owned()),
        reauthorize: true,
    };
    assert_eq!(
        fixture
            .host
            .auth
            .alias(&start("claude-subscription"), catalog),
        Some(("claude-subscription".to_owned(), true))
    );
    assert_eq!(
        fixture.host.auth.alias(&start("claude-new"), catalog),
        Some(("claude-new".to_owned(), false))
    );
}

#[tokio::test]
async fn the_name_typed_at_the_step_is_the_account_saved() {
    let fixture = Fixture::new_for(Mode::Stored, SubscriptionProvider::Claude);
    let accounts = crate::auth::accounts::gather(&fixture.host.context()).unwrap();
    let mut front = Answers(
        ["Claude1", "CODE_CANARY#STATE_CANARY"]
            .into_iter()
            .collect(),
    );
    let want = crate::auth::login::plan_subscription(
        &mut front,
        &accounts,
        SubscriptionProvider::Claude,
        None,
        true,
    )
    .unwrap();
    assert_eq!(want.name, "claude1");
    let done = crate::auth::login::subscribe(
        &*fixture.host,
        &mut front,
        &accounts,
        want,
        crate::auth::login::Prefs::default(),
    )
    .await
    .unwrap();
    assert_eq!(done.name, "claude1");
    let config = std::fs::read_to_string(fixture.dir.root.join("config.toml")).unwrap();
    assert!(config.contains("[endpoints.claude1]"), "{config}");
    assert!(!config.contains("claude-subscription"), "{config}");
}

async fn save(host: &CliEndpointHost, name: &str) -> wisp_core::endpoints::host::SaveOutcome {
    use wisp_core::endpoints::host::EndpointHost;
    host.save(
        wisp_protocol::endpoint::EndpointSpec {
            name: name.into(),
            provider: "anthropic".into(),
            auth: "bearer".into(),
            url: None,
            store: wisp_protocol::endpoint::StoreChoice::File,
            default: false,
        },
        Secret::new("placeholder"),
        false,
        tokio::sync::watch::channel(false).1,
    )
    .await
    .unwrap()
}

#[tokio::test]
async fn saved_accounts_reload_even_when_the_original_startup_account_is_missing() {
    use wisp_core::endpoints::host::{EditOutcome, EndpointHost, SaveOutcome};
    let fixture = Fixture::new(Mode::Absent);
    let missing_startup = CliEndpointHost {
        endpoint: Some("absent".to_owned()),
        ..(*fixture.host).clone()
    };
    let working = CliEndpointHost {
        endpoint: Some("api".to_owned()),
        ..(*fixture.host).clone()
    };
    let view = || missing_startup.catalog.as_ref().unwrap().view();
    let summary = |name: &str| {
        view()
            .endpoints
            .into_iter()
            .find(|summary| summary.name == name)
    };
    let ready = |name: &str| {
        assert!(
            view()
                .ready
                .iter()
                .any(|connection| connection.name == name)
        );
        assert_eq!(summary(name).map(|summary| summary.unavailable), Some(None));
    };
    let outcome = save(&working, "api").await;
    assert!(matches!(outcome, SaveOutcome::Saved(_)), "{outcome:?}");
    ready("api");
    let outcome = save(&missing_startup, "gw").await;
    assert!(matches!(outcome, SaveOutcome::Saved(_)), "{outcome:?}");
    ready("gw");
    assert_eq!(
        summary("gw").and_then(|summary| summary.credential),
        Some(wisp_protocol::settings::CredentialKind::AuthToken)
    );
    let edited = missing_startup
        .edit(
            "api".to_owned(),
            wisp_protocol::endpoint::EndpointEdit {
                key: Some(Secret::new("placeholder-next")),
                ..Default::default()
            },
            None,
            false,
        )
        .await
        .unwrap();
    assert!(matches!(edited, EditOutcome::Edited(_)), "{edited:?}");
    ready("api");
    let change = missing_startup
        .catalog
        .as_ref()
        .unwrap()
        .begin("api")
        .unwrap();
    std::fs::write(fixture.dir.root.join("config.toml"), "[").unwrap();
    assert!(missing_startup.reconnect().is_err());
    missing_startup
        .publish_catalog(
            change,
            "api",
            Some(wisp_core::endpoints::ProviderKind::Anthropic),
            Some(wisp_core::endpoints::AuthKind::Bearer),
            false,
        )
        .await;
    assert!(
        !view()
            .ready
            .iter()
            .any(|connection| connection.name == "api")
    );
    assert!(view().unavailable.iter().any(|(name, reason)| name == "api"
        && reason == "saved, not usable yet; retry loading this account"));
    assert_eq!(
        summary("api")
            .and_then(|summary| summary.unavailable)
            .as_deref(),
        Some("saved, not usable yet; retry loading this account")
    );
}

fn logout(request: u64, alias: &str) -> AuthCommand {
    AuthCommand::logout(
        RequestId(request),
        &[alias.to_owned()],
        false,
        None,
        Vec::new(),
    )
}

#[tokio::test]
async fn another_windows_changes_reach_this_one_only_through_the_watch() {
    use wisp_core::endpoints::generation::{Generation, Unavailable};
    use wisp_core::endpoints::host::{EditOutcome, EndpointHost, SaveOutcome};
    use wisp_core::endpoints::watch::AccountsWatch;
    let fixture = Fixture::new(Mode::Stored);
    let fixture = Fixture {
        host: Arc::new(CliEndpointHost {
            endpoint: Some("a".to_owned()),
            ..(*fixture.host).clone()
        }),
        ..fixture
    };
    let elsewhere = fixture.host.clone();
    let config = fixture.dir.root.join("config.toml");
    let mut without_c = Vec::new();
    for name in ["a", "b", "c"] {
        if name == "c" {
            without_c = std::fs::read(&config).unwrap();
        }
        let outcome = save(&elsewhere, name).await;
        assert!(matches!(outcome, SaveOutcome::Saved(_)), "{outcome:?}");
    }
    let source = Arc::new(elsewhere.paths());
    let first = source.look(Default::default()).await.unwrap();
    let catalog = LiveCatalog::new(wisp_core::kit::Connected {
        provider: first.view.ready[0].provider.clone(),
        ready: first.view.ready,
        unavailable: first.view.unavailable,
        active: "a".to_owned(),
        env_account: None,
    })
    .with_watch(source, first.prints);
    let logs = wisp_core::session::MemoryStore::default();
    let mut log = wisp_core::session::SessionLog::new(Box::new(logs.clone()), "chat");
    let here = CliEndpointHost {
        catalog: Some(catalog.clone()),
        ..(*elsewhere).clone()
    };
    let generation = |name: &str| -> Option<Generation> {
        catalog
            .view()
            .ready
            .into_iter()
            .find(|c| c.name == name)
            .and_then(|c| c.provider.generation())
    };
    let a = generation("a").unwrap();
    catalog.check().await;
    catalog.check().await;
    assert_eq!(catalog.revision(), 0);
    assert!(a.admit().is_ok());

    assert!(matches!(
        fixture.start(1, "plan", false).await,
        AuthOutcome::Challenge { .. }
    ));
    let outcome = fixture.finish(1).await;
    assert!(matches!(outcome, AuthOutcome::Ready { .. }), "{outcome:?}");
    catalog.check().await;
    assert_eq!(catalog.revision(), 1);
    let plan = generation("plan").unwrap();
    write_profile(
        &fixture.dir.root.join("config.toml"),
        "oauth:00000000000000000000000000000001",
    );
    catalog.check().await;
    assert_eq!(catalog.revision(), 1);
    assert!(plan.admit().is_ok());

    let store = wisp_core::secrets::credentials_path(&config);
    let stored = std::fs::read(&store).unwrap();
    std::fs::write(&store, "{ not json").unwrap();
    catalog.check().await;
    std::fs::write(&store, &stored).unwrap();
    let good = std::fs::read(&config).unwrap();
    std::fs::write(&config, "[endpoints.a\n").unwrap();
    catalog.check().await;
    let folder = wisp_core::config_file::lock_folder(&config);
    let held = wisp_core::secrets::lock(
        &[(&folder, wisp_core::secrets::LockKind::Config)],
        wisp_core::endpoints::PathShown::Default,
        std::time::Duration::from_secs(1),
    )
    .unwrap();
    std::fs::write(&config, &without_c).unwrap();
    catalog.check().await;
    std::fs::write(&config, &good).unwrap();
    drop(held);
    assert_eq!(catalog.revision(), 1);
    assert_eq!(catalog.view().ready.len(), 4);
    assert!(catalog.reader().take_removed().is_empty());

    let in_flight = a.admit().unwrap();
    assert!(matches!(
        elsewhere.remove("a".to_owned(), false).await.unwrap(),
        RemoveOutcome::Removed(_)
    ));
    assert!(matches!(
        here.remove("c".to_owned(), false).await.unwrap(),
        RemoveOutcome::Removed(_)
    ));
    let edited = elsewhere
        .edit(
            "b".to_owned(),
            wisp_protocol::endpoint::EndpointEdit {
                key: Some(Secret::new("placeholder-next")),
                ..Default::default()
            },
            None,
            false,
        )
        .await
        .unwrap();
    assert!(matches!(edited, EditOutcome::Edited(_)), "{edited:?}");
    let b = generation("b").unwrap();
    catalog.check().await;
    assert_eq!(a.admit().err(), Some(Unavailable::Closing));
    assert_eq!(catalog.reader().take_removed(), ["a"]);
    assert_eq!(b.admit().err(), Some(Unavailable::Retired));
    assert!(generation("b").unwrap().admit().is_ok());
    let names: Vec<String> = catalog.view().ready.into_iter().map(|c| c.name).collect();
    assert_eq!(names, ["b", "plan"]);
    drop(in_flight);
    assert_eq!(a.available(), Err(Unavailable::Retired));

    let same = wisp_protocol::endpoint::EndpointEdit {
        key: Some(Secret::new("placeholder-next")),
        ..Default::default()
    };
    let here = CliEndpointHost {
        endpoint: Some("b".to_owned()),
        ..here
    };
    let edited = here.edit("b".to_owned(), same, None, false).await.unwrap();
    assert!(matches!(edited, EditOutcome::Edited(_)), "{edited:?}");
    catalog.check().await;
    let b = generation("b").unwrap();
    assert!(matches!(
        elsewhere.remove("b".to_owned(), false).await.unwrap(),
        RemoveOutcome::Removed(_)
    ));
    catalog.check().await;
    assert_eq!(catalog.reader().take_removed(), ["b"]);
    assert!(b.admit().is_err());

    let mut clear = Vec::new();
    let mut note = |reason: String| {
        let event = wisp_protocol::log::LogEvent::Rejected {
            turn: None,
            reason,
            id: None,
            kind: None,
        };
        assert!(log.record(event).is_none());
        let records = wisp_core::session::SessionStore::read(&logs, "chat").unwrap();
        clear.push(
            serde_json::to_string(&records)
                .unwrap()
                .matches("CANARY")
                .count(),
        );
    };
    let spare =
        |catalog: &LiveCatalog| catalog.view().unavailable.iter().any(|(n, _)| n == "spare");
    let held = ["sk-test-CANARY-one x", "sk-test-CANARY-two xx"];
    let outcome = save(&elsewhere, "spare").await;
    assert!(matches!(outcome, SaveOutcome::Saved(_)), "{outcome:?}");
    let mut was = "placeholder";
    for key in held {
        let text = std::fs::read_to_string(&store).unwrap();
        let (from, to) = (format!("\"{was}\""), format!("\"{key}\""));
        assert!(text.contains(&from));
        std::fs::write(&store, text.replace(&from, &to)).unwrap();
        was = key;
        catalog.check().await;
        assert!(spare(&catalog));
        note(key.to_owned());
    }
    assert!(matches!(
        elsewhere.remove("spare".to_owned(), false).await.unwrap(),
        RemoveOutcome::Removed(_)
    ));
    catalog.check().await;
    assert!(!spare(&catalog));
    note(held.join(" "));
    let outcome = save(&elsewhere, "claude-api").await;
    assert!(matches!(outcome, SaveOutcome::Saved(_)), "{outcome:?}");
    let keys_file = elsewhere.env_file.clone().unwrap();
    let shadowed = ["sk-test-CANARY-five", "sk-test-CANARY-six-x"];
    for key in shadowed {
        std::fs::write(&keys_file, format!("ANTHROPIC_API_KEY={key}\n")).unwrap();
        catalog.check().await;
        let entries = elsewhere.paths().sources().entries().unwrap();
        assert!(wisp_core::endpoints::shadowed(&entries).is_some());
        note(key.to_owned());
    }
    std::fs::write(&keys_file, "").unwrap();
    catalog.check().await;
    note(shadowed.join(" "));
    let failing = wisp_core::endpoints::ResolvedEndpoint {
        name: wisp_core::endpoints::EndpointName::parse("failing").unwrap(),
        provider: wisp_core::endpoints::ProviderKind::Anthropic,
        credential: wisp_core::provider::credential::Credential::ApiKey(
            "sk-test-CANARY-four\u{7f}".to_owned(),
        ),
        base_url: wisp_core::provider::anthropic::DEFAULT_BASE_URL.to_owned(),
        source: wisp_core::provider::credential::Source::Keyring,
        key_env: None,
        url_source: wisp_core::endpoints::UrlSource::Official,
        pricing: wisp_core::endpoints::PricingMode::List,
    };
    let entries = std::collections::BTreeMap::from([(
        failing.name.clone(),
        wisp_core::endpoints::Availability::Ready(failing),
    )]);
    let (ready, refused) = crate::endpoints::connections(&entries, None);
    assert!(ready.is_empty() && refused.len() == 1);
    note("sk-test-CANARY-four\u{7f}".to_owned());
    assert_eq!(clear, [0, 0, 0, 0, 0, 0, 0]);
}

#[tokio::test]
async fn antigravity_challenge_kind_follows_browser_check_for_loopback_and_manual_paste() {
    for (can_open, expected_kind, pasted) in [
        (true, ChallengeKind::BrowserLoopback, None),
        (
            false,
            ChallengeKind::ManualCode,
            Some(Secret::new(
                "http://127.0.0.1:51121/oauth-callback?code=CODE_CANARY&state=STATE_CANARY",
            )),
        ),
    ] {
        let mut fixture = Fixture::new_for(Mode::Stored, SubscriptionProvider::Antigravity);
        let auth = super::super::SessionAuth::new(
            vec![fixture.fake.clone()],
            Arc::new(move |_: &str| can_open),
        )
        .with_browser_check(Arc::new(move || can_open));
        fixture.host = Arc::new(CliEndpointHost {
            auth,
            ..(*fixture.host).clone()
        });
        let started = session_auth(
            &fixture.host,
            AuthCommand::Start {
                request: RequestId(1),
                provider: SubscriptionProvider::Antigravity,
                alias: None,
                reauthorize: false,
            },
        )
        .await
        .outcome;
        let AuthOutcome::Challenge { challenge } = started else {
            panic!("{started:?}");
        };
        assert_eq!(challenge.provider, SubscriptionProvider::Antigravity);
        assert_eq!(challenge.alias, "antigravity");
        assert_eq!(challenge.kind, expected_kind);
        assert_eq!(challenge.opened, can_open);
        let finished = session_auth(
            &fixture.host,
            AuthCommand::Finish {
                request: RequestId(2),
                login: RequestId(1),
                response: pasted,
                alias: None,
            },
        )
        .await
        .outcome;
        assert!(
            matches!(
                finished,
                AuthOutcome::Ready {
                    endpoint: SubscriptionEndpoint {
                        provider: SubscriptionProvider::Antigravity,
                        state: LocalState::Working,
                        ..
                    },
                    ..
                }
            ),
            "{finished:?}"
        );
    }
}

#[test]
fn signing_out_of_a_keys_file_account_answers_the_same_with_its_provider_off() {
    let fixture = Fixture::new(Mode::Stored);
    let path = fixture.dir.root.join("config.toml");
    let text =
        "[endpoints.g1]\nprovider = \"gemini\"\nauth = \"api_key\"\nkey_env = \"GEMINI_API_KEY\"\n";
    std::fs::write(
        fixture.host.env_file.as_ref().unwrap(),
        "GEMINI_API_KEY=CANARY_GEMINI_KEYS_FILE\n",
    )
    .unwrap();
    let mut answers = Vec::new();
    for plugin in ["", "\n[plugins.gemini]\nenabled = false\n"] {
        std::fs::write(&path, text.to_owned() + plugin).unwrap();
        let Err(answer) = checked(&fixture.host, &["g1".to_owned()]) else {
            panic!("a keys-file key cannot be signed out");
        };
        answers.push(answer);
    }
    assert_eq!(answers[0], answers[1]);
    assert!(answers[0].1.contains("GEMINI_API_KEY"));
    assert!(!answers[0].1.contains("CANARY"));
}
