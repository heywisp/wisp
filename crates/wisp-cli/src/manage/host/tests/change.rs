use wisp_core::endpoints::host::{EditOutcome, RemoveOutcome};
use wisp_protocol::VariableName;
use wisp_protocol::endpoint::{ConnectionField, EndpointEdit, UrlEdit, VariableEdit};

use super::*;

const NEW_KEY: &str = "sk-ant-CANARYNEWKEYc3d4";

async fn saved_gw(place: &Place, host: &CliEndpointHost, store: StoreChoice) {
    let outcome = host
        .save(
            spec("gw", Some("https://gw.example.com".into()), store),
            Secret::new(KEY),
            false,
            idle(),
        )
        .await
        .unwrap();
    assert!(matches!(outcome, SaveOutcome::Saved(_)), "{outcome:?}");
    assert!(place.read("config.toml").contains("[endpoints.gw]"));
}

fn key_edit() -> EndpointEdit {
    EndpointEdit {
        key: Some(Secret::new(NEW_KEY)),
        ..EndpointEdit::default()
    }
}

fn no_path(place: &Place, notes: &[String]) {
    let root = place.dir.root.to_string_lossy().into_owned();
    for note in notes {
        assert!(!note.contains(&root), "{note}");
    }
}

#[tokio::test]
async fn a_checked_key_edit_writes_the_new_key_and_names_what_changed() {
    let place = Place::new();
    let host = place.host(Keychain::off());
    let url = answering(200, r#"{"input_tokens":3}"#);
    let outcome = host
        .save(
            spec("gw", Some(url), StoreChoice::File),
            Secret::new(KEY),
            false,
            idle(),
        )
        .await
        .unwrap();
    assert!(matches!(outcome, SaveOutcome::Saved(_)));
    let outcome = host
        .edit("gw".into(), key_edit(), None, true)
        .await
        .unwrap();
    let EditOutcome::Edited(edited) = &outcome else {
        panic!("{outcome:?}");
    };
    assert_eq!(edited.name, "gw");
    assert_eq!(edited.changed, vec![ConnectionField::Key]);
    assert_eq!(edited.store, Some(Kept::File));
    assert_eq!(edited.check, CheckResult::of(CheckKind::Ok));
    no_path(&place, &edited.notes);
    no_canary(&format!("{outcome:?}"));
    let credentials = place.read("credentials.json");
    assert!(credentials.contains(NEW_KEY), "the new key is kept");
    assert!(!credentials.contains(KEY));
}

#[tokio::test]
async fn a_refused_check_writes_nothing_and_save_anyway_writes() {
    let place = Place::new();
    let host = place.host(Keychain::off());
    let url = answering(401, "{}");
    let outcome = host
        .save(
            spec("gw", Some(url), StoreChoice::File),
            Secret::new(KEY),
            false,
            idle(),
        )
        .await
        .unwrap();
    assert!(matches!(outcome, SaveOutcome::Saved(_)));
    let before = place.read("credentials.json");
    let outcome = host
        .edit("gw".into(), key_edit(), None, true)
        .await
        .unwrap();
    let EditOutcome::NotSaved(NotSaved {
        why: NotSavedReason::Check(check),
        ..
    }) = &outcome
    else {
        panic!("{outcome:?}");
    };
    assert_eq!(check.outcome, CheckKind::KeyRefused);
    assert_eq!(place.read("credentials.json"), before);
    let outcome = host
        .edit("gw".into(), key_edit(), None, false)
        .await
        .unwrap();
    let EditOutcome::Edited(edited) = outcome else {
        panic!("{outcome:?}");
    };
    assert_eq!(edited.check, CheckResult::skipped());
    assert!(place.read("credentials.json").contains(NEW_KEY));
}

#[tokio::test]
async fn auth_and_the_provider_url_change_the_table_without_a_probe() {
    let place = Place::new();
    let host = place.host(Keychain::off());
    saved_gw(&place, &host, StoreChoice::File).await;
    let edit = EndpointEdit {
        auth: Some("api_key".into()),
        url: Some(UrlEdit::ProviderDefault),
        ..EndpointEdit::default()
    };
    let outcome = host.edit("gw".into(), edit, None, false).await.unwrap();
    let EditOutcome::Edited(edited) = outcome else {
        panic!("{outcome:?}");
    };
    assert_eq!(
        edited.changed,
        vec![ConnectionField::Auth, ConnectionField::Url]
    );
    let config = place.read("config.toml");
    assert!(config.contains("auth = \"api_key\""), "{config}");
    assert!(!place.read("credentials.json").contains("gw.example.com"));
}

#[tokio::test]
async fn store_file_moves_a_keychain_key_and_reports_the_move() {
    let place = Place::new();
    let memory = MemoryKeyring::new();
    let host = place.host(memory_keychain(&memory));
    saved_gw(&place, &host, StoreChoice::Keyring).await;
    assert_eq!(memory.items().len(), 1);
    let outcome = host
        .edit(
            "gw".into(),
            EndpointEdit::default(),
            Some(StoreChoice::File),
            false,
        )
        .await
        .unwrap();
    let EditOutcome::Edited(edited) = outcome else {
        panic!("{outcome:?}");
    };
    assert_eq!(edited.changed, vec![ConnectionField::Store]);
    assert_eq!(edited.store, Some(Kept::File));
    assert!(place.read("credentials.json").contains(KEY));
    assert!(place.read("config.toml").contains("key_store = \"file\""));
}

#[tokio::test]
async fn a_new_key_for_a_keychain_that_is_down_is_refused_before_anything_is_written() {
    let place = Place::new();
    let memory = MemoryKeyring::new();
    let host = place.host(memory_keychain(&memory));
    saved_gw(&place, &host, StoreChoice::Keyring).await;
    memory.set_mode(MemoryMode::Unopenable);
    let host = place.host(memory_keychain(&memory));
    let before = place.read("config.toml");
    let refused = host
        .edit("gw".into(), key_edit(), None, false)
        .await
        .unwrap_err();
    assert!(refused.0.contains("keychain"), "{}", refused.0);
    no_canary(&refused.0);
    assert!(place.read("credentials.json").is_empty());
    assert_eq!(place.read("config.toml"), before);
}

#[tokio::test]
async fn a_variable_endpoint_changes_its_variables_and_refuses_a_key() {
    let place = Place::new();
    std::fs::write(
        place.config(),
        "[endpoints.gw]\nauth = \"bearer\"\nkey_env = \"GW_KEY\"\n",
    )
    .unwrap();
    let host = place.host(Keychain::off());
    let edit = EndpointEdit {
        key_env: VariableName::parse("WORK_KEY").map(VariableEdit::Set),
        url_env: VariableName::parse("WORK_URL").map(VariableEdit::Set),
        ..EndpointEdit::default()
    };
    let outcome = host.edit("gw".into(), edit, None, true).await.unwrap();
    let EditOutcome::Edited(edited) = outcome else {
        panic!("{outcome:?}");
    };
    assert_eq!(
        edited.changed,
        vec![ConnectionField::KeyEnv, ConnectionField::UrlEnv]
    );
    assert_eq!(edited.store, None);
    assert_eq!(edited.check, CheckResult::skipped());
    assert!(
        edited
            .notes
            .iter()
            .any(|n| n.starts_with("Nothing was read or checked now")),
        "{:?}",
        edited.notes
    );
    let config = place.read("config.toml");
    assert!(config.contains("key_env = \"WORK_KEY\""), "{config}");
    assert!(config.contains("url_env = \"WORK_URL\""), "{config}");
    let before = place.read("config.toml");
    let refused = host
        .edit("gw".into(), key_edit(), None, false)
        .await
        .unwrap_err();
    assert!(
        refused.0.contains("names its key as a variable"),
        "{}",
        refused.0
    );
    no_canary(&refused.0);
    let unset = EndpointEdit {
        key_env: Some(VariableEdit::Unset),
        ..EndpointEdit::default()
    };
    let refused = host
        .edit("gw".into(), unset, None, false)
        .await
        .unwrap_err();
    assert!(refused.0.contains("can't be unset"), "{}", refused.0);
    assert_eq!(place.read("config.toml"), before);
}

#[tokio::test]
async fn invalid_edits_are_refused_without_echoing_them() {
    let place = Place::new();
    let host = place.host(Keychain::off());
    saved_gw(&place, &host, StoreChoice::File).await;
    let before = place.read("config.toml");
    let canary_var = VariableName::parse("abcdefghijCANARY0123456789").map(VariableEdit::Set);
    let cases = [
        ("nope", key_edit()),
        ("env", key_edit()),
        ("gw", EndpointEdit::default()),
        (
            "gw",
            EndpointEdit {
                key_env: canary_var,
                ..EndpointEdit::default()
            },
        ),
        (
            "gw",
            EndpointEdit {
                url: Some(UrlEdit::Set(Secret::new("ftp://CANARYHOST/x"))),
                ..EndpointEdit::default()
            },
        ),
        (
            "gw",
            EndpointEdit {
                auth: Some("CANARYAUTH".into()),
                ..EndpointEdit::default()
            },
        ),
        (
            "gw",
            EndpointEdit {
                url_env: Some(VariableEdit::Unset),
                ..EndpointEdit::default()
            },
        ),
    ];
    for (name, edit) in cases {
        let refused = host.edit(name.into(), edit, None, false).await.unwrap_err();
        no_canary(&refused.0);
    }
    assert_eq!(place.read("config.toml"), before);
}

#[tokio::test]
async fn remove_deletes_the_table_and_its_key_and_clears_the_default() {
    let place = Place::new();
    let host = place.host(Keychain::off());
    saved_gw(&place, &host, StoreChoice::File).await;
    let outcome = host.remove("gw".into(), false).await.unwrap();
    let RemoveOutcome::Removed(removed) = &outcome else {
        panic!("{outcome:?}");
    };
    assert_eq!(removed.name, "gw");
    assert!(!removed.kept_item);
    assert!(removed.default_cleared);
    no_path(&place, &removed.notes);
    let config = place.read("config.toml");
    assert!(!config.contains("[endpoints.gw]"), "{config}");
    assert!(!place.read("credentials.json").contains(KEY));
    let again = host.remove("gw".into(), false).await.unwrap_err();
    assert!(again.0.contains("there is no account"), "{}", again.0);
}

#[tokio::test]
async fn a_keychain_that_is_down_offers_keep_key_and_keep_key_removes_the_table() {
    let place = Place::new();
    let memory = MemoryKeyring::new();
    let host = place.host(memory_keychain(&memory));
    saved_gw(&place, &host, StoreChoice::Keyring).await;
    memory.set_mode(MemoryMode::Unopenable);
    let host = place.host(memory_keychain(&memory));
    let before = place.read("config.toml");
    let outcome = host.remove("gw".into(), false).await.unwrap();
    assert!(
        matches!(
            &outcome,
            RemoveOutcome::NotSaved(NotSaved {
                why: NotSavedReason::KeychainFailed(_),
                ..
            })
        ),
        "{outcome:?}"
    );
    assert_eq!(place.read("config.toml"), before);
    let outcome = host.remove("gw".into(), true).await.unwrap();
    let RemoveOutcome::Removed(removed) = &outcome else {
        panic!("{outcome:?}");
    };
    assert!(removed.kept_item);
    assert!(!place.read("config.toml").contains("[endpoints.gw]"));
    let account = store_id_of(&memory);
    for note in &removed.notes {
        assert!(!note.contains(&account), "{note}");
    }
    assert!(!format!("{outcome:?}").contains(&account));
}

#[tokio::test]
async fn a_failed_keychain_write_is_answered_as_not_saved_with_nothing_written() {
    let place = Place::new();
    let host = place.host(Keychain::off());
    saved_gw(&place, &host, StoreChoice::File).await;
    let memory = MemoryKeyring::new();
    memory.set_mode(MemoryMode::Unopenable);
    let host = place.host(memory_keychain(&memory));
    let before = (place.read("config.toml"), place.read("credentials.json"));
    let outcome = host
        .edit("gw".into(), key_edit(), Some(StoreChoice::Keyring), false)
        .await
        .unwrap();
    assert_eq!(
        outcome,
        EditOutcome::NotSaved(NotSaved {
            name: "gw".into(),
            why: NotSavedReason::KeychainFailed("isn't running".into()),
        })
    );
    assert_eq!(
        (place.read("config.toml"), place.read("credentials.json")),
        before
    );
}

#[tokio::test]
async fn a_move_to_the_store_that_already_holds_the_key_is_refused_and_writes_nothing() {
    let place = Place::new();
    let host = place.host(Keychain::off());
    saved_gw(&place, &host, StoreChoice::File).await;
    let config = place.read("config.toml");
    let credentials = place.read("credentials.json");
    let refused = host
        .edit(
            "gw".into(),
            EndpointEdit::default(),
            Some(StoreChoice::File),
            false,
        )
        .await
        .unwrap_err();
    assert_eq!(
        refused.0,
        "gw's key is already kept in credentials.json; nothing was changed"
    );
    assert_eq!(place.read("config.toml"), config);
    assert_eq!(place.read("credentials.json"), credentials);
}

#[test]
fn an_unconfirmed_keychain_write_on_edit_or_remove_names_no_account() {
    use wisp_core::endpoints::EndpointName;
    use wisp_core::secrets::{ChangeError, KeyringError};

    let name = EndpointName::parse("gw").unwrap();
    let unconfirmed = || KeyringError::Unconfirmed {
        name: name.clone(),
        account: "store1234/gw".to_owned(),
    };
    for error in [
        ChangeError::KeychainWrite {
            name: name.clone(),
            error: unconfirmed(),
        },
        ChangeError::Keychain {
            name: name.clone(),
            error: unconfirmed(),
            keep_key: false,
        },
    ] {
        let words = super::super::change::safe_error(&error, None);
        assert!(words.contains("may have been saved there"), "{words}");
        assert!(!words.contains("store1234"), "{words}");
        assert!(!words.contains("account"), "{words}");
    }
}

#[test]
fn file_cleanup_failures_are_reported_without_calling_them_keychain_failures() {
    let name = wisp_core::endpoints::EndpointName::parse("plan").unwrap();
    let error = wisp_core::secrets::StoreError::Write(std::io::ErrorKind::PermissionDenied);
    let text = crate::manage::host::change::left_words(&name, &error);
    assert!(
        text.contains("plan")
            && text.contains("wasn't deleted")
            && text.contains("credentials.json"),
        "{text}"
    );
    assert!(!text.contains("keychain"), "{text}");
}

#[tokio::test]
async fn a_signed_out_key_the_keychain_wont_delete_is_named_and_the_start_moves() {
    use wisp_protocol::subscription_auth::{AuthCommand, AuthOutcome};
    let place = Place::new();
    let memory = MemoryKeyring::new();
    let host = place.host(memory_keychain(&memory));
    saved_gw(&place, &host, StoreChoice::File).await;
    let outcome = host
        .save(
            spec("work", None, StoreChoice::Keyring),
            Secret::new(NEW_KEY),
            false,
            idle(),
        )
        .await
        .unwrap();
    assert!(matches!(outcome, SaveOutcome::Saved(_)), "{outcome:?}");
    assert!(place.read("config.toml").contains("default = \"work\""));
    memory.set_mode(MemoryMode::Failing(BackendError::PlatformFailure));
    let out = crate::auth::session_auth(
        &host,
        AuthCommand::logout(
            wisp_protocol::RequestId(1),
            &["work".to_owned()],
            true,
            None,
            Vec::new(),
        ),
    )
    .await
    .outcome;
    let AuthOutcome::LoggedOut {
        left_keys,
        new_start,
        stopped: None,
        ..
    } = &out
    else {
        panic!("{out:?}")
    };
    assert_eq!(left_keys, &["work".to_owned()]);
    assert_eq!(new_start.as_deref(), Some("gw"));
    assert!(!place.read("config.toml").contains("[endpoints.work]"));
    no_canary(&format!("{out:?}"));
}
