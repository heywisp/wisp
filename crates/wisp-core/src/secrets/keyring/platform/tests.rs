use keyring_core::Error;

use super::*;

const CANARY: &str = "canary-secret-0123456789abcdef";

fn boxed() -> keyring_core::error::PlatformError {
    Box::new(std::io::Error::other(CANARY))
}

#[test]
fn errors_map_by_kind_and_carry_none_of_their_payload() {
    let cases = [
        (Error::NoEntry, BackendError::NoEntry),
        (
            Error::NoStorageAccess(boxed()),
            BackendError::NoStorageAccess,
        ),
        (
            Error::PlatformFailure(boxed()),
            BackendError::PlatformFailure,
        ),
        (Error::TooLong(CANARY.to_owned(), 7), BackendError::TooLong),
        (
            Error::BadEncoding(CANARY.as_bytes().to_vec()),
            BackendError::BadEncoding,
        ),
        (
            Error::BadDataFormat(CANARY.as_bytes().to_vec(), boxed()),
            BackendError::BadDataFormat,
        ),
        (
            Error::BadStoreFormat(CANARY.to_owned()),
            BackendError::PlatformFailure,
        ),
        (
            Error::Invalid(CANARY.to_owned(), CANARY.to_owned()),
            BackendError::Invalid,
        ),
        (Error::Ambiguous(Vec::new()), BackendError::Ambiguous),
        (Error::NoDefaultStore, BackendError::Unsupported),
        (
            Error::NotSupportedByStore(CANARY.to_owned()),
            BackendError::Unsupported,
        ),
    ];
    for (error, kind) in cases {
        let mapped = map_error(&error);
        assert_eq!(mapped, kind);
        let shown = format!("{mapped:?}");
        assert!(!shown.contains(CANARY), "{shown}");
    }
}

#[test]
fn bad_text_from_the_keychain_reaches_output_without_its_bytes() {
    let memory = super::super::MemoryKeyring::new();
    memory.set_mode(super::super::MemoryMode::Failing(map_error(
        &Error::BadEncoding(CANARY.as_bytes().to_vec()),
    )));
    let store = memory.store(std::time::Duration::from_secs(30));
    let id = crate::secrets::StoreId::parse("0123456789abcdef").unwrap();
    let name = crate::endpoints::EndpointName::parse("direct").unwrap();
    let error = store
        .read(&id, &name, super::super::Budget::Start)
        .unwrap_err();
    let shown = format!("{error} {error:?}");
    assert_eq!(
        error.to_string(),
        "the keychain item for direct isn't valid text"
    );
    assert!(!shown.contains(CANARY), "{shown}");
}

#[cfg(target_os = "linux")]
#[test]
#[ignore = "needs a throwaway Secret Service; run it in the container described in docs/testing.md"]
fn a_real_secret_service_round_trips_an_item() {
    if std::env::var_os("WISP_CONTAINER_KEYCHAIN").is_none() {
        return;
    }
    use crate::secrets::detect::{BusAnswer, BusCheck};
    use crate::secrets::{Stored, StoredRecord};
    assert!(matches!(
        SessionBus.secret_service(),
        BusAnswer::Owned | BusAnswer::Activatable
    ));
    let store = super::super::KeyringStore::new(opener(), std::time::Duration::from_secs(20));
    let id = crate::secrets::StoreId::new_random();
    let name = crate::endpoints::EndpointName::parse("direct").unwrap();
    let stored = Stored {
        record: StoredRecord::Key {
            key: wisp_protocol::Secret::new("k-container"),
            url: Some(wisp_protocol::Secret::new("https://gw.example")),
        },
        owner: Some(std::path::PathBuf::from("/c/config.toml")),
    };
    let budget = super::super::Budget::Start;
    assert_eq!(store.read(&id, &name, budget), Ok(None));
    store.write(&id, &name, &stored, budget).unwrap();
    let fresh = super::super::KeyringStore::new(opener(), std::time::Duration::from_secs(20));
    assert_eq!(fresh.read(&id, &name, budget), Ok(Some(stored)));
    assert_eq!(fresh.delete(&id, &name, budget), Ok(true));
    let again = super::super::KeyringStore::new(opener(), std::time::Duration::from_secs(20));
    assert_eq!(again.read(&id, &name, budget), Ok(None));
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .unwrap();
    let inside = super::super::KeyringStore::new(opener(), std::time::Duration::from_secs(20));
    let found = runtime.block_on(async { inside.read(&id, &name, budget) });
    assert_eq!(found, Ok(None), "works when started from inside a runtime");
}
