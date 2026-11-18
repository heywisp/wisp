use std::time::{Duration, Instant};

use wisp_core::endpoints::{ConfigFile, PathShown};
use wisp_core::secrets::detect::Backend;
use wisp_core::secrets::keyring::{MemoryKeyring, MemoryMode, account, encode};
use wisp_core::secrets::{StoreId, Stored, StoredRecord};
use wisp_protocol::Secret;

use super::*;

const ID: &str = "0123456789abcdef";

fn auto(backend: Detected, store: Option<KeyringStore>) -> Keychain {
    Keychain::assemble(
        KeychainChoice {
            setting: KeychainSetting::Auto,
            origin: None,
        },
        backend,
        false,
        store,
    )
}

fn available() -> Detected {
    Detected::Available {
        backend: Backend::SecretService,
        down: None,
    }
}

fn config(text: &str) -> ConfigFile {
    ConfigFile::parse(
        std::path::Path::new("config.toml"),
        PathShown::Default,
        text,
    )
    .unwrap()
}

fn two_keychain_endpoints() -> ConfigFile {
    config(&format!(
        "store_id = \"{ID}\"\ndefault = \"main\"\n\n[endpoints.main]\nauth = \"api_key\"\nkey_store = \"keyring\"\n\n[endpoints.side]\nauth = \"api_key\"\nkey_store = \"keyring\"\n\n[endpoints.file]\nauth = \"api_key\"\nkey_store = \"file\"\n"
    ))
}

fn put(memory: &MemoryKeyring, name: &str, key: &str) {
    let id = StoreId::parse(ID).unwrap();
    let stored = Stored {
        record: StoredRecord::Key {
            key: Secret::new(key),
            url: None,
        },
        owner: None,
    };
    memory.insert(
        &account(&id, &EndpointName::parse(name).unwrap()),
        &encode(&stored),
    );
}

#[test]
fn the_doctor_row_for_each_state() {
    assert_eq!(Keychain::off().row(0), "off (WISP_KEYCHAIN)");
    let wsl = auto(
        Detected::Absent {
            why: NoKeychain::Wsl,
        },
        None,
    );
    assert_eq!(wsl.row(0), "none (WSL); variables and keys files are read");
    let no_service = auto(
        Detected::Absent {
            why: NoKeychain::NoSecretService,
        },
        None,
    );
    assert_eq!(
        no_service.row(0),
        "none (no Secret Service on the session bus); variables and keys files are read"
    );
    let memory = MemoryKeyring::new();
    let working = auto(available(), Some(memory.store(Duration::from_secs(30))));
    assert_eq!(working.row(1), "Secret Service (keychain = auto), 1 key");
    assert_eq!(working.row(2), "Secret Service (keychain = auto), 2 keys");
    let silent = auto(
        available(),
        Some(
            memory
                .store(Duration::from_secs(30))
                .down_from_start(KeyringError::BusSilent),
        ),
    );
    assert_eq!(
        silent.row(0),
        "Secret Service (keychain = auto), didn't answer"
    );
    memory.set_mode(MemoryMode::Unopenable);
    let store = memory.store(Duration::from_secs(30));
    let id = StoreId::parse(ID).unwrap();
    let _ = store.read(&id, &EndpointName::parse("a").unwrap(), Budget::Start);
    assert_eq!(
        auto(available(), Some(store)).row(0),
        "Secret Service (keychain = auto), not running"
    );
    let unread = Keychain::assemble(
        KeychainChoice {
            setting: KeychainSetting::Auto,
            origin: None,
        },
        available(),
        true,
        None,
    );
    assert_eq!(
        unread.row(0),
        "Secret Service (keychain = auto; config.toml doesn't parse, so its setting wasn't read)"
    );
}

#[test]
fn the_start_endpoint_gets_the_deadline_and_the_rest_the_short_budget() {
    let memory = MemoryKeyring::new();
    put(&memory, "main", "k-main");
    put(&memory, "side", "k-side");
    let keychain = auto(available(), Some(memory.store(Duration::from_secs(30))));
    let found = keychain.prefetch(&two_keychain_endpoints(), None, false);
    assert_eq!(
        found.len(),
        2,
        "file endpoints aren't read from the keychain"
    );
    assert!(found.values().all(|r| matches!(r, Ok(Some(_)))));

    let memory = MemoryKeyring::new();
    memory.set_mode(MemoryMode::Hanging);
    let keychain = auto(available(), Some(memory.store(Duration::from_secs(30))));
    let started = Instant::now();
    let found = keychain.prefetch(&two_keychain_endpoints(), Some("file"), false);
    let spent = started.elapsed();
    assert!(spent < Duration::from_secs(4), "{spent:?}");
    assert!(found.values().all(|r| *r == Err(KeyringError::TimedOut)));
    assert_eq!(memory.state().calls, 1, "down after the first failure");
}

#[test]
fn under_off_nothing_is_read_and_every_keychain_endpoint_says_why() {
    let found = Keychain::off().prefetch(&two_keychain_endpoints(), None, false);
    assert_eq!(found.len(), 2);
    for result in found.values() {
        assert_eq!(
            *result,
            Err(KeyringError::Off {
                origin: "WISP_KEYCHAIN".to_owned()
            })
        );
    }
}

#[test]
fn the_config_setting_is_read_only_from_a_config_that_parses() {
    assert!(matches!(config_setting(Err(())), ConfigSetting::Unread));
    let with = config("keychain = \"off\"\n");
    assert!(matches!(
        config_setting(Ok(Some(&with))),
        ConfigSetting::Read(Some(KeychainSetting::Off))
    ));
    assert!(matches!(
        config_setting(Ok(None)),
        ConfigSetting::Read(None)
    ));
}

#[test]
fn the_note_for_a_machine_without_a_keychain() {
    let wsl = auto(
        Detected::Absent {
            why: NoKeychain::Wsl,
        },
        None,
    );
    assert_eq!(
        wsl.absent_note("~/.config/wisp/credentials.json")
            .as_deref(),
        Some(
            "Under WSL, Wisp keeps keys in ~/.config/wisp/credentials.json, readable only by you. --store keyring or keychain = \"on\" uses a keychain instead."
        )
    );
    assert!(Keychain::off().absent_note("x").is_none());
    assert!(auto(available(), None).absent_note("x").is_none());
}

#[test]
fn an_exported_provider_key_takes_the_long_budget_from_the_default() {
    let memory = MemoryKeyring::new();
    memory.set_mode(MemoryMode::Hanging);
    let keychain = auto(available(), Some(memory.store(Duration::from_secs(30))));
    let started = Instant::now();
    let found = keychain.prefetch(&two_keychain_endpoints(), None, true);
    let spent = started.elapsed();
    assert!(spent < Duration::from_secs(4), "{spent:?}");
    assert!(found.values().all(|r| *r == Err(KeyringError::TimedOut)));
}
