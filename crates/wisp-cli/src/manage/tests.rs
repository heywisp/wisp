use wisp_core::secrets::DetachNote;

use super::*;

fn name(text: &str) -> EndpointName {
    EndpointName::parse(text).unwrap()
}

#[test]
fn names_follow_the_rule_and_env_is_reserved() {
    assert_eq!(parse_name("work-1").unwrap(), name("work-1"));
    for bad in ["", "Work", "sk-ant-CANARY", "a b", &"x".repeat(33)] {
        assert_eq!(
            parse_name(bad).unwrap_err(),
            wisp_protocol::bad_account_name(),
            "{bad}"
        );
    }
    assert!(parse_name("env").unwrap_err().contains("reserved"));
}

#[test]
fn a_suggested_name_comes_from_the_host_and_never_clashes() {
    let none = BTreeSet::new();
    assert_eq!(
        suggested_name(ProviderKind::Anthropic, None, &none),
        name("anthropic")
    );
    assert_eq!(
        suggested_name(
            ProviderKind::Anthropic,
            Some("https://Gw.Example.com/v1"),
            &none
        ),
        name("gw")
    );
    assert_eq!(
        suggested_name(
            ProviderKind::Anthropic,
            Some("http://127.0.0.1:8080"),
            &none
        ),
        name("127")
    );
    assert_eq!(
        suggested_name(ProviderKind::Anthropic, Some("http://[::1]:8080"), &none),
        name("gateway")
    );
    assert_eq!(
        suggested_name(ProviderKind::Anthropic, Some("http://env.example"), &none),
        name("gateway")
    );
    let taken: BTreeSet<EndpointName> = [name("anthropic"), name("anthropic-2")].into();
    assert_eq!(
        suggested_name(ProviderKind::Anthropic, None, &taken),
        name("anthropic-3")
    );
}

#[test]
fn gemini_default_suggested_and_api_names_use_gemini_api() {
    let none = BTreeSet::new();
    assert_eq!(api_name(ProviderKind::Gemini), wisp_protocol::GEMINI_API);
    assert_eq!(
        suggested_name(ProviderKind::Gemini, None, &none),
        name("gemini-api")
    );
    let taken: BTreeSet<EndpointName> = [name("gemini-api")].into();
    assert_eq!(
        suggested_name(ProviderKind::Gemini, None, &taken),
        name("gemini-api-2")
    );
    assert_eq!(
        suggested_name(
            ProviderKind::Gemini,
            Some("https://gemini-gw.example.com/v1beta"),
            &none,
        ),
        name("gemini-gw")
    );
}

#[test]
fn enter_takes_the_opening_row_only_where_one_is_offered() {
    assert_eq!(choose_prompt(3, Some(0)), "Choose [1]: ");
    assert_eq!(choose_prompt(2, Some(1)), "Choose [2]: ");
    assert_eq!(choose_prompt(1, None), "Choose 1: ");
    assert_eq!(choose_prompt(2, None), "Choose 1-2: ");
    assert_eq!(picked("", 2, Some(1)), Some(1));
    assert_eq!(picked("", 2, None), None);
    assert_eq!(picked("2", 2, None), Some(1));
    assert_eq!(picked("3", 2, Some(0)), None);
    assert_eq!(picked("0", 2, Some(0)), None);
    assert_eq!(picked("one", 2, Some(0)), None);
}

#[test]
fn plain_http_is_warned_about_except_on_loopback() {
    assert_eq!(
        plain_http_host("http://gw.example.com").as_deref(),
        Some("gw.example.com")
    );
    assert_eq!(plain_http_host("https://gw.example.com"), None);
    assert_eq!(plain_http_host("http://localhost:4000"), None);
}

fn outcome_of(outcome: Result<CheckOutcome, String>) -> Asked<Checked> {
    after_check(outcome, None, true)
}

#[test]
fn without_a_terminal_each_check_outcome_saves_or_stops() {
    assert!(matches!(
        outcome_of(Ok(CheckOutcome::Ok)),
        Asked::Answer(Checked::Save(None))
    ));
    assert!(matches!(
        outcome_of(Ok(CheckOutcome::Forbidden)),
        Asked::Answer(Checked::Save(Some(FORBIDDEN_NOTE)))
    ));
    assert!(matches!(
        outcome_of(Ok(CheckOutcome::NotOffered)),
        Asked::Answer(Checked::Save(Some(NOT_OFFERED_NOTE)))
    ));
    match outcome_of(Ok(CheckOutcome::KeyRefused)) {
        Asked::Answer(Checked::Stop(why)) => {
            assert_eq!(why, "the key was refused (401), so nothing was saved");
        }
        _ => panic!("a refused key saved"),
    }
    match outcome_of(Err("connection refused".to_owned())) {
        Asked::Answer(Checked::Stop(why)) => assert_eq!(
            why,
            "the check failed (not checked: connection refused), so nothing was saved; --no-check saves without it"
        ),
        _ => panic!("a failed check saved"),
    }
}

fn owners<'a>(default: Option<&Path>, shown: Option<&'a Path>) -> Owners<'a> {
    Owners {
        this: Some(PathBuf::from("/h/copy/config.toml")),
        default: default.map(Path::to_path_buf),
        default_shown: shown,
        home: Some("/h"),
    }
}

#[test]
fn each_detach_note_names_where_its_keys_came_from() {
    let default = Path::new("/h/.config/wisp/config.toml");
    let other = PathBuf::from("/h/elsewhere/config.toml");
    let words = |from: Vec<Option<PathBuf>>, default_path: Option<&Path>| {
        owners(default_path, Some(default)).detach_words(&DetachNote { from })
    };
    let lead = "This config.toml now keeps its own copy of its keys";
    let d = || Some(default.to_path_buf());
    assert_eq!(
        words(vec![d()], Some(default)),
        format!("{lead} (they were written by ~/.config/wisp/config.toml).")
    );
    assert_eq!(
        words(vec![d(), d()], Some(default)),
        format!("{lead} (they were written by ~/.config/wisp/config.toml).")
    );
    assert_eq!(
        words(vec![d(), Some(other.clone())], Some(default)),
        format!("{lead} (they were written by another config file).")
    );
    assert_eq!(
        words(vec![None, None], Some(default)),
        format!("{lead} (they were written by hand).")
    );
    assert_eq!(
        words(vec![None, d()], Some(default)),
        format!(
            "{lead} (they were written by ~/.config/wisp/config.toml) (some were written by hand)."
        )
    );
    assert_eq!(
        words(vec![None, Some(other)], Some(default)),
        format!("{lead} (they were written by another config file) (some were written by hand).")
    );
    assert_eq!(
        words(vec![d()], None),
        format!("{lead} (they were written by another config file).")
    );
    assert_eq!(
        owners(Some(default), None).detach_words(&DetachNote { from: vec![d()] }),
        format!("{lead} (they were written by another config file).")
    );
}

#[test]
fn a_refused_detach_names_the_file_without_an_absolute_path() {
    let error = ChangeError::CantDetach {
        unreadable: vec![name("direct")],
        credentials: PathBuf::from("/h/.config/wisp/credentials.json"),
        shown: PathShown::Default,
    };
    let words = change_error(&error, Some("/h"));
    assert!(
        words.contains(
            "the key of direct can't be read (it isn't in ~/.config/wisp/credentials.json)"
        ),
        "{words}"
    );
    assert!(words.ends_with("to give it a new key."), "{words}");
    assert!(!words.contains("/h/"), "{words}");
    let error = ChangeError::CantDetach {
        unreadable: vec![name("a"), name("b")],
        credentials: PathBuf::from("/h/CANARY/credentials.json"),
        shown: PathShown::Given,
    };
    let words = change_error(&error, Some("/h"));
    assert!(
        words.contains("the keys of a and b can't be read (they aren't in credentials.json beside the file given with --config)"),
        "{words}"
    );
    assert!(
        !words.contains("CANARY") && words.ends_with("here first."),
        "{words}"
    );
}

#[test]
fn an_unwritable_target_names_the_link_and_its_target_only_for_the_default_config() {
    use wisp_core::config_file::{ConfigWriteError, WriteProblem};
    use wisp_core::secrets::{PathNote, PathNoteKind};
    let error = |shown| {
        ChangeError::Config(ConfigWriteError {
            path: PathBuf::from("/h/.config/wisp/config.toml"),
            shown,
            problem: WriteProblem::TargetNotWritable(Box::new(PathNote {
                kind: PathNoteKind::TargetNotWritable,
                path: PathBuf::from("/h/CANARY/config.toml"),
                repo: None,
                shown,
            })),
        })
    };
    assert_eq!(
        change_error(&error(PathShown::Default), Some("/h")),
        "~/.config/wisp/config.toml \u{2192} ~/CANARY/config.toml: Wisp can't write that folder; nothing was changed"
    );
    assert_eq!(
        change_error(&error(PathShown::Given), Some("/h")),
        "the file given with --config (a link) points into a folder Wisp can't write; nothing was changed"
    );
}

#[test]
fn an_unconfirmed_keychain_write_never_offers_the_file_and_names_the_account() {
    use std::cell::Cell;
    use wisp_core::secrets::keyring::{BackendError, MemoryKeyring, MemoryMode};
    let dir = crate::scratch_dir("manage-store");
    let path = dir.root.join("config.toml");
    let before = "store_id = \"0123456789abcdef\"\n\n[endpoints.work]\nauth = \"api_key\"\nkey_env = \"WORK_KEY\"\n";
    std::fs::write(&path, before).unwrap();
    let memory = MemoryKeyring::new();
    memory.set_mode(MemoryMode::ReadsFail(BackendError::Invalid));
    let keys = memory.store(std::time::Duration::from_secs(30));
    let saving = Saving {
        path: &path,
        shown: PathShown::Given,
        home: None,
        keys: &keys,
        prompter: None,
        many: false,
        saved_before: 0,
    };
    let make = |store: StoreKind| Change::Add {
        name: name("direct"),
        endpoint: NewEndpoint {
            provider: ProviderKind::Anthropic,
            auth: AuthKind::ApiKey,
            store,
            pricing: PricingMode::List,
        },
        record: StoredRecord::Key {
            key: Secret::new("sk-ant-CANARYUNCONFIRMEDab12"),
            url: None,
        },
        make_default: false,
    };
    let asked = Cell::new(false);
    let yes = |_: &str| {
        asked.set(true);
        Some(Asked::Answer(AddAnswer::Yes))
    };
    let saved = save_stored_asking(&saving, &Picked::Auto, &mut Fallback::Ask, &make, &yes);
    assert!(saved.is_err());
    assert!(!asked.get(), "the credentials.json question was offered");
    assert!(!dir.root.join("credentials.json").exists());
    assert_eq!(std::fs::read_to_string(&path).unwrap(), before);
    assert_eq!(
        memory.items().keys().cloned().collect::<Vec<_>>(),
        vec!["0123456789abcdef/direct".to_owned()],
        "the item that may hold the key is left where the message says"
    );
    let failed = try_change(&path, PathShown::Given, make(StoreKind::Keyring), &keys).unwrap_err();
    let words = change_error(&failed.error, None);
    assert!(
        words.contains("may have been saved there (account 0123456789abcdef/direct)"),
        "{words}"
    );
    assert!(!words.contains("CANARY"), "{words}");
}

#[test]
fn the_endpoint_flags_print_the_registry_words() {
    use clap::CommandFactory;
    use wisp_core::endpoints::KeyFrom;
    use wisp_protocol::settings::{Applies, SETTINGS, Secrecy, Site};

    let login = crate::Args::command();
    let login = login.find_subcommand("login").unwrap();
    let store = login
        .get_arguments()
        .find(|a| a.get_id() == "store")
        .unwrap();
    let words: BTreeSet<String> = store
        .get_possible_values()
        .iter()
        .map(|v| v.get_name().to_owned())
        .collect();
    assert_eq!(
        words,
        StoreChoice::ALL
            .map(|c| c.as_str().to_owned())
            .into_iter()
            .collect::<BTreeSet<_>>()
    );

    let read = |field: &str, word: &str| {
        let text = format!(
            "store_id = \"0000000000000000\"\n\n[endpoints.e]\nauth = \"api_key\"\n{field} = \"{word}\"\n{}",
            if field == "key_store" {
                ""
            } else {
                "key_env = \"SYNTHETIC_KEY\"\n"
            }
        );
        let file = ConfigFile::parse(Path::new("config.toml"), PathShown::Default, &text).unwrap();
        file.endpoints[&name("e")].clone()
    };
    for mode in PricingMode::ALL {
        assert_eq!(read("pricing", mode.as_str()).pricing, mode);
    }
    for kind in StoreKind::ALL {
        assert_eq!(read("key_store", kind.as_str()).key, KeyFrom::Store(kind));
    }

    #[derive(clap::Parser)]
    struct One {
        #[command(flatten)]
        args: edit::EditArgs,
    }
    let edit = One::command();
    for spec in SETTINGS.iter().filter(|s| {
        s.is_built()
            && s.has_site(Site::Endpoint)
            && s.applies == Applies::Connection
            && s.secrecy == Secrecy::Plain
            && s.name != "key_store"
    }) {
        let flag = spec.leaf().replace('_', "-");
        assert!(
            edit.get_arguments()
                .any(|a| a.get_long() == Some(flag.as_str())),
            "--{flag}"
        );
    }
}
