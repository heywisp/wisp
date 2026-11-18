use std::io::{Read, Write};
use std::net::TcpListener;
use std::time::Duration;

use wisp_core::secrets::keyring::{
    BackendError, FakeClock, MemoryKeyring, MemoryMode, TERMINAL_DEADLINE,
};
use wisp_core::secrets::{LOCK_WAIT, LockKind};
use wisp_protocol::endpoint::{StoreChoice, StoreKind as Kept};

use super::*;
use crate::turn_lines::with_turn_lines;

const KEY: &str = "sk-ant-CANARYHOSTKEYa1b2";

struct Place {
    dir: crate::scratch::ScratchDir,
}

impl Place {
    fn new() -> Self {
        Self {
            dir: crate::scratch_dir("manage-host"),
        }
    }

    fn config(&self) -> PathBuf {
        self.dir.root.join("config.toml")
    }

    fn read(&self, name: &str) -> String {
        std::fs::read_to_string(self.dir.root.join(name)).unwrap_or_default()
    }

    fn host(&self, keychain: Keychain) -> CliEndpointHost {
        CliEndpointHost {
            catalog: None,
            auth: crate::auth::SessionAuth::default(),
            env_file: None,
            config: Some(self.config()),
            endpoint: None,
            model: "claude-sonnet-5-5".into(),
            xdg_config_home: None,
            home: None,
            keychain: Arc::new(keychain),
            budget: Budget::Short,
            set: Vec::new(),
            models: None,
        }
    }
}

fn answering(status: u16, body: &'static str) -> String {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    std::thread::spawn(move || {
        for stream in listener.incoming() {
            let Ok(mut stream) = stream else { return };
            let mut buf = [0u8; 8192];
            let _ = stream.read(&mut buf);
            let reply = format!(
                "HTTP/1.1 {status} X\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{body}",
                body.len()
            );
            let _ = stream.write_all(reply.as_bytes());
        }
    });
    format!("http://127.0.0.1:{port}")
}

fn spec(name: &str, url: Option<String>, store: StoreChoice) -> EndpointSpec {
    EndpointSpec {
        name: name.into(),
        provider: "anthropic".into(),
        auth: "bearer".into(),
        url: url.map(Secret::new),
        store,
        default: true,
    }
}

fn idle() -> watch::Receiver<bool> {
    watch::channel(false).1
}

fn no_canary(text: &str) {
    assert!(!text.contains("CANARY"), "{text}");
}

fn memory_keychain(memory: &MemoryKeyring) -> Keychain {
    keychain_on(memory.store(Duration::from_secs(30)))
}

fn keychain_on(store: wisp_core::secrets::keyring::KeyringStore) -> Keychain {
    use wisp_core::secrets::detect::{Backend, Detected, KeychainChoice, KeychainSetting};
    Keychain::assemble(
        KeychainChoice {
            setting: KeychainSetting::On,
            origin: Some("WISP_KEYCHAIN".to_owned()),
        },
        Detected::Available {
            backend: Backend::SecretService,
            down: None,
        },
        false,
        Some(store),
    )
}

#[tokio::test]
async fn a_checked_save_writes_the_file_and_notes_where_the_key_went() {
    let place = Place::new();
    let host = place.host(Keychain::off());
    let url = answering(200, r#"{"input_tokens":3}"#);
    let outcome = host
        .save(
            spec("gw", Some(url), StoreChoice::Auto),
            Secret::new(KEY),
            true,
            idle(),
        )
        .await
        .unwrap();
    let SaveOutcome::Saved(saved) = &outcome else {
        panic!("{outcome:?}");
    };
    assert_eq!(saved.name, "gw");
    assert_eq!(saved.store, Kept::File);
    assert_eq!(saved.check, CheckResult::of(CheckKind::Ok));
    assert!(
        saved
            .notes
            .iter()
            .any(|n| n == "Created the file given with --config."),
        "{:?}",
        saved.notes
    );
    no_canary(&format!("{outcome:?}"));
    let config = place.read("config.toml");
    assert!(config.contains("[endpoints.gw]"), "{config}");
    assert!(config.contains("default = \"gw\""), "{config}");
    no_canary(&config);
    assert!(place.read("credentials.json").contains(KEY));
    let reconnected = host.connections().await.unwrap();
    let names: Vec<&str> = reconnected.ready.iter().map(|c| c.name.as_str()).collect();
    assert_eq!(names, ["gw"]);
    assert_eq!(reconnected.endpoints.len(), 1);
    assert!(reconnected.endpoints[0].default);
}

#[tokio::test]
async fn each_check_outcome_is_answered_before_anything_is_written() {
    for (status, kind) in [
        (401, CheckKind::KeyRefused),
        (403, CheckKind::Forbidden),
        (404, CheckKind::NotOffered),
        (500, CheckKind::Unexpected),
    ] {
        let place = Place::new();
        let host = place.host(Keychain::off());
        let url = answering(status, "{}");
        let outcome = host
            .save(
                spec("gw", Some(url), StoreChoice::Auto),
                Secret::new(KEY),
                true,
                idle(),
            )
            .await
            .unwrap();
        let SaveOutcome::NotSaved(NotSaved {
            name,
            why: NotSavedReason::Check(check),
        }) = &outcome
        else {
            panic!("{outcome:?}");
        };
        assert_eq!(name, "gw");
        assert_eq!(check.outcome, kind, "{status}");
        assert_eq!(check.reason.is_some(), kind == CheckKind::Unexpected);
        assert!(!place.config().exists());
        assert!(place.read("credentials.json").is_empty());
    }
    let place = Place::new();
    let host = place.host(Keychain::off());
    let held = tokio::net::TcpSocket::new_v4().unwrap();
    held.bind("127.0.0.1:0".parse().unwrap()).unwrap();
    let closed = format!("http://127.0.0.1:{}", held.local_addr().unwrap().port());
    let outcome = host
        .save(
            spec("gw", Some(closed.clone()), StoreChoice::Auto),
            Secret::new(KEY),
            true,
            idle(),
        )
        .await
        .unwrap();
    assert!(matches!(
        &outcome,
        SaveOutcome::NotSaved(NotSaved {
            why: NotSavedReason::Check(CheckResult {
                outcome: CheckKind::Unreachable,
                reason: Some(_),
            }),
            ..
        })
    ));
    let anyway = host
        .save(
            spec("gw", Some(closed), StoreChoice::Auto),
            Secret::new(KEY),
            false,
            idle(),
        )
        .await
        .unwrap();
    let SaveOutcome::Saved(saved) = anyway else {
        panic!("{anyway:?}");
    };
    assert_eq!(saved.check, CheckResult::skipped());
}

#[tokio::test]
async fn a_keychain_that_is_down_asks_about_the_file_and_the_file_saves() {
    let place = Place::new();
    let memory = MemoryKeyring::new();
    memory.set_mode(MemoryMode::Unopenable);
    let host = place.host(memory_keychain(&memory));
    let outcome = host
        .save(
            spec(
                "gw",
                Some("https://gw.example.com".into()),
                StoreChoice::Auto,
            ),
            Secret::new(KEY),
            false,
            idle(),
        )
        .await
        .unwrap();
    assert_eq!(
        outcome,
        SaveOutcome::NotSaved(NotSaved {
            name: "gw".into(),
            why: NotSavedReason::KeychainFailed("isn't running".into()),
        })
    );
    assert!(place.read("credentials.json").is_empty());
    assert!(!place.read("config.toml").contains("[endpoints.gw]"));
    let outcome = host
        .save(
            spec(
                "gw",
                Some("https://gw.example.com".into()),
                StoreChoice::File,
            ),
            Secret::new(KEY),
            false,
            idle(),
        )
        .await
        .unwrap();
    let SaveOutcome::Saved(saved) = outcome else {
        panic!("{outcome:?}");
    };
    assert_eq!(saved.store, Kept::File);
    assert!(place.read("config.toml").contains("key_store = \"file\""));
    assert!(place.read("credentials.json").contains(KEY));
}

#[tokio::test]
async fn an_unconfirmed_keychain_write_is_refused_and_never_offers_the_file() {
    let place = Place::new();
    let memory = MemoryKeyring::new();
    memory.set_mode(MemoryMode::ReadsFail(BackendError::Invalid));
    let host = place.host(memory_keychain(&memory));
    let refused = host
        .save(
            spec("gw", None, StoreChoice::Auto),
            Secret::new(KEY),
            false,
            idle(),
        )
        .await
        .unwrap_err();
    assert!(
        refused.0.contains("may have been saved there"),
        "{}",
        refused.0
    );
    no_canary(&refused.0);
    assert!(place.read("credentials.json").is_empty());
    assert!(!place.read("config.toml").contains("[endpoints.gw]"));
}

#[tokio::test]
async fn a_working_keychain_keeps_the_key_and_the_reconnect_reads_it_back() {
    let place = Place::new();
    let memory = MemoryKeyring::new();
    let host = place.host(memory_keychain(&memory));
    let outcome = host
        .save(
            spec(
                "gw",
                Some("https://gw.example.com/CANARYPATH".into()),
                StoreChoice::Auto,
            ),
            Secret::new(KEY),
            false,
            idle(),
        )
        .await
        .unwrap();
    let SaveOutcome::Saved(saved) = outcome else {
        panic!("{outcome:?}");
    };
    assert_eq!(saved.store, Kept::Keyring);
    assert!(place.read("credentials.json").is_empty());
    assert_eq!(memory.items().len(), 1);
    let reconnected = host.connections().await.unwrap();
    assert_eq!(reconnected.ready.len(), 1);
    no_canary(&format!("{:?}", reconnected.endpoints));
}

#[tokio::test]
async fn invalid_requests_are_refused_without_echoing_them() {
    let place = Place::new();
    std::fs::write(
        place.config(),
        "[endpoints.gw]\nauth = \"bearer\"\nkey_env = \"GW_KEY\"\n",
    )
    .unwrap();
    let before = place.read("config.toml");
    let host = place.host(Keychain::off());
    let taken = host
        .save(
            spec("gw", None, StoreChoice::File),
            Secret::new(KEY),
            false,
            idle(),
        )
        .await
        .unwrap_err();
    assert!(taken.0.contains("already exists"), "{}", taken.0);
    let keyring = host
        .save(
            spec("other", None, StoreChoice::Keyring),
            Secret::new(KEY),
            false,
            idle(),
        )
        .await
        .unwrap_err();
    assert!(keyring.0.contains("the keychain is off"), "{}", keyring.0);
    let bad = host
        .save(
            spec("CANARY", None, StoreChoice::File),
            Secret::new(KEY),
            false,
            idle(),
        )
        .await
        .unwrap_err();
    no_canary(&bad.0);
    assert_eq!(place.read("config.toml"), before);
}

#[tokio::test]
async fn a_cancel_before_the_locks_writes_nothing() {
    let place = Place::new();
    let host = place.host(Keychain::off());
    let (stop, cancel) = watch::channel(false);
    stop.send_replace(true);
    let outcome = host
        .save(
            spec("gw", None, StoreChoice::File),
            Secret::new(KEY),
            false,
            cancel,
        )
        .await
        .unwrap();
    assert_eq!(outcome, SaveOutcome::Cancelled);
    assert!(!place.config().exists());
    assert!(!place.dir.root.join(".wisp.lock").exists());
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_save_waits_for_a_held_lock_and_then_writes() {
    let place = Place::new();
    let host = place.host(Keychain::off());
    let held = wisp_core::secrets::lock(
        &[(place.dir.root.as_path(), LockKind::Config)],
        PathShown::Given,
        Duration::ZERO,
    )
    .unwrap();
    let saving = tokio::spawn(async move {
        host.save(
            spec("gw", None, StoreChoice::File),
            Secret::new(KEY),
            false,
            idle(),
        )
        .await
    });
    tokio::time::sleep(Duration::from_millis(400)).await;
    assert!(!saving.is_finished());
    assert!(!place.read("config.toml").contains("[endpoints.gw]"));
    drop(held);
    let outcome = tokio::time::timeout(LOCK_WAIT, saving)
        .await
        .unwrap()
        .unwrap()
        .unwrap();
    assert!(matches!(outcome, SaveOutcome::Saved(_)), "{outcome:?}");
    assert!(place.read("config.toml").contains("[endpoints.gw]"));
}

struct CardWrite {
    place: Place,
    memory: MemoryKeyring,
    clock: FakeClock,
    host: CliEndpointHost,
}

fn card_write(mode: MemoryMode) -> CardWrite {
    let place = Place::new();
    let memory = MemoryKeyring::new();
    memory.set_mode(mode);
    let clock = FakeClock::default();
    let mut host = place.host(keychain_on(
        memory.store_on(TERMINAL_DEADLINE, clock.clone()),
    ));
    host.budget = Budget::Fresh(TERMINAL_DEADLINE);
    clock.advance(TERMINAL_DEADLINE + Duration::from_secs(1));
    CardWrite {
        place,
        memory,
        clock,
        host,
    }
}

impl CardWrite {
    fn save(&self) -> tokio::task::JoinHandle<Result<SaveOutcome, HostError>> {
        let host = self.host.clone();
        tokio::spawn(async move {
            host.save(
                spec("gw", None, StoreChoice::Auto),
                Secret::new(KEY),
                false,
                idle(),
            )
            .await
        })
    }

    async fn reached(&self) {
        let started = std::time::Instant::now();
        while self.memory.state().calls == 0 {
            assert!(started.elapsed() < Duration::from_secs(10), "never wrote");
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    }

    async fn pass(&self, by: Duration) {
        self.clock.advance(by);
        tokio::time::sleep(Duration::from_millis(30)).await;
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_first_run_card_write_to_a_locked_keychain_waits_from_its_own_start() {
    let card = card_write(MemoryMode::Locked);
    let save = card.save();
    card.reached().await;
    for _ in 0..5 {
        card.pass(Duration::from_secs(1)).await;
    }
    assert!(!save.is_finished(), "past 2 s, inside its 60 s");
    card.memory.set_mode(MemoryMode::Working);
    let outcome = tokio::time::timeout(Duration::from_secs(10), save)
        .await
        .unwrap()
        .unwrap()
        .unwrap();
    let SaveOutcome::Saved(saved) = outcome else {
        panic!("{outcome:?}");
    };
    assert_eq!(saved.store, Kept::Keyring);
    assert_eq!(card.memory.items().len(), 1);
    assert_eq!(card.host.keychain.access().fixed(), None);
    assert!(card.place.read("credentials.json").is_empty());
    assert!(card.place.read("config.toml").contains("[endpoints.gw]"));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_first_run_card_write_that_never_answers_fails_when_its_own_sixty_seconds_pass() {
    let card = card_write(MemoryMode::Hanging);
    let save = card.save();
    card.reached().await;
    card.pass(TERMINAL_DEADLINE - Duration::from_secs(1)).await;
    assert!(!save.is_finished(), "59 s into the write");
    card.pass(Duration::from_secs(1)).await;
    let outcome = tokio::time::timeout(Duration::from_secs(10), save)
        .await
        .unwrap()
        .unwrap()
        .unwrap();
    assert_eq!(
        outcome,
        SaveOutcome::NotSaved(NotSaved {
            name: "gw".into(),
            why: NotSavedReason::KeychainFailed("didn't answer".into()),
        })
    );
    assert!(card.place.read("credentials.json").is_empty());
    assert!(!card.place.read("config.toml").contains("[endpoints.gw]"));
}

fn unconfirming() -> (Place, MemoryKeyring, CliEndpointHost) {
    let place = Place::new();
    let memory = MemoryKeyring::new();
    memory.set_mode(MemoryMode::ReadsFail(BackendError::PlatformFailure));
    let host = place.host(memory_keychain(&memory));
    (place, memory, host)
}

fn store_id_of(memory: &MemoryKeyring) -> String {
    let items = memory.items();
    let account = items.keys().next().unwrap();
    account.split('/').next().unwrap().to_owned()
}

async fn save_gw(host: &CliEndpointHost) -> Result<SaveOutcome, HostError> {
    host.save(
        spec("gw", None, StoreChoice::Auto),
        Secret::new(KEY),
        false,
        idle(),
    )
    .await
}

#[tokio::test]
async fn a_retry_after_an_unconfirmed_write_is_refused_and_never_offers_the_file() {
    let (place, memory, host) = unconfirming();
    let first = save_gw(&host).await.unwrap_err();
    assert!(first.0.contains("may have been saved there"), "{}", first.0);
    let id = store_id_of(&memory);
    assert!(!first.0.contains(&id), "{}", first.0);
    assert!(!first.0.contains("account"), "{}", first.0);
    let session = CliEndpointHost {
        budget: Budget::Short,
        ..host.clone()
    };
    for retry in [&host, &session] {
        let refused = save_gw(retry).await.unwrap_err();
        assert!(
            refused
                .0
                .contains("won't keep the key in credentials.json instead"),
            "{}",
            refused.0
        );
        assert!(!refused.0.contains(&id), "{}", refused.0);
        no_canary(&refused.0);
    }
    assert!(place.read("credentials.json").is_empty());
    assert!(!place.read("config.toml").contains("[endpoints.gw]"));
}

#[tokio::test]
async fn a_keychain_failure_with_no_unconfirmed_write_still_offers_the_file() {
    let place = Place::new();
    let memory = MemoryKeyring::new();
    memory.set_mode(MemoryMode::Failing(BackendError::PlatformFailure));
    let host = place.host(memory_keychain(&memory));
    for _ in 0..2 {
        let outcome = save_gw(&host).await.unwrap();
        assert!(
            matches!(
                &outcome,
                SaveOutcome::NotSaved(NotSaved {
                    why: NotSavedReason::KeychainFailed(_),
                    ..
                })
            ),
            "{outcome:?}"
        );
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn an_unconfirmed_write_names_no_store_id_in_any_event_or_the_log() {
    use wisp_core::agent::{self, Agent, LogContext, default_session};
    use wisp_core::provider::anthropic::{AnthropicProvider, Credential as ApiKey};
    use wisp_core::session::{MemoryStore, Redactions, SessionLog, SessionStore};
    use wisp_protocol::log::CredentialSource;
    use wisp_protocol::{AgentEvent, ClientCommand, RequestId};

    let (_place, memory, host) = unconfirming();
    let provider =
        AnthropicProvider::new(&ApiKey::ApiKey("sk-test".into()), "http://127.0.0.1:9").unwrap();
    let store = MemoryStore::default();
    let context = LogContext {
        endpoint: "http://127.0.0.1:9".into(),
        endpoint_name: "env".into(),
        credential_source: CredentialSource::Environment,
        redactions: Redactions::new([KEY.to_owned()]),
    };
    let agent = with_turn_lines(Agent::new(Arc::new(provider), default_session()))
        .with_endpoint_host(Arc::new(host))
        .with_log(SessionLog::new(Box::new(store.clone()), "s1"), context);
    let (commands, mut events, task) = agent::spawn(agent);
    let mut seen = Vec::new();
    for request in [1, 2] {
        commands
            .send(ClientCommand::SaveEndpoint {
                request: RequestId(request),
                endpoint: spec("gw", None, StoreChoice::Auto),
                key: Secret::new(KEY),
                check: false,
            })
            .await
            .unwrap();
        loop {
            let event = tokio::time::timeout(Duration::from_secs(10), events.recv())
                .await
                .unwrap()
                .unwrap();
            let answered = matches!(
                &event,
                AgentEvent::Rejected { request: Some(r), .. } if *r == RequestId(request)
            );
            assert!(
                !matches!(event, AgentEvent::EndpointNotSaved { .. }),
                "{event:?}"
            );
            seen.push(event);
            if answered {
                break;
            }
        }
    }
    drop(commands);
    while let Some(event) = events.recv().await {
        seen.push(event);
    }
    assert!(task.join().await);
    let id = store_id_of(&memory);
    let shown = format!("{seen:?}");
    let logged = serde_json::to_string(&store.read("s1").unwrap()).unwrap();
    for text in [&shown, &logged] {
        assert!(text.contains("may have been saved there"), "{text}");
        assert!(!text.contains(&id), "{text}");
        assert!(!text.contains("account "), "{text}");
        no_canary(text);
    }
}

mod change;

mod unconfirmed;

#[tokio::test]
async fn the_reconnect_keeps_a_pricing_flag_for_the_run() {
    let place = Place::new();
    let host = place.host(Keychain::off());
    let url = answering(200, r#"{"input_tokens":3}"#);
    host.save(
        spec("gw", Some(url), StoreChoice::Auto),
        Secret::new(KEY),
        false,
        idle(),
    )
    .await
    .unwrap();
    let plain = host.connections().await.unwrap();
    assert!(plain.ready[0].priced);
    let flagged = CliEndpointHost {
        set: crate::settings::flag_layer(Vec::new(), &["endpoints.gw.pricing=none".to_owned()])
            .unwrap(),
        ..place.host(Keychain::off())
    };
    let reconnected = flagged.connections().await.unwrap();
    assert_eq!(reconnected.ready[0].name, "gw");
    assert!(!reconnected.ready[0].priced);
}

#[tokio::test]
async fn account_reads_in_a_chat_keep_its_switches_when_another_chat_saves_a_switch() {
    let place = Place::new();
    let mut host = place.host(Keychain::off());
    let mut gemini = spec("g1", None, StoreChoice::File);
    gemini.provider = "gemini".into();
    gemini.auth = "api_key".into();
    assert!(matches!(
        host.save(gemini, Secret::new("CANARY_GEMINI_HOST"), false, idle())
            .await
            .unwrap(),
        SaveOutcome::Saved(_)
    ));
    let view = host.reconnect().unwrap();
    let ready = view.ready;
    let catalog = wisp_core::endpoints::catalog::LiveCatalog::new(wisp_core::kit::Connected {
        provider: ready[0].provider.clone(),
        ready,
        unavailable: view.unavailable,
        active: "g1".into(),
        env_account: None,
    });
    host.catalog = Some(catalog);
    let text = place.read("config.toml") + "\n[plugins.gemini]\nenabled = false\n";
    std::fs::write(place.config(), text).unwrap();
    assert!(host.sources().off.is_empty());
    assert!(matches!(
        host.sources().entries().unwrap()[&EndpointName::parse("g1").unwrap()],
        wisp_core::endpoints::Availability::Ready(_)
    ));
    let setup = place.host(Keychain::off());
    assert_eq!(setup.sources().off.get("gemini"), Some(&false));
    assert!(
        matches!(setup.sources().entries().unwrap()[&EndpointName::parse("g1").unwrap()], wisp_core::endpoints::Availability::Unavailable(ref u) if u.reason == "gemini is off")
    );
}

#[tokio::test]
async fn an_account_saved_or_edited_inside_a_chat_follows_the_chats_off_set() {
    for startup in [None, Some("g1")] {
        let place = Place::new();
        let mut host = place.host(Keychain::off());
        let mut gemini = spec("g1", None, StoreChoice::File);
        gemini.provider = "gemini".into();
        gemini.auth = "api_key".into();
        let mut other = spec("other", None, StoreChoice::File);
        other.default = false;
        for (account, key) in [(gemini.clone(), "CANARY_GEMINI_HOST"), (other, KEY)] {
            assert!(matches!(
                host.save(account, Secret::new(key), false, idle())
                    .await
                    .unwrap(),
                SaveOutcome::Saved(_)
            ));
        }
        let view = host.reconnect().unwrap();
        let provider = view
            .ready
            .iter()
            .find(|c| c.name == "other")
            .unwrap()
            .provider
            .clone();
        let catalog = wisp_core::endpoints::catalog::LiveCatalog::new(wisp_core::kit::Connected {
            provider,
            ready: view.ready,
            unavailable: view.unavailable,
            active: "other".into(),
            env_account: None,
        })
        .with_summaries(view.endpoints);
        catalog.set_providers_off(std::collections::BTreeMap::from([(
            "gemini".to_owned(),
            false,
        )]));
        host.endpoint = startup.map(str::to_owned);
        host.catalog = Some(catalog.clone());
        let ready =
            |off_accounts: &[&str]| {
                let view = catalog.view();
                assert!(
                    view.ready.iter().any(|c| c.name == "other"),
                    "{:?}",
                    view.unavailable
                );
                for name in off_accounts {
                    assert!(
                        view.unavailable
                            .iter()
                            .any(|(alias, reason)| alias == name && reason == "gemini is off"),
                        "{:?}",
                        view.unavailable
                    );
                }
                assert!(view.endpoints.iter().any(|summary| summary.name == "g1"
                    && summary.default
                    && summary.mark.is_some()));
            };
        let edited = host
            .edit(
                "g1".into(),
                EndpointEdit {
                    key: Some(Secret::new("CANARY_EDITED_GEMINI")),
                    ..EndpointEdit::default()
                },
                None,
                false,
            )
            .await
            .unwrap();
        assert!(matches!(edited, EditOutcome::Edited(_)), "{edited:?}");
        ready(&["g1"]);
        gemini.name = "g2".into();
        gemini.default = false;
        assert!(matches!(
            host.save(gemini, Secret::new("CANARY_ADDED_GEMINI"), false, idle())
                .await
                .unwrap(),
            SaveOutcome::Saved(_)
        ));
        ready(&["g1", "g2"]);
        let edited = host
            .edit(
                "other".into(),
                EndpointEdit {
                    key: Some(Secret::new("CANARY_EDITED_OTHER")),
                    ..EndpointEdit::default()
                },
                None,
                false,
            )
            .await
            .unwrap();
        assert!(matches!(edited, EditOutcome::Edited(_)), "{edited:?}");
        ready(&["g1", "g2"]);
        let reloaded = host.connections().await.unwrap();
        assert!(reloaded.ready.iter().any(|c| c.name == "other"));
        assert!(
            reloaded
                .unavailable
                .iter()
                .any(|(name, reason)| name == "g1" && reason == "gemini is off")
        );
        assert!(matches!(
            place.host(Keychain::off()).sources().entries().unwrap()
                [&EndpointName::parse("g1").unwrap()],
            wisp_core::endpoints::Availability::Ready(_)
        ));
    }
}

#[test]
fn the_keys_file_account_keeps_its_list_row_when_its_provider_is_off() {
    let place = Place::new();
    let provider = ProviderKind::IMPLICIT.as_str();
    std::fs::write(
        place.config(),
        format!("[plugins.{provider}]\nenabled = false\n"),
    )
    .unwrap();
    let mut host = place.host(Keychain::off());
    let keys = place.dir.root.join("synthetic-keys");
    std::fs::write(
        &keys,
        "ANTHROPIC_API_KEY=sk-ant-CANARY_IMPLICIT_LIST\nANTHROPIC_BASE_URL=http://127.0.0.1:1\n",
    )
    .unwrap();
    host.env_file = Some(keys);
    let accounts = crate::auth::accounts::gather(&host.context()).unwrap();
    assert_eq!(accounts.list.len(), 1);
    let account = &accounts.list[0];
    assert!(matches!(
        account.held,
        crate::auth::accounts::Held::Variable {
            keys_file: true,
            ..
        }
    ));
    assert_eq!(
        account.off,
        Some((
            wisp_core::plugin::registry::off_words(provider, false),
            wisp_core::plugin::registry::provider_fix(provider, false)
        ))
    );
}
