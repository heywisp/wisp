use std::sync::Arc;

use keyring_core::CredentialStore;

use super::{BackendError, KeyringBackend, Opener, SERVICE};

pub fn map_error(error: &keyring_core::Error) -> BackendError {
    match error {
        keyring_core::Error::NoEntry => BackendError::NoEntry,
        keyring_core::Error::NoStorageAccess(_) => BackendError::NoStorageAccess,
        keyring_core::Error::PlatformFailure(_) => BackendError::PlatformFailure,
        keyring_core::Error::TooLong(_, _) => BackendError::TooLong,
        keyring_core::Error::BadEncoding(_) => BackendError::BadEncoding,
        keyring_core::Error::BadDataFormat(_, _) => BackendError::BadDataFormat,
        keyring_core::Error::BadStoreFormat(_) => BackendError::PlatformFailure,
        keyring_core::Error::Invalid(_, _) => BackendError::Invalid,
        keyring_core::Error::Ambiguous(_) => BackendError::Ambiguous,
        keyring_core::Error::NoDefaultStore | keyring_core::Error::NotSupportedByStore(_) => {
            BackendError::Unsupported
        }
        _ => BackendError::Other,
    }
}

struct PlatformBackend {
    store: Arc<CredentialStore>,
}

impl PlatformBackend {
    fn entry(&self, account: &str) -> Result<keyring_core::Entry, BackendError> {
        self.store
            .build(SERVICE, account, None)
            .map_err(|e| map_error(&e))
    }
}

impl KeyringBackend for PlatformBackend {
    fn get(&mut self, account: &str) -> Result<String, BackendError> {
        self.entry(account)?
            .get_password()
            .map_err(|e| map_error(&e))
    }

    fn set(&mut self, account: &str, secret: &str) -> Result<(), BackendError> {
        self.entry(account)?
            .set_password(secret)
            .map_err(|e| map_error(&e))
    }

    fn delete(&mut self, account: &str) -> Result<(), BackendError> {
        self.entry(account)?
            .delete_credential()
            .map_err(|e| map_error(&e))
    }
}

#[cfg(any(
    target_os = "linux",
    target_os = "freebsd",
    target_os = "openbsd",
    target_os = "netbsd",
    target_os = "dragonfly"
))]
fn open_store() -> Result<Arc<CredentialStore>, BackendError> {
    zbus_secret_service_keyring_store::Store::new()
        .map(|store| store as Arc<CredentialStore>)
        .map_err(|e| map_error(&e))
}

#[cfg(target_os = "macos")]
fn open_store() -> Result<Arc<CredentialStore>, BackendError> {
    apple_native_keyring_store::keychain::Store::new()
        .map(|store| store as Arc<CredentialStore>)
        .map_err(|e| map_error(&e))
}

#[cfg(windows)]
fn open_store() -> Result<Arc<CredentialStore>, BackendError> {
    windows_native_keyring_store::store::Store::new()
        .map(|store| store as Arc<CredentialStore>)
        .map_err(|e| map_error(&e))
}

#[cfg(not(any(
    target_os = "linux",
    target_os = "freebsd",
    target_os = "openbsd",
    target_os = "netbsd",
    target_os = "dragonfly",
    target_os = "macos",
    windows
)))]
fn open_store() -> Result<Arc<CredentialStore>, BackendError> {
    Err(BackendError::Unsupported)
}

pub fn opener() -> Opener {
    Box::new(|| {
        let store = open_store()?;
        Ok(Box::new(PlatformBackend { store }) as Box<dyn KeyringBackend>)
    })
}

#[cfg(any(
    target_os = "linux",
    target_os = "freebsd",
    target_os = "openbsd",
    target_os = "netbsd",
    target_os = "dragonfly"
))]
mod bus {
    use std::sync::mpsc;

    use crate::secrets::detect::{BUS_LIMIT, BusAnswer, BusCheck, SECRETS_NAME};

    #[derive(Clone, Copy, Debug, Default)]
    pub struct SessionBus;

    fn ask() -> BusAnswer {
        let Ok(connection) = zbus::blocking::Connection::session() else {
            return BusAnswer::NoBus;
        };
        let Ok(proxy) = zbus::blocking::fdo::DBusProxy::new(&connection) else {
            return BusAnswer::Silent;
        };
        let Ok(name) = zbus::names::BusName::try_from(SECRETS_NAME) else {
            return BusAnswer::Missing;
        };
        match proxy.name_has_owner(name) {
            Ok(true) => return BusAnswer::Owned,
            Ok(false) => {}
            Err(_) => return BusAnswer::Missing,
        }
        match proxy.list_activatable_names() {
            Ok(names) if names.iter().any(|n| n.as_str() == SECRETS_NAME) => BusAnswer::Activatable,
            Ok(_) | Err(_) => BusAnswer::Missing,
        }
    }

    impl BusCheck for SessionBus {
        fn secret_service(&self) -> BusAnswer {
            let (sender, answer) = mpsc::channel();
            let spawned = std::thread::Builder::new()
                .name("wisp-bus-check".to_owned())
                .spawn(move || {
                    let _ = sender.send(ask());
                });
            if spawned.is_err() {
                return BusAnswer::Silent;
            }
            answer.recv_timeout(BUS_LIMIT).unwrap_or(BusAnswer::Silent)
        }
    }
}

#[cfg(any(
    target_os = "linux",
    target_os = "freebsd",
    target_os = "openbsd",
    target_os = "netbsd",
    target_os = "dragonfly"
))]
pub use bus::SessionBus;

#[cfg(test)]
mod tests;
