use std::cell::Cell;

use super::*;

struct FakeBus {
    answer: BusAnswer,
    asked: Cell<usize>,
}

impl FakeBus {
    fn new(answer: BusAnswer) -> Self {
        Self {
            answer,
            asked: Cell::new(0),
        }
    }
}

impl BusCheck for FakeBus {
    fn secret_service(&self) -> BusAnswer {
        self.asked.set(self.asked.get() + 1);
        self.answer
    }
}

fn choice(setting: KeychainSetting) -> KeychainChoice {
    KeychainChoice {
        setting,
        origin: Some(VARIABLE.to_owned()),
    }
}

fn inputs<'a>(platform: Platform, wsl: WslFacts, bus: &'a FakeBus) -> DetectionInputs<'a> {
    DetectionInputs {
        platform,
        wsl,
        bus,
        built: true,
    }
}

fn wsl() -> WslFacts {
    WslFacts {
        interop: true,
        ..WslFacts::default()
    }
}

#[test]
fn auto_on_linux_asks_the_bus_and_reads_each_answer() {
    let rows = [
        (
            BusAnswer::Owned,
            Detected::Available {
                backend: Backend::SecretService,
                down: None,
            },
        ),
        (
            BusAnswer::Activatable,
            Detected::Available {
                backend: Backend::SecretService,
                down: None,
            },
        ),
        (
            BusAnswer::Missing,
            Detected::Absent {
                why: NoKeychain::NoSecretService,
            },
        ),
        (
            BusAnswer::NoBus,
            Detected::Absent {
                why: NoKeychain::NoBus,
            },
        ),
        (
            BusAnswer::Silent,
            Detected::Available {
                backend: Backend::SecretService,
                down: Some(KeyringError::BusSilent),
            },
        ),
    ];
    for platform in [Platform::Linux, Platform::Bsd] {
        for (answer, expected) in rows.clone() {
            let bus = FakeBus::new(answer);
            let found = detect(
                &choice(KeychainSetting::Auto),
                &inputs(platform, WslFacts::default(), &bus),
            );
            assert_eq!(
                found.as_ref().unwrap().auto_store(),
                if expected.available() {
                    crate::secrets::StoreKind::Keyring
                } else {
                    crate::secrets::StoreKind::File
                }
            );
            assert_eq!(found, Ok(expected), "{platform:?} {answer:?}");
            assert_eq!(bus.asked.get(), 1);
        }
    }
}

#[test]
fn the_bus_is_asked_only_under_auto_on_linux_or_bsd_outside_wsl() {
    let cases = [
        (KeychainSetting::Auto, Platform::Linux, wsl()),
        (KeychainSetting::Auto, Platform::MacOs, WslFacts::default()),
        (
            KeychainSetting::Auto,
            Platform::Windows,
            WslFacts::default(),
        ),
        (KeychainSetting::Auto, Platform::Other, WslFacts::default()),
        (KeychainSetting::On, Platform::Linux, WslFacts::default()),
        (KeychainSetting::On, Platform::Linux, wsl()),
        (KeychainSetting::Off, Platform::Linux, WslFacts::default()),
    ];
    for (setting, platform, facts) in cases {
        let bus = FakeBus::new(BusAnswer::Owned);
        let _ = detect(&choice(setting), &inputs(platform, facts, &bus));
        assert_eq!(bus.asked.get(), 0, "{setting:?} {platform:?}");
    }
}

#[test]
fn the_platform_rows_of_the_detection_table() {
    let bus = FakeBus::new(BusAnswer::Missing);
    let auto = choice(KeychainSetting::Auto);
    assert_eq!(
        detect(&auto, &inputs(Platform::Linux, wsl(), &bus)),
        Ok(Detected::Absent {
            why: NoKeychain::Wsl
        })
    );
    assert_eq!(
        detect(&auto, &inputs(Platform::MacOs, WslFacts::default(), &bus)),
        Ok(Detected::Available {
            backend: Backend::MacKeychain,
            down: None
        })
    );
    assert_eq!(
        detect(&auto, &inputs(Platform::Windows, WslFacts::default(), &bus)),
        Ok(Detected::Available {
            backend: Backend::WindowsCredentials,
            down: None
        })
    );
    assert_eq!(
        detect(&auto, &inputs(Platform::Other, WslFacts::default(), &bus)),
        Ok(Detected::Absent {
            why: NoKeychain::Unsupported
        })
    );
    assert_eq!(
        detect(
            &choice(KeychainSetting::On),
            &inputs(Platform::Linux, wsl(), &bus)
        ),
        Ok(Detected::Available {
            backend: Backend::SecretService,
            down: None
        }),
        "on overrides the WSL rule"
    );
    assert_eq!(
        detect(
            &choice(KeychainSetting::Off),
            &inputs(Platform::MacOs, WslFacts::default(), &bus)
        ),
        Ok(Detected::Off {
            origin: VARIABLE.to_owned()
        })
    );
}

#[test]
fn on_without_the_feature_is_an_error_naming_where_on_came_from() {
    let bus = FakeBus::new(BusAnswer::Owned);
    let unbuilt = DetectionInputs {
        built: false,
        ..inputs(Platform::Linux, WslFacts::default(), &bus)
    };
    assert_eq!(
        detect(&choice(KeychainSetting::On), &unbuilt),
        Err(
            "keychain = \"on\" (WISP_KEYCHAIN) needs a Wisp built with the os-keyring feature"
                .to_owned()
        )
    );
    assert_eq!(
        detect(&choice(KeychainSetting::Auto), &unbuilt),
        Ok(Detected::Absent {
            why: NoKeychain::NotBuilt
        })
    );
    assert_eq!(bus.asked.get(), 0);
}

#[test]
fn wsl_is_read_from_the_kernel_files() {
    let root = tempfile::tempdir().unwrap();
    assert!(!WslFacts::read(root.path()).is_wsl());
    let binfmt = root.path().join("proc/sys/fs/binfmt_misc");
    let kernel = root.path().join("proc/sys/kernel");
    std::fs::create_dir_all(&binfmt).unwrap();
    std::fs::create_dir_all(&kernel).unwrap();
    std::fs::write(kernel.join("osrelease"), "6.8.0-45-generic\n").unwrap();
    assert!(!WslFacts::read(root.path()).is_wsl());
    std::fs::write(
        kernel.join("osrelease"),
        "6.6.36.6-Microsoft-standard-WSL2\n",
    )
    .unwrap();
    assert!(WslFacts::read(root.path()).is_wsl(), "osrelease, any case");
    std::fs::write(kernel.join("osrelease"), "6.8.0\n").unwrap();
    std::fs::write(binfmt.join("WSLInterop-late"), "enabled\n").unwrap();
    let facts = WslFacts::read(root.path());
    assert!(facts.interop_late && !facts.interop && facts.is_wsl());
    std::fs::remove_file(binfmt.join("WSLInterop-late")).unwrap();
    std::fs::write(binfmt.join("WSLInterop"), "enabled\n").unwrap();
    let facts = WslFacts::read(root.path());
    assert!(facts.interop && facts.is_wsl());
}

#[test]
fn the_flag_beats_the_variable_which_beats_the_config() {
    let flag = Some((KeychainSetting::Off, "--keychain".to_owned()));
    let config = Some((KeychainSetting::On, "config.toml".to_owned()));
    assert_eq!(
        KeychainChoice::decide(flag.clone(), Some("auto"), config.clone()),
        Ok(KeychainChoice {
            setting: KeychainSetting::Off,
            origin: Some("--keychain".to_owned())
        })
    );
    assert_eq!(
        KeychainChoice::decide(None, Some(" auto "), config.clone()),
        Ok(KeychainChoice {
            setting: KeychainSetting::Auto,
            origin: Some(VARIABLE.to_owned())
        })
    );
    assert_eq!(
        KeychainChoice::decide(None, Some(""), config.clone()),
        Ok(KeychainChoice {
            setting: KeychainSetting::On,
            origin: Some("config.toml".to_owned())
        })
    );
    assert_eq!(
        KeychainChoice::decide(None, None, None),
        Ok(KeychainChoice {
            setting: KeychainSetting::Auto,
            origin: None
        })
    );
    let bad = KeychainChoice::decide(None, Some("canary-0123456789"), config).unwrap_err();
    assert_eq!(
        bad,
        "WISP_KEYCHAIN must be auto, on or off (what was given isn't shown)"
    );
    assert!(
        KeychainChoice::decide(flag, Some("canary-0123456789"), None).is_ok(),
        "the flag wins before the variable is judged"
    );
}
