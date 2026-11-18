use std::sync::{Arc, Mutex};

use super::*;

const HANDLE: &str = "oauth:0123456789abcdef0123456789abcdef";
const CANARY: &str = "synthetic-url-state-code-token-account-canary";
const PRIVATE_CANARIES: [&str; 6] = [
    "https://example.invalid/authorize?state=synthetic-state-canary",
    "synthetic-state-canary",
    "synthetic-code-canary",
    "synthetic-access-token-canary",
    "synthetic-refresh-token-canary",
    "synthetic-account-canary",
];

#[derive(Default)]
struct FakeLogin {
    provider: Option<ProviderKind>,
    forgotten: Mutex<Vec<PathBuf>>,
    fail_cleanup: bool,
    revoked_before_cleanup_failure: bool,
    committed_before_cleanup: Option<Arc<Mutex<Vec<Secret>>>>,
    removed_before_logout: Option<(PathBuf, EndpointName)>,
}

impl SubscriptionLogin for FakeLogin {
    fn descriptor(&self) -> Descriptor {
        match self.provider {
            Some(ProviderKind::Anthropic) => OauthLogin(SubscriptionKind::Anthropic).descriptor(),
            Some(ProviderKind::Gemini) => OauthLogin(SubscriptionKind::Antigravity).descriptor(),
            _ => OauthLogin(SubscriptionKind::ChatGpt).descriptor(),
        }
    }

    fn start(
        &self,
        _allow_loopback: bool,
    ) -> BoxFuture<'_, Result<Box<dyn PendingSubscriptionLogin>, AuthError>> {
        Box::pin(async { Ok(Box::new(FakePending(None)) as Box<dyn PendingSubscriptionLogin>) })
    }

    fn forget<'a>(&'a self, profile: &'a Path) -> BoxFuture<'a, Result<(), AuthError>> {
        Box::pin(async move {
            if let Some(committed) = &self.committed_before_cleanup {
                assert!(!committed.lock().unwrap().is_empty());
            }
            self.forgotten.lock().unwrap().push(profile.to_owned());
            if self.fail_cleanup {
                Err(AuthError::Store)
            } else {
                Ok(())
            }
        })
    }

    fn status(&self, _: &Path) -> Result<AuthStatus, AuthError> {
        Err(AuthError::LoggedOut)
    }

    fn logout<'a>(&'a self, profile: &'a Path) -> BoxFuture<'a, Result<bool, AuthError>> {
        Box::pin(async move {
            if let Some((path, name)) = &self.removed_before_logout {
                assert!(!read_config(path).endpoints.contains_key(name));
            }
            if self.revoked_before_cleanup_failure {
                return Err(AuthError::LogoutCleanup {
                    remote_revoked: true,
                });
            }
            self.forget(profile).await?;
            oauth::forget(profile).await?;
            Ok(false)
        })
    }
}

struct FakePending(Option<AuthError>);

impl PendingSubscriptionLogin for FakePending {
    fn authorization_url(&self) -> &str {
        CANARY
    }

    fn finish(
        self: Box<Self>,
        _: Option<Secret>,
        _: PathBuf,
    ) -> BoxFuture<'static, Result<Box<dyn Approved>, AuthError>> {
        Box::pin(async move {
            match self.0 {
                Some(error) => Err(error),
                None => Ok(Box::new(FakeApproved) as Box<dyn Approved>),
            }
        })
    }
}

struct FakeApproved;

impl Approved for FakeApproved {
    fn held_by(&self, _: &Path, _: Option<&Path>) -> Vec<PathBuf> {
        vec![]
    }

    fn persist(self: Box<Self>, _: &Path, _: StoreKind) -> Result<Secret, AuthError> {
        Ok(Secret::new(HANDLE))
    }
}

struct FakeStorage {
    config: PathBuf,
    previous: PathBuf,
    committed: Arc<Mutex<Vec<Secret>>>,
    fail: bool,
}

impl FakeStorage {
    fn new(config: PathBuf, fail: bool) -> Self {
        Self {
            previous: config.with_file_name("old-profile"),
            config,
            committed: Arc::default(),
            fail,
        }
    }
}

impl LoginPersistence for FakeStorage {
    fn config_path(&self) -> &Path {
        &self.config
    }
    fn store(&self) -> StoreKind {
        StoreKind::File
    }
    fn previous_profile(&self) -> Option<&Path> {
        Some(&self.previous)
    }
    fn persist(&self, handle: Secret) -> Result<(), AuthError> {
        if self.fail {
            return Err(AuthError::Store);
        }
        self.committed.lock().unwrap().push(handle);
        Ok(())
    }
}

#[tokio::test]
async fn persistence_failure_cleans_only_new_profile_and_preserves_old_login() {
    let dir = tempfile::tempdir().unwrap();
    let storage = FakeStorage::new(dir.path().join("config.toml"), true);
    let login = FakeLogin::default();
    let result = complete(&login, login.start(true).await.unwrap(), None, &storage).await;
    assert!(matches!(result, Err(AuthError::Store)));
    assert!(storage.committed.lock().unwrap().is_empty());
    assert_eq!(
        *login.forgotten.lock().unwrap(),
        vec![oauth::profile_path(&storage.config, HANDLE).unwrap()]
    );
}

#[tokio::test]
async fn successful_reauthorization_commits_before_retiring_previous_profile() {
    let dir = tempfile::tempdir().unwrap();
    let storage = FakeStorage::new(dir.path().join("config.toml"), false);
    let login = FakeLogin {
        committed_before_cleanup: Some(storage.committed.clone()),
        ..FakeLogin::default()
    };
    let result = complete(
        &login,
        login.start(true).await.unwrap(),
        Some(Secret::new(PRIVATE_CANARIES.join(" "))),
        &storage,
    )
    .await
    .unwrap();
    assert!(!result.previous_cleanup_pending);
    assert_eq!(storage.committed.lock().unwrap().len(), 1);
    assert_eq!(
        *login.forgotten.lock().unwrap(),
        vec![storage.previous.clone()]
    );
    assert!(!format!("{result:?}").contains(CANARY));
    assert!(!format!("{result:?}").contains(HANDLE));
    for canary in PRIVATE_CANARIES {
        assert!(!format!("{result:?}").contains(canary));
    }
}

#[tokio::test]
async fn failed_old_profile_cleanup_reports_saved_login_without_rollback() {
    let dir = tempfile::tempdir().unwrap();
    let storage = FakeStorage::new(dir.path().join("config.toml"), false);
    let login = FakeLogin {
        fail_cleanup: true,
        ..FakeLogin::default()
    };
    let result = complete(&login, login.start(true).await.unwrap(), None, &storage)
        .await
        .unwrap();
    assert!(result.previous_cleanup_pending);
    assert_eq!(storage.committed.lock().unwrap().len(), 1);
    assert_eq!(
        *login.forgotten.lock().unwrap(),
        vec![storage.previous.clone()]
    );
}

#[tokio::test]
async fn failed_rollback_reports_uncertain_cleanup_without_losing_old_login() {
    let dir = tempfile::tempdir().unwrap();
    let storage = FakeStorage::new(dir.path().join("config.toml"), true);
    let login = FakeLogin {
        fail_cleanup: true,
        ..FakeLogin::default()
    };
    let result = complete(&login, login.start(true).await.unwrap(), None, &storage).await;
    assert!(matches!(result, Err(AuthError::CleanupUncertain)));
    assert!(storage.committed.lock().unwrap().is_empty());
    assert!(!login.forgotten.lock().unwrap().contains(&storage.previous));
}

#[tokio::test]
async fn oauth_failure_does_not_touch_persistence_or_previous_profile() {
    for error in [
        AuthError::State,
        AuthError::Pasted(wisp_protocol::CodeProblem::Repeated),
        AuthError::NoCode,
        AuthError::Expired,
        AuthError::Declined,
        AuthError::Callback,
    ] {
        let dir = tempfile::tempdir().unwrap();
        let storage = FakeStorage::new(dir.path().join("config.toml"), false);
        let login = FakeLogin::default();
        let result = complete(&login, Box::new(FakePending(Some(error))), None, &storage).await;
        assert!(result.is_err());
        assert!(storage.committed.lock().unwrap().is_empty());
        assert!(login.forgotten.lock().unwrap().is_empty());
        assert!(!format!("{result:?}").contains(CANARY));
    }
}

fn read_config(path: &Path) -> ConfigFile {
    ConfigFile::parse(
        path,
        PathShown::Given,
        &std::fs::read_to_string(path).unwrap(),
    )
    .unwrap()
}

#[test]
fn persistence_survives_restart_preserves_pricing_default_and_isolates_aliases() {
    use crate::secrets::keyring::{MemoryKeyring, TERMINAL_DEADLINE};
    for store in [StoreKind::File, StoreKind::Keyring] {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.toml");
        let memory = MemoryKeyring::new();
        let keys = memory.store(TERMINAL_DEADLINE);
        let first = EndpointName::parse("first").unwrap();
        let second = EndpointName::parse("second").unwrap();
        let destination = Destination {
            path: &path,
            shown: PathShown::Given,
            config: None,
            name: first.clone(),
            endpoint: NewEndpoint {
                pricing: PricingMode::None,
                ..subscription_endpoint(SubscriptionKind::ChatGpt, store)
            },
            make_default: true,
            previous: None,
            keychain: &keys,
        };
        destination.persist(Secret::new(HANDLE)).unwrap();
        let config = read_config(&path);
        Destination {
            config: Some(&config),
            name: second.clone(),
            previous: None,
            make_default: false,
            endpoint: subscription_endpoint(SubscriptionKind::ChatGpt, store),
            ..destination
        }
        .persist(Secret::new("oauth:11111111111111111111111111111111"))
        .unwrap();
        let config = read_config(&path);
        let destination = Destination {
            config: Some(&config),
            endpoint: subscription_endpoint(SubscriptionKind::ChatGpt, store),
            make_default: false,
            ..destination
        };
        destination
            .persist(Secret::new("oauth:abcdef0123456789abcdef0123456789"))
            .unwrap();
        let reopened = read_config(&path);
        assert_eq!(reopened.default, Some(first.clone()));
        assert_eq!(reopened.endpoints[&first].pricing, PricingMode::None);
        assert_eq!(reopened.endpoints[&second].pricing, PricingMode::List);
        assert_eq!(reopened.endpoints.len(), 2);
        assert_eq!(
            profile(&path, &reopened, &first, &keys).unwrap().0,
            oauth::profile_path(&path, "oauth:abcdef0123456789abcdef0123456789").unwrap()
        );
        assert_eq!(
            profile(&path, &reopened, &second, &keys).unwrap().0,
            oauth::profile_path(&path, "oauth:11111111111111111111111111111111").unwrap()
        );
    }
}

#[test]
fn conflicting_provider_is_refused_before_login_or_persistence() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("config.toml");
    let text = "[endpoints.plan]\nprovider = \"anthropic\"\nauth = \"api_key\"\nkey_env = \"SYNTHETIC_KEY\"\n";
    let config = ConfigFile::parse(&path, PathShown::Given, text).unwrap();
    let name = EndpointName::parse("plan").unwrap();
    assert!(matches!(
        validate_provider(Some(&config), &name, ProviderKind::OpenAi),
        Err(AuthError::Provider)
    ));
    assert!(
        validate_provider(
            Some(&config),
            &EndpointName::parse("other").unwrap(),
            ProviderKind::OpenAi
        )
        .is_ok()
    );
}

#[test]
fn endpoint_changed_during_browser_login_is_not_overwritten() {
    use crate::secrets::keyring::{MemoryKeyring, TERMINAL_DEADLINE};
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("config.toml");
    let memory = MemoryKeyring::new();
    let keys = memory.store(TERMINAL_DEADLINE);
    let destination = Destination {
        path: &path,
        shown: PathShown::Given,
        config: None,
        name: EndpointName::parse("plan").unwrap(),
        endpoint: subscription_endpoint(SubscriptionKind::ChatGpt, StoreKind::File),
        make_default: false,
        previous: None,
        keychain: &keys,
    };
    destination.persist(Secret::new(HANDLE)).unwrap();
    let snapshot = read_config(&path);
    secrets::apply(
        &path,
        PathShown::Given,
        Change::Edit {
            name: destination.name.clone(),
            edit: Edit {
                pricing: Some(PricingMode::None),
                ..Edit::default()
            },
        },
        &keys,
        secrets::LOCK_WAIT,
    )
    .unwrap();
    let destination = Destination {
        config: Some(&snapshot),
        ..destination
    };
    let before = std::fs::read(&path).unwrap();
    assert!(matches!(
        destination.persist(Secret::new(CANARY)),
        Err(AuthError::EndpointChanged)
    ));
    assert_eq!(std::fs::read(&path).unwrap(), before);
}

#[test]
fn unavailable_keychain_leaves_configuration_untouched() {
    use crate::secrets::keyring::{KeyringError, Unreachable};
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("config.toml");
    let keys = Unreachable(KeyringError::NotRunning);
    let destination = Destination {
        path: &path,
        shown: PathShown::Given,
        config: None,
        name: EndpointName::parse("plan").unwrap(),
        endpoint: subscription_endpoint(SubscriptionKind::ChatGpt, StoreKind::Keyring),
        make_default: false,
        previous: None,
        keychain: &keys,
    };
    assert!(destination.persist(Secret::new(HANDLE)).is_err());
    assert!(!path.exists());
}

fn obstruct_config(path: &Path, saved: &Path) {
    std::fs::rename(path, saved).unwrap();
    std::fs::create_dir(path).unwrap();
    std::fs::write(path.join("blocker"), "synthetic write obstruction").unwrap();
}

fn restore_config(path: &Path, saved: &Path) {
    assert_eq!(
        std::fs::read_to_string(path.join("blocker")).unwrap(),
        "synthetic write obstruction"
    );
    std::fs::remove_file(path.join("blocker")).unwrap();
    std::fs::remove_dir(path).unwrap();
    std::fs::rename(saved, path).unwrap();
}

#[test]
fn obstructed_config_target_does_not_replace_stored_login() {
    use crate::secrets::keyring::{MemoryKeyring, TERMINAL_DEADLINE};
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("config.toml");
    let memory = MemoryKeyring::new();
    let keys = memory.store(TERMINAL_DEADLINE);
    let destination = Destination {
        path: &path,
        shown: PathShown::Given,
        config: None,
        name: EndpointName::parse("plan").unwrap(),
        endpoint: subscription_endpoint(SubscriptionKind::ChatGpt, StoreKind::File),
        make_default: false,
        previous: None,
        keychain: &keys,
    };
    destination.persist(Secret::new(HANDLE)).unwrap();
    let config = read_config(&path);
    let before = std::fs::read(&path).unwrap();
    let saved = dir.path().join("saved-config.toml");
    let credentials_before = std::fs::read(secrets::credentials_path(&path)).unwrap();
    obstruct_config(&path, &saved);
    let destination = Destination {
        config: Some(&config),
        ..destination
    };
    let result = destination.persist(Secret::new(CANARY));
    restore_config(&path, &saved);
    assert_eq!(
        std::fs::read(secrets::credentials_path(&path)).unwrap(),
        credentials_before
    );
    assert!(matches!(result, Err(AuthError::Save(_))));
    assert_eq!(std::fs::read(&path).unwrap(), before);
    assert!(!format!("{result:?}").contains(CANARY));
}

struct LogoutFixture {
    folder: tempfile::TempDir,
    path: PathBuf,
    name: EndpointName,
    profile: PathBuf,
    keys: crate::secrets::keyring::KeyringStore,
}

impl LogoutFixture {
    fn new() -> Self {
        Self::with_store(StoreKind::File)
    }

    fn claude() -> Self {
        Self::with_endpoint(subscription_endpoint(
            SubscriptionKind::Anthropic,
            StoreKind::File,
        ))
    }

    fn with_store(store: StoreKind) -> Self {
        Self::with_endpoint(subscription_endpoint(SubscriptionKind::ChatGpt, store))
    }

    fn with_endpoint(endpoint: NewEndpoint) -> Self {
        use crate::secrets::keyring::{MemoryKeyring, TERMINAL_DEADLINE};
        let folder = tempfile::tempdir().unwrap();
        let path = folder.path().join("config.toml");
        let name = EndpointName::parse("plan").unwrap();
        let memory = MemoryKeyring::new();
        let keys = memory.store(TERMINAL_DEADLINE);
        Destination {
            path: &path,
            shown: PathShown::Given,
            config: None,
            name: name.clone(),
            endpoint,
            make_default: false,
            previous: None,
            keychain: &keys,
        }
        .persist(Secret::new(HANDLE))
        .unwrap();
        let profile = oauth::profile_path(&path, HANDLE).unwrap();
        std::fs::create_dir_all(profile.parent().unwrap()).unwrap();
        std::fs::write(&profile, "synthetic-profile").unwrap();
        Self {
            folder,
            path,
            name,
            profile,
            keys,
        }
    }

    fn login(&self) -> FakeLogin {
        FakeLogin {
            provider: Some(read_config(&self.path).endpoints[&self.name].provider),
            removed_before_logout: Some((self.path.clone(), self.name.clone())),
            ..FakeLogin::default()
        }
    }
}

#[tokio::test]
async fn logout_removes_config_then_credentials_and_relogin_is_fresh() {
    for fixture in [LogoutFixture::new(), LogoutFixture::claude()] {
        let config = read_config(&fixture.path);
        let login = fixture.login();
        let hint = fixture.profile.with_extension("account");
        std::fs::write(&hint, "a".repeat(64)).unwrap();
        let mark = fixture.profile.with_extension("refused");
        std::fs::write(&mark, "").unwrap();
        let outcome = logout_endpoint(
            &login,
            &fixture.path,
            &config,
            &fixture.name,
            None,
            &fixture.keys,
        )
        .await
        .unwrap();
        assert!(!outcome.cleanup_pending);
        assert!(!outcome.remote_revoked);
        assert!(!fixture.profile.exists());
        assert!(!hint.exists());
        let stem = fixture.profile.file_stem().unwrap().to_owned();
        let left = std::fs::read_dir(fixture.profile.parent().unwrap())
            .unwrap()
            .filter(|entry| entry.as_ref().unwrap().path().file_stem() == Some(&stem))
            .count();
        assert_eq!(left, 0);
        let config = read_config(&fixture.path);
        assert!(!config.endpoints.contains_key(&fixture.name));
        let records = secrets::FileStore::beside(&fixture.path, PathShown::Given)
            .read_all(config.store_id.as_ref().unwrap())
            .unwrap();
        assert!(!records.contains_key(&fixture.name));
        let destination = Destination {
            path: &fixture.path,
            shown: PathShown::Given,
            config: Some(&config),
            name: fixture.name.clone(),
            endpoint: subscription_endpoint(login.descriptor().kind, StoreKind::File),
            make_default: false,
            previous: None,
            keychain: &fixture.keys,
        };
        let outcome = complete(&login, login.start(true).await.unwrap(), None, &destination)
            .await
            .unwrap();
        assert!(!outcome.previous_cleanup_pending);
        assert!(
            read_config(&fixture.path)
                .endpoints
                .contains_key(&fixture.name)
        );
        assert_eq!(login.forgotten.lock().unwrap().len(), 1);
    }
}

#[test]
fn same_account_names_the_first_other_holder_and_reads_nothing_without_a_match() {
    use crate::secrets::keyring::{KeyringError, Unreachable};
    use crate::secrets::{KeyringAccess, StoreId, Stored};
    use std::sync::atomic::{AtomicUsize, Ordering};
    struct Counting<'a>(&'a dyn KeyringAccess, AtomicUsize);
    impl KeyringAccess for Counting<'_> {
        fn read(&self, id: &StoreId, name: &EndpointName) -> Result<Option<Stored>, KeyringError> {
            self.1.fetch_add(1, Ordering::SeqCst);
            self.0.read(id, name)
        }
        fn write(
            &self,
            id: &StoreId,
            name: &EndpointName,
            stored: &Stored,
        ) -> Result<(), KeyringError> {
            self.0.write(id, name, stored)
        }
        fn delete(&self, id: &StoreId, name: &EndpointName) -> Result<bool, KeyringError> {
            self.0.delete(id, name)
        }
    }
    let fixture = LogoutFixture::with_store(StoreKind::Keyring);
    let handles = [
        ("zeta", "oauth:00000000000000000000000000000003"),
        ("beta", "oauth:00000000000000000000000000000002"),
    ];
    for (name, handle) in handles {
        let config = read_config(&fixture.path);
        Destination {
            path: &fixture.path,
            shown: PathShown::Given,
            config: Some(&config),
            name: EndpointName::parse(name).unwrap(),
            endpoint: subscription_endpoint(SubscriptionKind::ChatGpt, StoreKind::Keyring),
            make_default: false,
            previous: None,
            keychain: &fixture.keys,
        }
        .persist(Secret::new(handle))
        .unwrap();
    }
    let config = read_config(&fixture.path);
    let at = |handle: &str| oauth::profile_path(&fixture.path, handle).unwrap();
    let auth = AuthKind::ChatGptSubscription;
    let new = EndpointName::parse("new").unwrap();
    let all = [at(handles[0].1), at(handles[1].1), at(HANDLE)];
    let counting = Counting(&fixture.keys, AtomicUsize::new(0));
    assert_eq!(
        same_account(&fixture.path, &config, &new, auth, &[], &counting),
        None
    );
    assert_eq!(counting.1.load(Ordering::SeqCst), 0);
    let found = |name: &EndpointName, matches: &[PathBuf]| {
        same_account(&fixture.path, &config, name, auth, matches, &fixture.keys)
            .map(|name| name.to_string())
    };
    assert_eq!(found(&new, &all).as_deref(), Some("beta"));
    assert_eq!(
        found(&EndpointName::parse("beta").unwrap(), &all).as_deref(),
        Some("plan")
    );
    assert_eq!(found(&new, &all[..1]).as_deref(), Some("zeta"));
    assert_eq!(
        same_account(
            &fixture.path,
            &config,
            &new,
            AuthKind::AnthropicSubscription,
            &all,
            &fixture.keys
        ),
        None
    );
    let unreachable = Unreachable(KeyringError::Locked);
    assert_eq!(
        same_account(&fixture.path, &config, &new, auth, &all, &unreachable),
        None
    );
}

fn add_subscription(fixture: &LogoutFixture, name: &str, kind: SubscriptionKind) {
    let config = read_config(&fixture.path);
    Destination {
        path: &fixture.path,
        shown: PathShown::Given,
        config: Some(&config),
        name: EndpointName::parse(name).unwrap(),
        endpoint: subscription_endpoint(kind, StoreKind::File),
        make_default: false,
        previous: None,
        keychain: &fixture.keys,
    }
    .persist(Secret::new(HANDLE))
    .unwrap();
}

#[tokio::test]
async fn signing_out_of_the_start_account_moves_the_start_and_two_windows_leave_a_valid_one() {
    let work = EndpointName::parse("claude-work").unwrap();
    let chatgpt = EndpointName::parse("chatgpt").unwrap();
    for (chat, start) in [(None, &work), (Some(&chatgpt), &chatgpt)] {
        let fixture = LogoutFixture::claude();
        add_subscription(&fixture, "chatgpt", SubscriptionKind::ChatGpt);
        add_subscription(&fixture, "claude-work", SubscriptionKind::Anthropic);
        secrets::apply(
            &fixture.path,
            PathShown::Given,
            Change::SetDefault {
                name: fixture.name.clone(),
            },
            &fixture.keys,
            secrets::LOCK_WAIT,
        )
        .unwrap();
        let config = read_config(&fixture.path);
        let login = fixture.login();
        let chat = chat.cloned();
        let outcome = logout_endpoint(
            &login,
            &fixture.path,
            &config,
            &fixture.name,
            chat,
            &fixture.keys,
        )
        .await
        .unwrap();
        assert_eq!(outcome.new_start.as_ref(), Some(start));
        let config = read_config(&fixture.path);
        assert_eq!(config.default.as_ref(), Some(start));
        assert!(!config.endpoints.contains_key(&fixture.name));
    }
    let fixture = LogoutFixture::claude();
    add_subscription(&fixture, "claude-work", SubscriptionKind::Anthropic);
    secrets::apply(
        &fixture.path,
        PathShown::Given,
        Change::SetDefault {
            name: fixture.name.clone(),
        },
        &fixture.keys,
        secrets::LOCK_WAIT,
    )
    .unwrap();
    let config = read_config(&fixture.path);
    secrets::apply(
        &fixture.path,
        PathShown::Given,
        Change::Remove {
            name: work.clone(),
            keep_key: false,
            chat: None,
        },
        &fixture.keys,
        secrets::LOCK_WAIT,
    )
    .unwrap();
    let outcome = logout_endpoint(
        &fixture.login(),
        &fixture.path,
        &config,
        &fixture.name,
        Some(work),
        &fixture.keys,
    )
    .await
    .unwrap();
    assert_eq!(outcome.new_start, None);
    assert_eq!(read_config(&fixture.path).default, None);
}

#[tokio::test]
async fn a_preset_naming_the_account_never_blocks_its_sign_out() {
    for fixture in [LogoutFixture::new(), LogoutFixture::claude()] {
        let text = std::fs::read_to_string(&fixture.path).unwrap();
        std::fs::write(
            &fixture.path,
            format!(
                "{text}\n[agents.presets.worker]\ndescription = \"test worker\"\nendpoint = \"plan\"\n"
            ),
        )
        .unwrap();
        let config = read_config(&fixture.path);
        let login = fixture.login();
        logout_endpoint(
            &login,
            &fixture.path,
            &config,
            &fixture.name,
            None,
            &fixture.keys,
        )
        .await
        .unwrap();
        assert!(
            !read_config(&fixture.path)
                .endpoints
                .contains_key(&fixture.name)
        );
        assert!(!fixture.profile.exists());
    }
}

#[tokio::test]
async fn logout_profile_failure_reports_partial_cleanup_without_restoring_endpoint() {
    for fixture in [LogoutFixture::new(), LogoutFixture::claude()] {
        let config = read_config(&fixture.path);
        let login = FakeLogin {
            fail_cleanup: true,
            ..fixture.login()
        };
        let outcome = logout_endpoint(
            &login,
            &fixture.path,
            &config,
            &fixture.name,
            None,
            &fixture.keys,
        )
        .await
        .unwrap();
        assert!(outcome.cleanup_pending);
        assert!(!outcome.remote_revoked);
        assert!(
            !read_config(&fixture.path)
                .endpoints
                .contains_key(&fixture.name)
        );
        assert!(fixture.profile.exists());
    }
}

#[tokio::test]
async fn logout_config_write_failure_preserves_endpoint_and_profile() {
    for fixture in [LogoutFixture::new(), LogoutFixture::claude()] {
        let config = read_config(&fixture.path);
        let before = std::fs::read(&fixture.path).unwrap();
        let login = fixture.login();
        let saved = fixture.folder.path().join("saved-config.toml");
        let credentials_before = std::fs::read(secrets::credentials_path(&fixture.path)).unwrap();
        obstruct_config(&fixture.path, &saved);
        let result = logout_endpoint(
            &login,
            &fixture.path,
            &config,
            &fixture.name,
            None,
            &fixture.keys,
        )
        .await;
        restore_config(&fixture.path, &saved);
        assert_eq!(
            std::fs::read(secrets::credentials_path(&fixture.path)).unwrap(),
            credentials_before
        );
        assert!(result.is_err());
        assert_eq!(std::fs::read(&fixture.path).unwrap(), before);
        assert!(fixture.profile.exists());
        assert!(login.forgotten.lock().unwrap().is_empty());
        assert_eq!(
            profile(
                &fixture.path,
                &read_config(&fixture.path),
                &fixture.name,
                &fixture.keys
            )
            .unwrap()
            .0,
            fixture.profile
        );
    }
}

#[tokio::test]
async fn concurrent_rename_refuses_logout_without_forgetting_shared_profile() {
    use crate::secrets::{KeyringAccess, KeyringError, StoreId, Stored};
    use std::sync::atomic::{AtomicBool, Ordering};
    struct RenameOnRead<'a> {
        fixture: &'a LogoutFixture,
        renamed: EndpointName,
        fired: AtomicBool,
    }
    impl KeyringAccess for RenameOnRead<'_> {
        fn read(&self, id: &StoreId, name: &EndpointName) -> Result<Option<Stored>, KeyringError> {
            let found = KeyringAccess::read(&self.fixture.keys, id, name)?;
            if name == &self.fixture.name && !self.fired.swap(true, Ordering::SeqCst) {
                secrets::apply(
                    &self.fixture.path,
                    PathShown::Given,
                    Change::Edit {
                        name: name.clone(),
                        edit: Edit {
                            rename: Some(self.renamed.clone()),
                            ..Edit::default()
                        },
                    },
                    &self.fixture.keys,
                    secrets::LOCK_WAIT,
                )
                .unwrap();
            }
            Ok(found)
        }
        fn write(
            &self,
            id: &StoreId,
            name: &EndpointName,
            stored: &Stored,
        ) -> Result<(), KeyringError> {
            KeyringAccess::write(&self.fixture.keys, id, name, stored)
        }
        fn delete(&self, id: &StoreId, name: &EndpointName) -> Result<bool, KeyringError> {
            KeyringAccess::delete(&self.fixture.keys, id, name)
        }
    }
    let fixture = LogoutFixture::with_store(StoreKind::Keyring);
    let snapshot = read_config(&fixture.path);
    let renamed = EndpointName::parse("renamed").unwrap();
    let keychain = RenameOnRead {
        fixture: &fixture,
        renamed: renamed.clone(),
        fired: AtomicBool::new(false),
    };
    let login = FakeLogin::default();
    let result = logout_endpoint(
        &login,
        &fixture.path,
        &snapshot,
        &fixture.name,
        None,
        &keychain,
    )
    .await;
    assert_eq!(
        result.unwrap_err().to_string(),
        "the account changed during logout; run logout again"
    );
    assert!(keychain.fired.load(Ordering::SeqCst));
    assert!(fixture.profile.exists());
    assert!(login.forgotten.lock().unwrap().is_empty());
    assert_eq!(
        profile(
            &fixture.path,
            &read_config(&fixture.path),
            &renamed,
            &fixture.keys
        )
        .unwrap()
        .0,
        fixture.profile
    );
}

#[tokio::test]
async fn local_cleanup_failure_preserves_confirmed_remote_revocation() {
    let fixture = LogoutFixture::new();
    let login = FakeLogin {
        revoked_before_cleanup_failure: true,
        ..fixture.login()
    };
    let result = logout_endpoint(
        &login,
        &fixture.path,
        &read_config(&fixture.path),
        &fixture.name,
        None,
        &fixture.keys,
    )
    .await
    .unwrap();
    assert_eq!(
        result,
        LoggedOut {
            remote_revoked: true,
            cleanup_pending: true,
            new_start: None,
        }
    );
    assert!(fixture.profile.exists());
    assert!(
        !read_config(&fixture.path)
            .endpoints
            .contains_key(&fixture.name)
    );
}

#[tokio::test]
async fn claude_login_reuses_the_cli_flow_client_scope_and_state_validation() {
    let legacy = PendingLogin::start(SubscriptionKind::Anthropic)
        .await
        .unwrap();
    let pending = OauthLogin(SubscriptionKind::Anthropic)
        .start(true)
        .await
        .unwrap();
    let legacy_url = reqwest::Url::parse(legacy.authorization_url()).unwrap();
    let url = reqwest::Url::parse(pending.authorization_url()).unwrap();
    assert_eq!(url.origin(), legacy_url.origin());
    assert_eq!(url.path(), legacy_url.path());
    for key in [
        "client_id",
        "scope",
        "redirect_uri",
        "response_type",
        "code_challenge_method",
        "code",
    ] {
        assert_eq!(
            url.query_pairs().find(|(name, _)| name == key),
            legacy_url.query_pairs().find(|(name, _)| name == key)
        );
    }
    assert_eq!(
        url.query_pairs()
            .find(|(name, _)| name == "code")
            .map(|(_, value)| value.into_owned())
            .as_deref(),
        Some("true")
    );
    let folder = tempfile::tempdir().unwrap();
    let result = pending
        .finish(
            Some(Secret::new("synthetic-code#wrong-state")),
            folder.path().join("config.toml"),
        )
        .await;
    assert!(matches!(result, Err(AuthError::State)));
    assert_eq!(
        OauthLogin(SubscriptionKind::Anthropic)
            .descriptor()
            .default_alias,
        "claude"
    );
    assert_eq!(
        OauthLogin(SubscriptionKind::ChatGpt)
            .descriptor()
            .default_alias,
        "chatgpt"
    );
}

#[tokio::test]
async fn claude_static_setup_token_logout_deletes_config_and_key_without_oauth() {
    let fixture = LogoutFixture::claude();
    let config = read_config(&fixture.path);
    let destination = Destination {
        path: &fixture.path,
        shown: PathShown::Given,
        config: Some(&config),
        name: fixture.name.clone(),
        endpoint: subscription_endpoint(SubscriptionKind::Anthropic, StoreKind::File),
        make_default: false,
        previous: None,
        keychain: &fixture.keys,
    };
    destination
        .persist(Secret::new("synthetic-setup-token"))
        .unwrap();
    let config = read_config(&fixture.path);
    let outcome = logout_endpoint(
        &OauthLogin(SubscriptionKind::Anthropic),
        &fixture.path,
        &config,
        &fixture.name,
        None,
        &fixture.keys,
    )
    .await
    .unwrap();
    assert!(!outcome.remote_revoked && !outcome.cleanup_pending);
    let config = read_config(&fixture.path);
    assert!(!config.endpoints.contains_key(&fixture.name));
    assert!(
        !secrets::FileStore::beside(&fixture.path, PathShown::Given)
            .read_all(config.store_id.as_ref().unwrap())
            .unwrap()
            .contains_key(&fixture.name)
    );
}
