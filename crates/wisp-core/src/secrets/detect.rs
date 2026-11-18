use std::fmt;
use std::path::Path;
use std::time::Duration;

use super::keyring::KeyringError;

pub const BUS_LIMIT: Duration = Duration::from_secs(2);
pub const SECRETS_NAME: &str = "org.freedesktop.secrets";
pub const VARIABLE: &str = "WISP_KEYCHAIN";

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum KeychainSetting {
    Auto,
    On,
    Off,
}

impl KeychainSetting {
    pub const ALL: [Self; 3] = [Self::Auto, Self::On, Self::Off];

    pub fn as_str(self) -> &'static str {
        match self {
            Self::Auto => "auto",
            Self::On => "on",
            Self::Off => "off",
        }
    }

    pub fn parse(text: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|s| s.as_str() == text.trim())
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct KeychainChoice {
    pub setting: KeychainSetting,
    pub origin: Option<String>,
}

impl KeychainChoice {
    pub fn decide(
        flag: Option<(KeychainSetting, String)>,
        variable: Option<&str>,
        config: Option<(KeychainSetting, String)>,
    ) -> Result<Self, String> {
        if let Some((setting, origin)) = flag {
            return Ok(Self {
                setting,
                origin: Some(origin),
            });
        }
        if let Some(text) = variable.filter(|t| !t.trim().is_empty()) {
            return match KeychainSetting::parse(text) {
                Some(setting) => Ok(Self {
                    setting,
                    origin: Some(VARIABLE.to_owned()),
                }),
                None => Err(format!(
                    "{VARIABLE} must be auto, on or off (what was given isn't shown)"
                )),
            };
        }
        Ok(match config {
            Some((setting, origin)) => Self {
                setting,
                origin: Some(origin),
            },
            None => Self {
                setting: KeychainSetting::Auto,
                origin: None,
            },
        })
    }

    pub fn origin_words(&self) -> &str {
        self.origin.as_deref().unwrap_or("the default")
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Platform {
    Linux,
    Bsd,
    MacOs,
    Windows,
    Other,
}

impl Platform {
    pub fn current() -> Self {
        if cfg!(target_os = "linux") {
            Self::Linux
        } else if cfg!(any(
            target_os = "freebsd",
            target_os = "openbsd",
            target_os = "netbsd",
            target_os = "dragonfly"
        )) {
            Self::Bsd
        } else if cfg!(target_os = "macos") {
            Self::MacOs
        } else if cfg!(windows) {
            Self::Windows
        } else {
            Self::Other
        }
    }
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct WslFacts {
    pub interop: bool,
    pub interop_late: bool,
    pub osrelease: Option<String>,
}

impl WslFacts {
    pub fn read(root: &Path) -> Self {
        let binfmt = root.join("proc/sys/fs/binfmt_misc");
        Self {
            interop: std::fs::symlink_metadata(binfmt.join("WSLInterop")).is_ok(),
            interop_late: std::fs::symlink_metadata(binfmt.join("WSLInterop-late")).is_ok(),
            osrelease: std::fs::read_to_string(root.join("proc/sys/kernel/osrelease")).ok(),
        }
    }

    pub fn is_wsl(&self) -> bool {
        self.interop
            || self.interop_late
            || self
                .osrelease
                .as_deref()
                .is_some_and(|r| r.to_ascii_lowercase().contains("microsoft"))
    }
}

pub fn wsl2_release(release: &str) -> bool {
    release.to_ascii_lowercase().contains("wsl2")
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BusAnswer {
    Owned,
    Activatable,
    Missing,
    NoBus,
    Silent,
}

pub trait BusCheck {
    fn secret_service(&self) -> BusAnswer;
}

pub struct DetectionInputs<'a> {
    pub platform: Platform,
    pub wsl: WslFacts,
    pub bus: &'a dyn BusCheck,
    pub built: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Backend {
    SecretService,
    MacKeychain,
    WindowsCredentials,
}

impl Backend {
    pub fn words(self) -> &'static str {
        match self {
            Self::SecretService => "Secret Service",
            Self::MacKeychain => "macOS Keychain",
            Self::WindowsCredentials => "Windows Credential Manager",
        }
    }

    fn of(platform: Platform) -> Option<Self> {
        match platform {
            Platform::Linux | Platform::Bsd => Some(Self::SecretService),
            Platform::MacOs => Some(Self::MacKeychain),
            Platform::Windows => Some(Self::WindowsCredentials),
            Platform::Other => None,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum NoKeychain {
    Wsl,
    NoSecretService,
    NoBus,
    NotBuilt,
    Unsupported,
}

impl NoKeychain {
    pub fn words(self) -> &'static str {
        match self {
            Self::Wsl => "WSL",
            Self::NoSecretService => "no Secret Service on the session bus",
            Self::NoBus => "no session bus",
            Self::NotBuilt => "this Wisp was built without os-keyring",
            Self::Unsupported => "no keychain Wisp supports on this platform",
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Detected {
    Off {
        origin: String,
    },
    Available {
        backend: Backend,
        down: Option<KeyringError>,
    },
    Absent {
        why: NoKeychain,
    },
}

impl Detected {
    pub fn auto_store(&self) -> super::StoreKind {
        if self.available() {
            super::StoreKind::Keyring
        } else {
            super::StoreKind::File
        }
    }

    pub fn available(&self) -> bool {
        matches!(self, Self::Available { .. })
    }

    pub fn off_error(&self) -> Option<KeyringError> {
        match self {
            Self::Off { origin } => Some(KeyringError::Off {
                origin: origin.clone(),
            }),
            _ => None,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct NeedsFeature {
    pub origin: String,
}

impl fmt::Display for NeedsFeature {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "keychain = \"on\" ({}) needs a Wisp built with the os-keyring feature",
            self.origin
        )
    }
}

pub fn detect(choice: &KeychainChoice, inputs: &DetectionInputs<'_>) -> Result<Detected, String> {
    match choice.setting {
        KeychainSetting::Off => Ok(Detected::Off {
            origin: choice.origin_words().to_owned(),
        }),
        KeychainSetting::On => {
            if !inputs.built {
                return Err(NeedsFeature {
                    origin: choice.origin_words().to_owned(),
                }
                .to_string());
            }
            match Backend::of(inputs.platform) {
                Some(backend) => Ok(Detected::Available {
                    backend,
                    down: None,
                }),
                None => Err(format!(
                    "keychain = \"on\" ({}) needs a platform with a keychain Wisp supports",
                    choice.origin_words()
                )),
            }
        }
        KeychainSetting::Auto => Ok(auto(inputs)),
    }
}

fn auto(inputs: &DetectionInputs<'_>) -> Detected {
    if !inputs.built {
        return Detected::Absent {
            why: NoKeychain::NotBuilt,
        };
    }
    match inputs.platform {
        Platform::Linux if inputs.wsl.is_wsl() => Detected::Absent {
            why: NoKeychain::Wsl,
        },
        Platform::Linux | Platform::Bsd => match inputs.bus.secret_service() {
            BusAnswer::Owned | BusAnswer::Activatable => Detected::Available {
                backend: Backend::SecretService,
                down: None,
            },
            BusAnswer::Missing => Detected::Absent {
                why: NoKeychain::NoSecretService,
            },
            BusAnswer::NoBus => Detected::Absent {
                why: NoKeychain::NoBus,
            },
            BusAnswer::Silent => Detected::Available {
                backend: Backend::SecretService,
                down: Some(KeyringError::BusSilent),
            },
        },
        Platform::MacOs => Detected::Available {
            backend: Backend::MacKeychain,
            down: None,
        },
        Platform::Windows => Detected::Available {
            backend: Backend::WindowsCredentials,
            down: None,
        },
        Platform::Other => Detected::Absent {
            why: NoKeychain::Unsupported,
        },
    }
}

#[cfg(test)]
mod tests;
