use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use wisp_core::config::McpServer;
use wisp_core::endpoints::{ConfigFile, EndpointName, Variables};
use wisp_core::provider::credential::EnvFile;
use wisp_core::secrets::detect::{
    BusCheck, Detected, DetectionInputs, KeychainChoice, KeychainSetting, NoKeychain, Platform,
    WslFacts, detect,
};
use wisp_core::secrets::keyring::{
    Budget, HEADLESS_DEADLINE, KeyringAccess, KeyringError, KeyringStore, TERMINAL_DEADLINE,
    Unreachable,
};
use wisp_core::secrets::{KeyringRecords, keyring_names};
use wisp_core::session::Redactions;
use wisp_protocol::CONFIG_FILE;
use wisp_protocol::settings::{SettingValue, names};

pub struct Keychain {
    choice: KeychainChoice,
    detected: Detected,
    config_unread: bool,
    store: Option<KeyringStore>,
    fallback: Unreachable,
    unconfirmed: AtomicBool,
}

impl std::fmt::Debug for Keychain {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Keychain")
            .field("choice", &self.choice)
            .field("detected", &self.detected)
            .field("store", &self.store)
            .finish()
    }
}

#[cfg(not(any(
    target_os = "linux",
    target_os = "freebsd",
    target_os = "openbsd",
    target_os = "netbsd",
    target_os = "dragonfly"
)))]
struct NoBusHere;

#[cfg(not(any(
    target_os = "linux",
    target_os = "freebsd",
    target_os = "openbsd",
    target_os = "netbsd",
    target_os = "dragonfly"
)))]
impl BusCheck for NoBusHere {
    fn secret_service(&self) -> wisp_core::secrets::detect::BusAnswer {
        wisp_core::secrets::detect::BusAnswer::NoBus
    }
}

#[cfg(any(
    target_os = "linux",
    target_os = "freebsd",
    target_os = "openbsd",
    target_os = "netbsd",
    target_os = "dragonfly"
))]
fn real_bus() -> Box<dyn BusCheck> {
    Box::new(wisp_core::secrets::keyring::platform::SessionBus)
}

#[cfg(not(any(
    target_os = "linux",
    target_os = "freebsd",
    target_os = "openbsd",
    target_os = "netbsd",
    target_os = "dragonfly"
)))]
fn real_bus() -> Box<dyn BusCheck> {
    Box::new(NoBusHere)
}

pub enum ConfigSetting {
    Read(Option<KeychainSetting>),
    Unread,
}

pub fn config_setting(config: Result<Option<&ConfigFile>, ()>) -> ConfigSetting {
    match config {
        Err(()) => ConfigSetting::Unread,
        Ok(config) => ConfigSetting::Read(
            config
                .and_then(|c| c.settings.global(names::KEYCHAIN))
                .and_then(|v| KeychainSetting::parse(&v.text())),
        ),
    }
}

pub fn flag_setting(value: &SettingValue) -> Option<KeychainSetting> {
    KeychainSetting::parse(&value.text())
}

impl Keychain {
    pub fn load(
        flag: Option<(KeychainSetting, String)>,
        variable: Option<&str>,
        config: ConfigSetting,
        terminal: bool,
        notice: wisp_core::secrets::keyring::Notice,
    ) -> Result<Self, String> {
        let (from_config, config_unread) = match config {
            ConfigSetting::Read(setting) => (setting.map(|s| (s, CONFIG_FILE.to_owned())), false),
            ConfigSetting::Unread => (None, true),
        };
        let choice = KeychainChoice::decide(flag, variable, from_config)?;
        let bus = real_bus();
        let inputs = DetectionInputs {
            platform: Platform::current(),
            wsl: WslFacts::read(Path::new("/")),
            bus: bus.as_ref(),
            built: true,
        };
        let detected = detect(&choice, &inputs)?;
        let store = match &detected {
            Detected::Off { .. } => None,
            Detected::Available { down, .. } => {
                let store = real_store(terminal, notice);
                Some(match down {
                    Some(reason) => store.down_from_start(reason.clone()),
                    None => store,
                })
            }
            Detected::Absent { .. } => Some(real_store(terminal, notice)),
        };
        Ok(Self::assemble(choice, detected, config_unread, store))
    }

    pub fn assemble(
        choice: KeychainChoice,
        detected: Detected,
        config_unread: bool,
        store: Option<KeyringStore>,
    ) -> Self {
        let fallback = Unreachable(detected.off_error().unwrap_or(KeyringError::Unsupported));
        Self {
            choice,
            detected,
            config_unread,
            store,
            fallback,
            unconfirmed: AtomicBool::new(false),
        }
    }

    pub fn left_unconfirmed(&self) {
        self.unconfirmed.store(true, Ordering::Relaxed);
    }

    pub fn has_unconfirmed(&self) -> bool {
        self.unconfirmed.load(Ordering::Relaxed)
    }

    #[cfg(any(test, feature = "test-support"))]
    pub fn off() -> Self {
        Self::assemble(
            KeychainChoice {
                setting: KeychainSetting::Off,
                origin: Some("WISP_KEYCHAIN".to_owned()),
            },
            Detected::Off {
                origin: "WISP_KEYCHAIN".to_owned(),
            },
            false,
            None,
        )
    }

    #[cfg(any(test, feature = "test-support"))]
    pub fn available_down() -> Self {
        Self::assemble(
            KeychainChoice {
                setting: KeychainSetting::On,
                origin: Some("WISP_KEYCHAIN".to_owned()),
            },
            Detected::Available {
                backend: wisp_core::secrets::detect::Backend::SecretService,
                down: Some(KeyringError::NotRunning),
            },
            false,
            None,
        )
    }

    pub fn choice(&self) -> &KeychainChoice {
        &self.choice
    }

    pub fn detected(&self) -> &Detected {
        &self.detected
    }

    pub fn access(&self) -> &(dyn KeyringAccess + Sync) {
        match &self.store {
            Some(store) => store,
            None => &self.fallback,
        }
    }

    pub fn off_error(&self) -> Option<KeyringError> {
        self.detected.off_error()
    }

    pub fn budgeted(&self, budget: Budget) -> Budgeted<'_> {
        Budgeted {
            keychain: self,
            budget,
        }
    }

    pub fn prefetch_short(&self, config: &ConfigFile) -> KeyringRecords {
        let mut found = KeyringRecords::new();
        let Some(id) = &config.store_id else {
            return found;
        };
        for name in keyring_names(config) {
            let result = match &self.store {
                Some(store) => store.read(id, &name, Budget::Short),
                None => Err(self.fallback.0.clone()),
            };
            found.insert(name, result);
        }
        found
    }

    pub fn prefetch(
        &self,
        config: &ConfigFile,
        start: Option<&str>,
        exported: bool,
    ) -> KeyringRecords {
        let names = keyring_names(config);
        let mut found = KeyringRecords::new();
        let Some(id) = &config.store_id else {
            return found;
        };
        let start = match start {
            Some(text) => EndpointName::parse(text),
            None if exported => None,
            None => config.default.clone(),
        };
        let mut ordered: Vec<(EndpointName, Budget)> = Vec::new();
        for name in &names {
            let budget = if start.as_ref() == Some(name) {
                Budget::Start
            } else {
                Budget::Short
            };
            ordered.push((name.clone(), budget));
        }
        ordered.sort_by_key(|(_, budget)| *budget != Budget::Start);
        for (name, budget) in ordered {
            let result = match &self.store {
                Some(store) => store.read(id, &name, budget),
                None => Err(self.fallback.0.clone()),
            };
            found.insert(name, result);
        }
        found
    }

    pub fn row(&self, keys: usize) -> String {
        let unread = if self.config_unread {
            concat!(
                "; ",
                wisp_protocol::config_file!(),
                " doesn't parse, so its setting wasn't read"
            )
        } else {
            ""
        };
        match &self.detected {
            Detected::Off { origin } => format!("off ({origin})"),
            Detected::Absent { why } => {
                format!(
                    "none ({}{unread}); variables and keys files are read",
                    why.words()
                )
            }
            Detected::Available { backend, .. } => {
                let mut line = format!(
                    "{} (keychain = {}{}{unread})",
                    backend.words(),
                    self.choice.setting.as_str(),
                    match self.choice.origin.as_deref() {
                        Some(origin) if origin != CONFIG_FILE => format!(", from {origin}"),
                        _ => String::new(),
                    }
                );
                if keys > 0 {
                    let noun = if keys == 1 { "key" } else { "keys" };
                    line.push_str(&format!(", {keys} {noun}"));
                }
                match self.store.as_ref().and_then(KeyringStore::down) {
                    Some(KeyringError::TimedOut | KeyringError::BusSilent) => {
                        line.push_str(", didn't answer");
                    }
                    Some(KeyringError::NotRunning) => line.push_str(", not running"),
                    Some(KeyringError::Locked) => line.push_str(", locked"),
                    Some(_) => line.push_str(", failing"),
                    None => {}
                }
                line
            }
        }
    }

    pub fn absent_note(&self, file: &str) -> Option<String> {
        match &self.detected {
            Detected::Absent {
                why: NoKeychain::Wsl,
            } => Some(format!(
                "Under WSL, Wisp keeps keys in {file}, readable only by you. --store keyring or keychain = \"on\" uses a keychain instead."
            )),
            Detected::Absent { why } => Some(format!(
                "No keychain here ({}), so Wisp keeps keys in {file}, readable only by you.",
                why.words()
            )),
            _ => None,
        }
    }
}

pub struct Budgeted<'a> {
    keychain: &'a Keychain,
    budget: Budget,
}

impl KeyringAccess for Budgeted<'_> {
    fn fixed(&self) -> Option<KeyringError> {
        self.keychain.access().fixed()
    }

    fn begin_phase(&self) {
        self.keychain.access().begin_phase();
    }

    fn read(
        &self,
        id: &wisp_core::secrets::StoreId,
        name: &EndpointName,
    ) -> Result<Option<wisp_core::secrets::Stored>, KeyringError> {
        match &self.keychain.store {
            Some(store) => store.read(id, name, self.budget),
            None => self.keychain.fallback.read(id, name),
        }
    }

    fn write(
        &self,
        id: &wisp_core::secrets::StoreId,
        name: &EndpointName,
        stored: &wisp_core::secrets::Stored,
    ) -> Result<(), KeyringError> {
        match &self.keychain.store {
            Some(store) => store.write(id, name, stored, self.budget),
            None => self.keychain.fallback.write(id, name, stored),
        }
    }

    fn delete(
        &self,
        id: &wisp_core::secrets::StoreId,
        name: &EndpointName,
    ) -> Result<bool, KeyringError> {
        match &self.keychain.store {
            Some(store) => store.delete(id, name, self.budget),
            None => self.keychain.fallback.delete(id, name),
        }
    }
}

pub struct EnvText(String);

impl EnvText {
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl std::fmt::Debug for EnvText {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "EnvText(<{} bytes>)", self.0.len())
    }
}

#[derive(Debug)]
pub enum Unscanned {
    GivenMissing,
    NotUtf8(EnvText),
    Unreadable(std::io::ErrorKind),
}

#[derive(Debug)]
pub enum EnvScan {
    Missing,
    Text(EnvText),
    Failed(Unscanned),
}

#[derive(Debug)]
enum Inputs {
    Available {
        scan: EnvScan,
    },
    Absent {
        env_file: Result<Option<Arc<EnvFile>>, String>,
    },
}

pub type Lookup = &'static (dyn Fn(&str) -> Option<String> + Sync);

#[derive(Debug)]
pub struct KeychainInputs {
    pub env_path: Option<PathBuf>,
    pub explicit_env_file: bool,
    pub keys_file: String,
    inputs: Inputs,
    #[cfg(any(test, feature = "test-support"))]
    test_env: TestEnv,
}

#[cfg(any(test, feature = "test-support"))]
pub struct TestEnv(pub Option<Lookup>);

#[cfg(any(test, feature = "test-support"))]
impl std::fmt::Debug for TestEnv {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("TestEnv")
    }
}

#[cfg(any(test, feature = "test-support"))]
fn no_env(_: &str) -> Option<String> {
    None
}

impl KeychainInputs {
    pub fn load(
        available: bool,
        env_path: Option<PathBuf>,
        explicit: bool,
        home: Option<&str>,
    ) -> Self {
        let inputs = if available {
            Inputs::Available {
                scan: match &env_path {
                    Some(path) => scan(path, explicit),
                    None => EnvScan::Missing,
                },
            }
        } else {
            Inputs::Absent {
                env_file: match &env_path {
                    Some(path) => {
                        wisp_core::secrets::read_env_file(path, explicit).map(|f| f.map(Arc::new))
                    }
                    None => Ok(None),
                },
            }
        };
        let mut loaded = Self {
            env_path,
            explicit_env_file: explicit,
            keys_file: String::new(),
            inputs,
            #[cfg(any(test, feature = "test-support"))]
            test_env: TestEnv((!crate::paths::process_env_used()).then_some(&no_env)),
        };
        loaded.keys_file = match &loaded.env_path {
            Some(path) if loaded.shows_path() => crate::endpoints::short(path, home),
            Some(_) => wisp_core::secrets::GIVEN_ENV_FILE.to_owned(),
            None => wisp_protocol::DEFAULT_KEYS_FILE.to_owned(),
        };
        loaded
    }

    pub fn shows_path(&self) -> bool {
        !self.explicit_env_file
            || self.env_text().is_some()
            || matches!(self.env_file(), Ok(Some(_)))
    }

    #[cfg(any(test, feature = "test-support"))]
    pub fn with_env(mut self, env: Lookup) -> Self {
        self.test_env = TestEnv(Some(env));
        self
    }

    pub fn available(&self) -> bool {
        matches!(self.inputs, Inputs::Available { .. })
    }

    pub fn env_file(&self) -> Result<Option<&EnvFile>, &str> {
        match &self.inputs {
            Inputs::Absent { env_file } => env_file
                .as_ref()
                .map(|f| f.as_deref())
                .map_err(String::as_str),
            Inputs::Available { .. } => Ok(None),
        }
    }

    pub fn scan(&self) -> Option<&EnvScan> {
        match &self.inputs {
            Inputs::Available { scan } => Some(scan),
            Inputs::Absent { .. } => None,
        }
    }

    pub fn env_text(&self) -> Option<&EnvText> {
        match self.scan() {
            Some(EnvScan::Text(text)) => Some(text),
            _ => None,
        }
    }

    pub fn redactions<'a>(
        &self,
        config: Option<&'a ConfigFile>,
        run_servers: impl IntoIterator<Item = &'a McpServer>,
    ) -> Redactions {
        #[cfg(not(any(test, feature = "test-support")))]
        let env: Lookup = &crate::paths::real_env;
        #[cfg(any(test, feature = "test-support"))]
        let env: Lookup = self.test_env.0.unwrap_or(&crate::paths::real_env);
        let text = match self.scan() {
            Some(EnvScan::Text(text) | EnvScan::Failed(Unscanned::NotUtf8(text))) => Some(text),
            _ => None,
        };
        #[cfg(not(any(test, feature = "test-support")))]
        let vars = crate::paths::real_vars();
        #[cfg(any(test, feature = "test-support"))]
        let vars = Vec::new();
        wisp_core::secrets::variable_redactions(config, run_servers, env, text.map(EnvText::as_str))
            .union([&wisp_core::secrets::named_key_values(vars)])
    }
}

fn readable_lines(bytes: &[u8]) -> EnvText {
    let lines: Vec<&str> = bytes
        .split(|b| *b == b'\n')
        .filter_map(|line| std::str::from_utf8(line).ok())
        .collect();
    EnvText(lines.join("\n"))
}

fn scan(path: &Path, explicit: bool) -> EnvScan {
    match std::fs::read(path) {
        Ok(bytes) => match String::from_utf8(bytes) {
            Ok(text) => EnvScan::Text(EnvText(text)),
            Err(e) => EnvScan::Failed(Unscanned::NotUtf8(readable_lines(e.as_bytes()))),
        },
        Err(e) if crate::endpoints::missing(e.kind()) && explicit => {
            EnvScan::Failed(Unscanned::GivenMissing)
        }
        Err(e) if crate::endpoints::missing(e.kind()) => EnvScan::Missing,
        Err(e) => EnvScan::Failed(Unscanned::Unreadable(e.kind())),
    }
}

pub fn keychain(inputs: &KeychainInputs) -> Result<wisp_core::endpoints::Keychain<'_>, String> {
    match &inputs.inputs {
        Inputs::Available { .. } => {
            Ok(wisp_core::endpoints::Keychain::Available(&inputs.keys_file))
        }
        Inputs::Absent { env_file } => {
            let file = env_file.as_ref().map_err(Clone::clone)?.as_deref();
            #[cfg(not(any(test, feature = "test-support")))]
            let env: Lookup = &crate::paths::real_env;
            #[cfg(any(test, feature = "test-support"))]
            let env: Lookup = inputs.test_env.0.unwrap_or(&crate::paths::real_env);
            Ok(wisp_core::endpoints::Keychain::Absent(Variables::new(
                env,
                file,
                inputs.explicit_env_file,
            )))
        }
    }
}

pub const WAITING: &str = "Waiting for the keychain (unlock it if it asks)\u{2026}";

fn real_store(terminal: bool, notice: wisp_core::secrets::keyring::Notice) -> KeyringStore {
    let deadline = if terminal {
        TERMINAL_DEADLINE
    } else {
        HEADLESS_DEADLINE
    };
    let store = KeyringStore::new(wisp_core::secrets::keyring::platform::opener(), deadline);
    if terminal {
        store.with_notice(notice)
    } else {
        store
    }
}

#[cfg(test)]
mod tests;
