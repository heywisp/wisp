use std::collections::BTreeSet;
use std::path::Path;

use wisp_core::endpoints::{ConfigFile, PathShown};

use super::*;
use crate::keychain::KeychainInputs;

const FILE: &str = "~/.config/wisp/.env";
const OFF: &str = "\nFor one run without the keychain: --keychain off";

fn names(list: &[&str]) -> BTreeSet<VariableName> {
    list.iter()
        .map(|n| VariableName::parse(n).unwrap())
        .collect()
}

fn config(text: &str) -> ConfigFile {
    ConfigFile::parse(
        Path::new("/home/u/.config/wisp/config.toml"),
        PathShown::Default,
        text,
    )
    .unwrap()
}

const TABLES: &str = "[endpoints.manifest]\nauth = \"bearer\"\nkey_env = \"MANIFEST_API_KEY\"\nurl_env = \"MANIFEST_BASE_URL\"\n";

#[test]
fn the_scan_returns_names_and_never_a_value() {
    let dir = crate::scratch_dir("presence");
    let path = dir.root.join(".env");
    std::fs::write(
        &path,
        "ANTHROPIC_API_KEY=sk-ant-CANARYFILEKEY\nsk-ant-CANARYPASTEDALONE000000\nUNASKED_KEY=CANARYUNASKED\nMANIFEST_API_KEY = CANARYTABLE\n",
    )
    .unwrap();
    let inputs = KeychainInputs::load(true, Some(path), false, None);
    let config = config(TABLES);
    let presence = Presence::scan(&inputs, Some(&config));
    assert_eq!(
        presence.file,
        names(&["ANTHROPIC_API_KEY", "MANIFEST_API_KEY"])
    );
    let shown = format!("{presence:?} {inputs:?}");
    assert!(!shown.contains("CANARY"), "{shown}");
    assert!(shown.contains("EnvText(<"), "{shown}");
    assert!(crate::keychain::keychain(&inputs).is_ok());
}

#[test]
fn a_variable_that_is_not_exported_is_not_present() {
    let asked = names(&["WISP_PRESENCE_TEST_NEVER_SET_7F3A"]);
    assert!(exported(&asked).is_empty());
    assert!(!is_set("WISP_PRESENCE_TEST_NEVER_SET_7F3A"));
}

#[test]
fn asked_names_cover_the_provider_and_every_table() {
    let config = config(TABLES);
    assert_eq!(
        asked(Some(&config)),
        names(&[
            "ANTHROPIC_API_KEY",
            "ANTHROPIC_AUTH_TOKEN",
            "ANTHROPIC_BASE_URL",
            "GEMINI_API_KEY",
            "GEMINI_BASE_URL",
            "MANIFEST_API_KEY",
            "MANIFEST_BASE_URL",
            "OPENAI_API_KEY",
            "OPENAI_BASE_URL",
        ])
    );
}

#[test]
fn each_message_is_chosen_by_what_is_present() {
    let tables = config(TABLES);
    let message = |env: &[&str], file: &[&str], config: Option<&ConfigFile>| -> Option<String> {
        Presence::of(names(env), names(file), config).unread_message(FILE)
    };
    assert_eq!(
        message(&[], &["ANTHROPIC_API_KEY"], None).unwrap(),
        format!(
            "no account is set up yet. {FILE} holds ANTHROPIC_API_KEY, but this machine has a keychain, so Wisp doesn't read it. Store it once with: wisp account import{OFF}"
        )
    );
    assert_eq!(
        message(
            &["ANTHROPIC_API_KEY"],
            &["ANTHROPIC_API_KEY", "MANIFEST_API_KEY"],
            Some(&tables)
        )
        .unwrap(),
        format!(
            "no account is set up yet. {FILE} holds ANTHROPIC_API_KEY and the keys config.toml names, but this machine has a keychain, so Wisp doesn't read them. Store them once with: wisp account import{OFF}"
        )
    );
    assert_eq!(
        message(&[], &["MANIFEST_API_KEY"], Some(&tables)).unwrap(),
        format!(
            "no account is set up yet. {FILE} holds the keys config.toml names, but this machine has a keychain, so Wisp doesn't read them. Store them once with: wisp account import{OFF}"
        )
    );
    assert_eq!(
        message(&["ANTHROPIC_API_KEY"], &[], None).unwrap(),
        format!(
            "no account is set up yet. ANTHROPIC_API_KEY is set, but this machine has a keychain, so Wisp doesn't read it. Store it once with: wisp account import --from-environment{OFF}"
        )
    );
    assert_eq!(
        message(
            &["ANTHROPIC_API_KEY", "ANTHROPIC_AUTH_TOKEN"],
            &["ANTHROPIC_BASE_URL"],
            None
        )
        .unwrap(),
        format!(
            "no account is set up yet. ANTHROPIC_API_KEY and ANTHROPIC_AUTH_TOKEN are set, but this machine has a keychain, so Wisp doesn't read them. Store them once with: wisp account import --from-environment{OFF}"
        )
    );
    assert_eq!(
        message(&["MANIFEST_API_KEY"], &[], Some(&tables)).unwrap(),
        format!(
            "no account is set up yet. config.toml's accounts name variables, which Wisp doesn't read on a machine with a keychain. Store their keys once with: wisp account import --from-environment{OFF}"
        )
    );
    assert_eq!(
        message(&[], &[], Some(&tables)).unwrap(),
        format!(
            "no account is set up yet. config.toml's accounts name variables, which Wisp doesn't read on a machine with a keychain. Store their keys once with: wisp account import --env-file FILE{OFF}"
        )
    );
    assert_eq!(
        message(&["ANTHROPIC_BASE_URL"], &["ANTHROPIC_BASE_URL"], None),
        None
    );
    assert_eq!(message(&[], &[], None), None);
}

#[test]
fn the_offer_lists_names_and_places_only() {
    let tables = config(TABLES);
    let presence = Presence::of(
        names(&["ANTHROPIC_API_KEY"]),
        names(&[
            "ANTHROPIC_AUTH_TOKEN",
            "ANTHROPIC_BASE_URL",
            "MANIFEST_API_KEY",
        ]),
        Some(&tables),
    );
    assert_eq!(
        presence.offer_lines(FILE),
        [
            "ANTHROPIC_API_KEY in your environment".to_owned(),
            format!("ANTHROPIC_AUTH_TOKEN and ANTHROPIC_BASE_URL in {FILE}"),
            format!("manifest in config.toml, with its key in {FILE}"),
        ]
    );
    assert!(presence.importable(Place::Environment));
    assert!(presence.importable(Place::File));
    let url_only = Presence::of(names(&["ANTHROPIC_BASE_URL"]), BTreeSet::new(), None);
    assert!(!url_only.any_importable());
    assert!(url_only.offer_lines(FILE).is_empty());
}

#[test]
fn a_broken_or_odd_env_file_is_only_a_warning() {
    let dir = crate::scratch_dir("presence");
    let path = dir.root.join(".env");
    std::fs::write(&path, "ANTHROPIC_API_KEY=k\nnot a line CANARYBROKEN\n").unwrap();
    let inputs = KeychainInputs::load(true, Some(path.clone()), false, None);
    assert!(crate::keychain::keychain(&inputs).is_ok());
    assert_eq!(
        scan_problem(&inputs, FILE).as_deref(),
        Some("~/.config/wisp/.env doesn't parse (line 2); it isn't read on this machine")
    );
    let absent = KeychainInputs::load(false, Some(path.clone()), false, None);
    let error = crate::keychain::keychain(&absent).unwrap_err();
    assert!(
        error.ends_with("line 2 is not KEY=VALUE; nothing was loaded"),
        "{error}"
    );
    assert!(!error.contains("CANARY"));
    assert_eq!(scan_problem(&absent, FILE), None);
    std::fs::write(&path, [0xff, 0xfe]).unwrap();
    let odd = KeychainInputs::load(true, Some(path), false, None);
    assert_eq!(
        scan_problem(&odd, FILE).as_deref(),
        Some("~/.config/wisp/.env is not valid UTF-8; it isn't read on this machine")
    );
    let given = KeychainInputs::load(true, Some(dir.root.join("CANARYMISSING")), true, None);
    assert_eq!(
        scan_problem(&given, FILE).as_deref(),
        Some("the file given with --env-file doesn't exist")
    );
    assert!(!given.shows_path());
    assert_eq!(given.keys_file, wisp_core::secrets::GIVEN_ENV_FILE);
    let read = dir.root.join("given.env");
    std::fs::write(&read, "ANTHROPIC_API_KEY=k\n").unwrap();
    assert!(KeychainInputs::load(true, Some(read), true, None).shows_path());
    let missing = KeychainInputs::load(true, Some(dir.root.join("none")), false, None);
    assert_eq!(scan_problem(&missing, FILE), None);
}

fn fake_env(name: &str) -> Option<String> {
    match name {
        "ANTHROPIC_API_KEY" => Some("sk-ant-CANARYEXPORTED".to_owned()),
        "MANIFEST_BASE_URL" => Some("https://u:CANARYPASS@gw.example.com/CANARYPATH".to_owned()),
        _ => None,
    }
}

#[test]
fn redactions_cover_ambient_values_with_or_without_a_keychain() {
    let dir = crate::scratch_dir("presence");
    let path = dir.root.join(".env");
    std::fs::write(&path, "MANIFEST_API_KEY=CANARYFILEKEY\n").unwrap();
    let config = config(TABLES);
    let line = "sk-ant-CANARYEXPORTED CANARYFILEKEY CANARYPASS gw.example.com/CANARYPATH";
    let on = KeychainInputs::load(true, Some(path.clone()), false, None).with_env(&fake_env);
    let scrubbed = on.redactions(Some(&config), []).scrub(line);
    assert!(!scrubbed.contains("CANARY"), "{scrubbed}");
    assert!(matches!(
        crate::keychain::keychain(&on),
        Ok(wisp_core::endpoints::Keychain::Available(_))
    ));
    let off = KeychainInputs::load(false, Some(path), false, None).with_env(&fake_env);
    let scrubbed = off.redactions(Some(&config), []).scrub(line);
    assert!(!scrubbed.contains("CANARYEXPORTED"), "{scrubbed}");
    assert!(!scrubbed.contains("CANARYPASS"), "{scrubbed}");
    assert!(!scrubbed.contains("CANARYPATH"), "{scrubbed}");
    assert!(scrubbed.contains("CANARYFILEKEY"), "{scrubbed}");
    let keychain = crate::keychain::keychain(&off).unwrap();
    let variables = keychain.variables().unwrap();
    assert_eq!(
        (variables.env())("ANTHROPIC_API_KEY").as_deref(),
        Some("sk-ant-CANARYEXPORTED")
    );
    assert!(variables.env_file().is_some());
    let inherited = KeychainInputs::load(false, None, false, None).with_env(&fake_env);
    assert!(!inherited.available());
    assert!(inherited.env_path.is_none());
    assert!(inherited.scan().is_none());
    assert!(inherited.env_file().unwrap().is_none());
    assert_eq!(
        inherited
            .redactions(None, [])
            .scrub("sk-ant-CANARYEXPORTED"),
        wisp_core::session::REDACTED
    );
    let keychain = crate::keychain::keychain(&inherited).unwrap();
    assert!(keychain.variables().unwrap().env_file().is_none());
}

#[test]
fn a_non_utf8_env_file_is_still_scrubbed_where_it_can_be_read() {
    let dir = crate::scratch_dir("presence");
    let path = dir.root.join(".env");
    let mut bytes = b"ANTHROPIC_API_KEY=sk-ant-CANARYREADABLE\n".to_vec();
    bytes.extend_from_slice(&[0xff, 0xfe, b'\n']);
    std::fs::write(&path, bytes).unwrap();
    let inputs = KeychainInputs::load(true, Some(path), false, None);
    let scrubbed = inputs
        .redactions(None, [])
        .scrub("x sk-ant-CANARYREADABLE y");
    assert_eq!(scrubbed, "x [redacted] y");
    assert_eq!(
        scan_problem(&inputs, FILE).as_deref(),
        Some("~/.config/wisp/.env is not valid UTF-8; it isn't read on this machine")
    );
    assert!(!format!("{inputs:?}").contains("CANARY"));
}
