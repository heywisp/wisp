use std::path::Path;

use wisp_core::endpoints::{ConfigFile, PathShown};

use super::*;
use crate::resume::tests::sources_with;

const FRESH: Facts = Facts {
    nothing_found: true,
    nothing_left: false,
    given_source: false,
    chosen: false,
    terminals: true,
    output: Output::FullScreen,
};

const START_ACCOUNTS: [&str; 5] = [
    "store_id = \"0123456789abcdef\"\ndefault = \"claude-work\"\n[endpoints.claude-work]\nauth = \"anthropic_subscription\"\nkey_store = \"file\"\n",
    "default = \"claude-work\"\n[endpoints.claude-work]\nauth = \"anthropic_subscription\"\nkey_env = \"TEST_KEY\"\n",
    "default = \"env\"\n",
    "default = \"claude-work\"\n[endpoints.claude-work]\nauth = \"api_key\"\nkey_env = \"WORK_KEY\"\n",
    "default = \"g1\"\n[endpoints.g1]\nprovider = \"gemini\"\nauth = \"api_key\"\nkey_env = \"TEST_KEY\"\n[plugins.gemini]\nenabled = false\n",
];

const DIRECT: &str = "[endpoints.direct]\nauth = \"api_key\"\nkey_env = \"TEST_KEY\"\n";

#[test]
fn setup_opens_only_on_a_fresh_full_screen_start() {
    assert_eq!(opening(FRESH), Opening::Setup);
    assert_eq!(nothing_found(&sources_with("")), Some(true));
    let alone = START_ACCOUNTS.map(|start| crate::resume::nothing_left(&sources_with(start)));
    assert_eq!(
        alone.clone().map(|lead| lead.unwrap_or_default()),
        [
            "claude-work's key is missing.",
            "",
            "claude-api's key is missing from ~/.config/wisp/.env.",
            "claude-work's key is missing.",
            "",
        ]
    );
    for (start, lead) in START_ACCOUNTS.iter().zip(alone) {
        for (config, alone) in [
            (start.to_string(), true),
            (format!("{start}{DIRECT}"), false),
        ] {
            let sources = sources_with(&config);
            let left = crate::resume::nothing_left(&sources);
            assert_eq!(left, lead.clone().filter(|_| alone), "{config}");
            let facts = Facts {
                nothing_found: nothing_found(&sources).unwrap(),
                nothing_left: left.is_some(),
                ..FRESH
            };
            let fresh = if left.is_some() {
                Opening::Setup
            } else {
                Opening::Proceed
            };
            assert_eq!(opening(facts), fresh, "{config}");
            for script in [
                Facts {
                    terminals: false,
                    ..facts
                },
                Facts {
                    given_source: true,
                    ..facts
                },
            ] {
                assert_eq!(opening(script), Opening::Proceed, "{config}");
            }
        }
    }
}

#[test]
fn a_found_endpoint_or_key_never_opens_setup() {
    let found = Facts {
        nothing_found: false,
        ..FRESH
    };
    assert_eq!(opening(found), Opening::Proceed);
    assert_eq!(
        opening(Facts {
            output: Output::Jsonl,
            terminals: false,
            ..found
        }),
        Opening::Proceed
    );
}

#[test]
fn a_chosen_endpoint_keeps_its_own_error() {
    for terminals in [true, false] {
        let facts = Facts {
            chosen: true,
            terminals,
            ..FRESH
        };
        assert_eq!(opening(facts), Opening::Proceed);
    }
}

#[test]
fn an_output_mode_that_fails_reports_that_first() {
    let facts = Facts {
        output: Output::Refused,
        terminals: false,
        ..FRESH
    };
    assert_eq!(opening(facts), Opening::Proceed);
}

#[test]
fn scripted_use_gets_the_hint_instead_of_questions() {
    for facts in [
        Facts {
            given_source: true,
            ..FRESH
        },
        Facts {
            terminals: false,
            ..FRESH
        },
        Facts {
            output: Output::Jsonl,
            ..FRESH
        },
    ] {
        assert_eq!(opening(facts), Opening::Hint, "{facts:?}");
    }
    let script = |start: &str| {
        let selected =
            crate::resume::start_gone(&sources_with(&format!("{start}{DIRECT}")), false)?;
        Some(match selected.settle(None, false) {
            Ok(crate::resume::Settled::Go { name, left }) => left?.continuing(&name),
            Ok(crate::resume::Settled::Ask(_)) => panic!("a script was asked"),
            Err(line) => line,
        })
    };
    let lines: Vec<_> = START_ACCOUNTS.iter().map(|start| script(start)).collect();
    let [
        Some(signed_out),
        None,
        Some(keys_file),
        Some(unusable),
        Some(off),
    ] = &lines[..]
    else {
        panic!("{lines:?}");
    };
    assert!(
        signed_out.starts_with("wisp: claude-work, where new chats start, can't be used now (")
            && signed_out.ends_with("); rerun with --account direct"),
        "{signed_out}"
    );
    assert_eq!(
        keys_file,
        "wisp: the keys-file account, where new chats start, can't be used now; this chat starts on direct."
    );
    assert_eq!(
        unusable,
        "wisp: claude-work, where new chats start, can't be used now (WORK_KEY is not set in the environment); this chat starts on direct."
    );
    assert_eq!(
        off,
        "wisp: g1, where new chats start, can't be used now (gemini is off); this chat starts on direct."
    );
}

fn parse(text: &str) -> ConfigFile {
    ConfigFile::parse(
        Path::new("/home/u/.config/wisp/config.toml"),
        PathShown::Default,
        text,
    )
    .unwrap()
}

#[test]
fn a_config_counts_as_configured_once_it_names_an_endpoint_or_a_default() {
    assert!(unconfigured(None));
    assert!(unconfigured(Some(&parse(""))));
    assert!(unconfigured(Some(&parse(
        "model = \"claude-sonnet-5-5\"\n"
    ))));
    assert!(!unconfigured(Some(&parse("default = \"env\"\n"))));
    assert!(!unconfigured(Some(&parse(
        "[endpoints.work]\nauth = \"bearer\"\nkey_env = \"WORK_TOKEN\"\nurl = \"https://gw.example.com\"\n"
    ))));
}

#[test]
fn an_unconfirmed_keychain_write_stops_the_offer_before_the_file_pass() {
    use std::cell::RefCell;
    use wisp_core::config_file::NewEndpoint;
    use wisp_core::endpoints::{AuthKind, EndpointName, PricingMode, ProviderKind};
    use wisp_core::secrets::keyring::{BackendError, MemoryKeyring, MemoryMode};
    use wisp_core::secrets::{Change, Secret, StoreKind, StoredRecord};

    use crate::manage::{Picked, Saving};

    let dir = crate::scratch_dir("first-run-store");
    let path = dir.root.join("config.toml");
    std::fs::write(&path, "store_id = \"0123456789abcdef\"\n").unwrap();
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
    let key = Secret::new("sk-ant-CANARYSAMEKEYBOTHab12");
    let make = |store: StoreKind| Change::Add {
        name: EndpointName::parse("direct").unwrap(),
        endpoint: NewEndpoint {
            provider: ProviderKind::Anthropic,
            auth: AuthKind::ApiKey,
            store,
            pricing: PricingMode::List,
        },
        record: StoredRecord::Key {
            key: key.clone(),
            url: None,
        },
        make_default: false,
    };
    let passes = RefCell::new(Vec::new());
    let mut pass = |place: Place, run: &mut ImportRun| {
        passes.borrow_mut().push(place);
        match run.save(&saving, &Picked::Auto, &make, &key) {
            Ok(_) => ExitCode::SUCCESS,
            Err(code) => code,
        }
    };
    let mut run = ImportRun::default();
    let stopped = offer_passes(&mut run, &|_| true, &mut pass);
    assert_eq!(stopped, Some((ExitCode::FAILURE, Place::Environment)));
    assert_eq!(*passes.borrow(), [Place::Environment]);
    assert!(run.stopped());
    assert!(!run.stored_any());
    assert!(!dir.root.join("credentials.json").exists());
}

#[test]
fn a_stopped_import_names_what_was_left_and_the_next_step() {
    let file = "~/.config/wisp/.env";
    let both = |_: Place| true;
    let env_only = |place: Place| place == Place::Environment;
    assert_eq!(
        stopped_words(Place::Environment, &both, file),
        "Setup stopped: the rest of your environment's keys and the keys in ~/.config/wisp/.env weren't imported. Run wisp setup to try again."
    );
    assert_eq!(
        stopped_words(Place::Environment, &env_only, file),
        "Setup stopped: the rest of your environment's keys weren't imported. Run wisp setup to try again."
    );
    assert_eq!(
        stopped_words(Place::File, &both, file),
        "Setup stopped: the rest of the keys in ~/.config/wisp/.env weren't imported. Run wisp setup to try again."
    );
}
