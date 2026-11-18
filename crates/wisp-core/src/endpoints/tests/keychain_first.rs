use super::*;
use crate::secrets::{KeyringError, KeyringRecords, Prefetched, Secret, Stored, StoredRecord};

const CANARIES: [&str; 7] = [
    "CANARYEXPORTED",
    "CANARYFILE",
    "CANARYEXPLICIT",
    "CANARYTABLE",
    "CANARYPASS",
    "CANARYPATH",
    "CANARYQUERY",
];

const KEYS_FILE: &str = "~/.config/wisp/.env";

const BODY: &str = "default = \"direct\"\n[endpoints.direct]\nauth = \"api_key\"\nkey_store = \"keyring\"\n[endpoints.work]\nauth = \"bearer\"\nkey_env = \"WORK_KEY\"\nurl_env = \"WORK_URL\"\n[endpoints.gw]\nauth = \"api_key\"\nkey_env = \"GW_KEY\"\nurl = \"https://gw.example.com\"\n";

fn canary_env() -> impl Fn(&str) -> Option<String> + Sync + use<> {
    env_of(&[
        ("ANTHROPIC_API_KEY", "sk-ant-CANARYEXPORTED"),
        (
            "ANTHROPIC_BASE_URL",
            "https://u:CANARYPASS@a.example.com/CANARYPATH?CANARYQUERY",
        ),
        ("WORK_KEY", "CANARYTABLE-work"),
        (
            "WORK_URL",
            "https://u:CANARYPASS@w.example.com/CANARYPATH?CANARYQUERY",
        ),
        ("GW_KEY", "sk-ant-CANARYTABLE-gw"),
    ])
}

fn canary_file() -> EnvFile {
    env_file(&[
        ("ANTHROPIC_AUTH_TOKEN", "CANARYFILE"),
        ("GW_KEY", "sk-ant-CANARYFILE-gw"),
    ])
}

fn keyring(entries: &[(&str, Result<Option<&str>, KeyringError>)]) -> Prefetched {
    let found: KeyringRecords = entries
        .iter()
        .map(|(n, result)| {
            let result = result.clone().map(|key| {
                key.map(|key| Stored {
                    record: StoredRecord::Key {
                        key: Secret::new(key),
                        url: None,
                    },
                    owner: None,
                })
            });
            (name(n), result)
        })
        .collect();
    Prefetched::default().with_keyring(found)
}

fn assert_no_canary(text: &str) {
    for canary in CANARIES {
        assert!(!text.contains(canary), "{canary} in {text}");
    }
}

#[test]
fn under_a_keychain_entries_build_nothing_from_any_variable() {
    let config = stored_config(BODY);
    let secrets = keyring(&[("direct", Ok(Some("sk-ant-stored-direct")))]);
    let env = canary_env();
    let file = canary_file();

    let absent = EndpointBook::entries(Some(&config), &secrets, &absent(&env, Some(&file), true));
    assert!(matches!(absent[&name("gateway")], Availability::Ready(_)));
    assert_eq!(
        absent[&EndpointName::env()],
        Availability::SameAs(name("gateway"))
    );
    assert!(matches!(absent[&name("work")], Availability::Ready(_)));
    assert!(matches!(absent[&name("gw")], Availability::Ready(_)));

    let entries = EndpointBook::entries(Some(&config), &secrets, &Keychain::Available(KEYS_FILE));
    assert_eq!(
        entries.keys().map(EndpointName::as_str).collect::<Vec<_>>(),
        ["direct", "gw", "work"]
    );
    assert_eq!(
        ready(&entries, "direct").credential,
        Credential::ApiKey("sk-ant-stored-direct".into())
    );
    assert_eq!(
        reason(&entries, "work"),
        "its key is in the variable WORK_KEY, which Wisp doesn't read on a machine with a keychain; wisp account import stores it"
    );
    assert_eq!(
        reason(&entries, "gw"),
        "its key is in the variable GW_KEY, which Wisp doesn't read on a machine with a keychain; wisp account import stores it"
    );
    let Availability::Unavailable(gw) = &entries[&name("gw")] else {
        panic!()
    };
    assert!(gw.secrets().is_empty());
    assert_eq!(gw.auth, Some(AuthKind::ApiKey));
    assert_eq!(gw.official, Some(false));
    assert_no_canary(&format!("{entries:?}"));

    let book = EndpointBook::resolve(
        Some(&config),
        &secrets,
        &Keychain::Available(KEYS_FILE),
        None,
    )
    .unwrap();
    assert_eq!(book.start, name("direct"));
    assert_eq!(book.start_reason, StartReason::Default);
    assert_eq!(book.implicit, None);
    assert_eq!(book.overridden, None);
    assert_no_canary(&format!("{book:?}"));
    let context = book.log_context();
    assert_eq!(
        context.redactions.scrub("sk-ant-stored-direct"),
        crate::session::REDACTED
    );
}

#[test]
fn the_env_endpoint_and_variable_defaults_are_refused_on_a_keychain_machine() {
    let elsewhere = "/srv/keys/.env";
    let unread_env = "/srv/keys/.env and the ANTHROPIC_* variables aren't read on a machine with a keychain; wisp account import --from-environment stores them";
    let flag = |n| {
        Some(Choice {
            name: n,
            reason: StartReason::Flag,
        })
    };
    for config in [None, Some(stored_config(BODY))] {
        let err = EndpointBook::resolve(
            config.as_ref(),
            &keyring(&[("direct", Ok(Some("k")))]),
            &Keychain::Available(elsewhere),
            flag("env"),
        )
        .unwrap_err();
        assert_eq!(
            err.to_string(),
            format!("the keys-file account is unavailable: {unread_env}")
        );
    }
    let env_default = stored_config(
        "default = \"env\"\n[endpoints.direct]\nauth = \"api_key\"\nkey_store = \"keyring\"\n",
    );
    let err = EndpointBook::resolve(
        Some(&env_default),
        &keyring(&[("direct", Ok(Some("k")))]),
        &Keychain::Available(elsewhere),
        None,
    )
    .unwrap_err();
    assert_eq!(
        err.to_string(),
        format!(
            "the keys-file account, where new chats start, is unavailable: {unread_env} (wisp account list shows your accounts)"
        )
    );
    let table_default = config(
        "default = \"work\"\n[endpoints.work]\nauth = \"api_key\"\nkey_env = \"WORK_KEY\"\n",
    );
    let err = EndpointBook::resolve(
        Some(&table_default),
        &NoSecrets,
        &Keychain::Available(KEYS_FILE),
        None,
    )
    .unwrap_err();
    assert_eq!(
        err.to_string(),
        "the default account \"work\" is unavailable: its key is in the variable WORK_KEY, which Wisp doesn't read on a machine with a keychain; wisp account import stores it (wisp account list shows your accounts)"
    );
}

#[test]
fn with_nothing_stored_a_keychain_machine_has_nothing_set_up() {
    for config in [
        None,
        Some(config("")),
        Some(config("[context]\ncontext_note = false\n")),
    ] {
        let err = EndpointBook::resolve(
            config.as_ref(),
            &NoSecrets,
            &Keychain::Available(KEYS_FILE),
            None,
        )
        .unwrap_err();
        assert!(matches!(err, EndpointError::NoneSetUp), "{err:?}");
        assert_eq!(err.to_string(), "no account is set up yet");
        assert!(
            EndpointBook::entries(config.as_ref(), &NoSecrets, &Keychain::Available(KEYS_FILE))
                .is_empty()
        );
    }
    let err = EndpointBook::resolve(
        None,
        &NoSecrets,
        &Keychain::Available(KEYS_FILE),
        Some(Choice {
            name: "direct",
            reason: StartReason::Variable,
        }),
    )
    .unwrap_err();
    assert_eq!(err.to_string(), "no account \"direct\" (available: none)");
}

#[test]
fn a_failing_keychain_never_makes_variables_a_source() {
    let config = stored_config(BODY);
    for failure in [
        KeyringError::Locked,
        KeyringError::TimedOut,
        KeyringError::NotRunning,
    ] {
        let secrets = keyring(&[("direct", Err(failure.clone()))]);
        let err = EndpointBook::resolve(
            Some(&config),
            &secrets,
            &Keychain::Available(KEYS_FILE),
            None,
        )
        .unwrap_err();
        let text = err.to_string();
        assert_eq!(
            text,
            format!(
                "the default account \"direct\" is unavailable: {failure} (wisp account list shows your accounts)"
            )
        );
        assert_no_canary(&text);
        let entries =
            EndpointBook::entries(Some(&config), &secrets, &Keychain::Available(KEYS_FILE));
        assert!(
            entries
                .values()
                .all(|a| !matches!(a, Availability::Ready(_)))
        );
        assert_no_canary(&format!("{entries:?}"));
    }
}

#[derive(Clone, Copy, Debug, PartialEq)]
enum Named {
    None,
    Direct,
    Second,
    Work,
    Env,
    Gone,
}

#[derive(Clone, Copy, Debug)]
struct KeyCase {
    flag: Named,
    variable: Named,
    default: Named,
    direct_ready: bool,
    second_ready: bool,
}

fn keychain_expected(c: KeyCase) -> Result<(&'static str, StartReason), String> {
    let ready = |n: Named| match n {
        Named::Direct => c.direct_ready,
        Named::Second => c.second_ready,
        _ => false,
    };
    let word = |n: Named| match n {
        Named::Direct => "direct",
        Named::Second => "second",
        Named::Work => "work",
        Named::Env => "env",
        Named::Gone => "gone",
        Named::None => "",
    };
    let chosen = match (c.flag, c.variable) {
        (Named::None, Named::None) => None,
        (Named::None, v) => Some((v, StartReason::Variable)),
        (f, _) => Some((f, StartReason::Flag)),
    };
    if let Some((pick, reason)) = chosen {
        return match pick {
            Named::Gone => Err("unknown gone".to_owned()),
            p if ready(p) => Ok((word(p), reason)),
            p => Err(format!("unavailable {}", word(p))),
        };
    }
    match c.default {
        Named::None => {}
        p if ready(p) => return Ok((word(p), StartReason::Default)),
        p => return Err(format!("default {}", word(p))),
    }
    match (c.direct_ready, c.second_ready) {
        (true, false) => Ok(("direct", StartReason::OnlyOne)),
        (false, true) => Ok(("second", StartReason::OnlyOne)),
        (true, true) => Err("not chosen".to_owned()),
        (false, false) => Err("none available".to_owned()),
    }
}

#[test]
fn the_keychain_precedence_holds_for_every_combination() {
    let picks = [
        Named::None,
        Named::Direct,
        Named::Second,
        Named::Work,
        Named::Env,
        Named::Gone,
    ];
    let defaults = [
        Named::None,
        Named::Direct,
        Named::Second,
        Named::Work,
        Named::Env,
    ];
    let mut checked = 0;
    for flag in picks {
        for variable in picks {
            for default in defaults {
                for bits in 0..4u8 {
                    keychain_check(KeyCase {
                        flag,
                        variable,
                        default,
                        direct_ready: bits & 1 != 0,
                        second_ready: bits & 2 != 0,
                    });
                    checked += 1;
                }
            }
        }
    }
    assert_eq!(checked, 720);
}

fn keychain_check(c: KeyCase) {
    let mut text = String::new();
    match c.default {
        Named::Direct => text.push_str("default = \"direct\"\n"),
        Named::Second => text.push_str("default = \"second\"\n"),
        Named::Work => text.push_str("default = \"work\"\n"),
        Named::Env => text.push_str("default = \"env\"\n"),
        _ => {}
    }
    text.push_str("[endpoints.direct]\nauth = \"api_key\"\nkey_store = \"keyring\"\n[endpoints.second]\nauth = \"api_key\"\nkey_store = \"file\"\n[endpoints.work]\nauth = \"api_key\"\nkey_env = \"WORK_KEY\"\n");
    let config = stored_config(&text);
    let mut file = vec![];
    if c.second_ready {
        file.push(("second", "sk-ant-second", None));
    }
    let direct: Result<Option<&str>, KeyringError> = if c.direct_ready {
        Ok(Some("sk-ant-direct"))
    } else {
        Err(KeyringError::Locked)
    };
    let secrets =
        records(&file).with_keyring(keyring(&[("direct", direct)]).keyring_records().clone());
    let name_of = |n: Named| match n {
        Named::None => None,
        Named::Direct => Some("direct"),
        Named::Second => Some("second"),
        Named::Work => Some("work"),
        Named::Env => Some("env"),
        Named::Gone => Some("gone"),
    };
    let choice = match (name_of(c.flag), name_of(c.variable)) {
        (Some(n), _) => Some(Choice {
            name: n,
            reason: StartReason::Flag,
        }),
        (None, Some(n)) => Some(Choice {
            name: n,
            reason: StartReason::Variable,
        }),
        (None, None) => None,
    };
    let got = EndpointBook::resolve(
        Some(&config),
        &secrets,
        &Keychain::Available(KEYS_FILE),
        choice,
    );
    match (keychain_expected(c), got) {
        (Ok((start, reason)), Ok(book)) => {
            assert_eq!(book.start.as_str(), start, "{c:?}");
            assert_eq!(book.start_reason, reason, "{c:?}");
            assert_eq!(book.overridden, None, "{c:?}");
            assert!(!book.entries.contains_key(&EndpointName::env()), "{c:?}");
        }
        (Err(kind), Err(e)) => {
            let text = e.to_string();
            let fits = match kind.as_str() {
                "unknown gone" => text.starts_with("no account \"gone\""),
                "unavailable env" => {
                    text.starts_with(
                        "the keys-file account is unavailable: ~/.config/wisp/.env and the ANTHROPIC_*",
                    )
                }
                "unavailable work" => text.starts_with(
                    "account \"work\" is unavailable: its key is in the variable WORK_KEY",
                ),
                "unavailable direct" => text.starts_with(
                    "account \"direct\" is unavailable: the keychain is locked",
                ),
                "unavailable second" => {
                    text.starts_with("account \"second\" is unavailable: no key for second")
                }
                "default second" => text.starts_with(
                    "the default account \"second\" is unavailable: no key for second",
                ),
                "default env" => text.starts_with(
                    "the keys-file account, where new chats start, is unavailable: ~/.config/wisp/.env and the ANTHROPIC_*",
                ),
                "default work" => text.starts_with(
                    "the default account \"work\" is unavailable: its key is in the variable WORK_KEY",
                ),
                "default direct" => text.starts_with(
                    "the default account \"direct\" is unavailable: the keychain is locked",
                ),
                "not chosen" => {
                    text == "no account chosen: use --account (available: direct, second)"
                }
                _ => text.starts_with("no account is available: direct: the keychain is locked"),
            };
            assert!(fits, "{c:?}: {kind}: {text}");
        }
        (want, got) => panic!("{c:?}: wanted {want:?}, got {got:?}"),
    }
}
