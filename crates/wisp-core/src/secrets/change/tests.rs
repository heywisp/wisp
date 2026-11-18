use std::path::Path;
use std::time::Duration;

use super::*;
use crate::config_file::NewEndpoint;
use crate::endpoints::{AuthKind, PricingMode, ProviderKind};
use crate::secrets::{CREDENTIALS, Secret, lock};
use crate::test_guard::absent;
use crate::test_guard::dir;
use crate::test_guard::name;

const WAIT: Duration = Duration::from_millis(300);

fn record(key: &str) -> StoredRecord {
    StoredRecord::Key {
        key: Secret::new(key),
        url: None,
    }
}

fn add(text: &str, key: &str) -> Change {
    Change::Add {
        name: name(text),
        endpoint: NewEndpoint {
            provider: ProviderKind::Anthropic,
            auth: AuthKind::ApiKey,
            store: StoreKind::File,
            pricing: PricingMode::List,
        },
        record: record(key),
        make_default: false,
    }
}

fn remove(text: &str) -> Change {
    Change::Remove {
        name: name(text),
        keep_key: false,
        chat: None,
    }
}

const NO_KEYCHAIN: crate::secrets::keyring::Unreachable =
    crate::secrets::keyring::Unreachable(KeyringError::Unsupported);

fn run(config: &Path, change: Change) -> Result<Applied, ChangeError> {
    apply(config, PathShown::Default, change, &NO_KEYCHAIN, WAIT)
}

fn read(path: &Path) -> String {
    std::fs::read_to_string(path).unwrap_or_default()
}

fn store_of(config: &Path) -> FileStore {
    FileStore::beside(config, PathShown::Default)
}

fn store_id(config: &Path) -> Option<StoreId> {
    ConfigText::read(config, PathShown::Default)
        .unwrap()
        .current()
        .unwrap()
        .store_id
}

fn records(config: &Path) -> BTreeMap<EndpointName, Stored> {
    let id = store_id(config).unwrap();
    store_of(config).read_all(&id).unwrap()
}

fn key_of(config: &Path, endpoint: &str) -> Option<String> {
    records(config).get(&name(endpoint)).map(|s| {
        let StoredRecord::Key { key, .. } = &s.record;
        key.expose().to_owned()
    })
}

fn canonical(path: &Path) -> PathBuf {
    std::fs::canonicalize(path).unwrap()
}

#[test]
fn add_writes_the_record_owned_by_this_config_then_the_table() {
    let root = dir();
    let config = root.path().join("wisp/config.toml");
    let applied = run(&config, add("direct", "k-direct")).unwrap();
    assert_eq!(
        applied.outcome,
        Outcome::Added {
            created: true,
            default: false
        }
    );
    assert!(applied.detached.is_none());
    let text = read(&config);
    assert!(text.contains("[endpoints.direct]"), "{text}");
    assert!(text.contains("key_store = \"file\""), "{text}");
    let stored = &records(&config)[&name("direct")];
    assert_eq!(stored.owner.as_deref(), Some(canonical(&config).as_path()));
    assert_eq!(key_of(&config, "direct").as_deref(), Some("k-direct"));
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mode = |p: &Path| std::fs::metadata(p).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode(&config), 0o600);
        assert_eq!(mode(&root.path().join("wisp").join(CREDENTIALS)), 0o600);
        assert_eq!(mode(&root.path().join("wisp")), 0o700);
    }
    let id = store_id(&config);
    run(&config, add("second", "k-second")).unwrap();
    assert_eq!(store_id(&config), id, "a config keeps its store ID");
}

fn colours(config: &Path, keys_file: Option<&str>) -> Vec<(String, u8)> {
    let file = ConfigText::read(config, PathShown::Default)
        .unwrap()
        .current()
        .unwrap();
    let picked = crate::endpoints::marks::pick(Some(&file), keys_file.map(name).as_ref());
    picked
        .into_iter()
        .map(|(n, m)| (n.to_string(), m))
        .collect()
}

fn marked(config: &Path, table: &str, line: &str) {
    let header = format!("[endpoints.{table}]\n");
    let text = read(config).replacen(&header, &format!("{header}{line}\n"), 1);
    std::fs::write(config, text).unwrap();
}

#[test]
fn each_account_keeps_its_colour_through_sign_ins_sign_outs_and_marks() {
    let own = |pairs: &[(&str, u8)]| -> Vec<(String, u8)> {
        pairs.iter().map(|(n, m)| ((*n).to_owned(), *m)).collect()
    };
    assert_eq!(crate::endpoints::marks::home(&name("claude")), 3);
    let root = dir();
    let config = root.path().join("config.toml");
    for account in ["a", "b", "c", "d"] {
        run(&config, add(account, "k")).unwrap();
    }
    let four = own(&[("a", 5), ("b", 3), ("c", 6), ("d", 1)]);
    assert_eq!(colours(&config, None), four);
    assert_eq!(
        colours(&config, None),
        four,
        "the same file, the same colours"
    );
    run(&config, add("e", "k")).unwrap();
    let five = own(&[("a", 5), ("b", 3), ("c", 6), ("d", 1), ("e", 4)]);
    assert_eq!(
        colours(&config, None),
        five,
        "a sign-in moves no other colour"
    );
    run(&config, remove("a")).unwrap();
    let left = own(&[("b", 3), ("c", 5), ("d", 1), ("e", 4)]);
    assert_eq!(colours(&config, None), left, "no account on its home moves");
    marked(&config, "e", "mark = 3");
    let pinned = own(&[("b", 4), ("c", 5), ("d", 1), ("e", 3)]);
    assert_eq!(
        colours(&config, None),
        pinned,
        "a mark holds and moves its pick"
    );
    marked(&config, "d", "mark = 3");
    assert_eq!(
        colours(&config, None)[2],
        ("d".to_owned(), 3),
        "two marks share"
    );
    for bad in ["mark = 0", "mark = 7", "mark = \"blue\""] {
        let text = read(&config);
        marked(&config, "c", bad);
        assert_eq!(colours(&config, None)[1], ("c".to_owned(), 5), "{bad}");
        std::fs::write(&config, text).unwrap();
    }
    let root = dir();
    let config = root.path().join("config.toml");
    for account in ["a", "b", "c", "d", "e", "f", "g"] {
        run(&config, add(account, "k")).unwrap();
    }
    assert_eq!(
        colours(&config, None)[6],
        ("g".to_owned(), 2),
        "a seventh shares"
    );

    let root = dir();
    let config = root.path().join("config.toml");
    for account in ["claude", "claude-work"] {
        run(&config, add(account, "k")).unwrap();
    }
    let tables = own(&[("claude", 3), ("claude-work", 4)]);
    let with_key = own(&[("claude", 3), ("claude-api", 5), ("claude-work", 4)]);
    assert_eq!(
        colours(&config, Some("claude-api")),
        with_key,
        "the keys file comes last"
    );
    assert_eq!(colours(&config, None), tables);
    let text = read(&config);
    for env in ["[endpoints.env]\n", "[endpoints.env]\nmark = 3\n"] {
        std::fs::write(
            &config,
            text.replacen("[endpoints.", &format!("{env}[endpoints."), 1),
        )
        .unwrap();
        let mut keyed = colours(&config, Some("claude-api"));
        keyed.retain(|(n, _)| n != "claude-api");
        assert_eq!(keyed, colours(&config, None), "{env}");
    }
    assert_eq!(
        colours(&config, None),
        own(&[("claude", 4), ("claude-work", 5)])
    );
    assert_eq!(
        colours(&config, Some("claude-api"))[1],
        ("claude-api".to_owned(), 3)
    );
    std::fs::write(&config, &text).unwrap();
    run(&config, add("second", "k")).unwrap();
    let moved = own(&[
        ("claude", 3),
        ("claude-api", 6),
        ("claude-work", 4),
        ("second", 5),
    ]);
    assert_eq!(
        colours(&config, Some("claude-api")),
        moved,
        "only the keys file moves"
    );
    run(&config, remove("second")).unwrap();
    run(&config, remove("claude")).unwrap();
    assert_eq!(colours(&config, None), own(&[("claude-work", 3)]));
    run(&config, add("claude", "k")).unwrap();
    let swapped = own(&[("claude", 4), ("claude-work", 3)]);
    assert_eq!(
        colours(&config, None),
        swapped,
        "out and in again swaps two"
    );
    marked(&config, "claude", "mark = 6");
    let rename = Edit {
        rename: Some(name("renamed")),
        ..Edit::default()
    };
    run(&config, edit("claude", rename)).unwrap();
    assert_eq!(colours(&config, None)[0], ("claude-work".to_owned(), 3));
    assert_eq!(
        colours(&config, None)[1],
        ("renamed".to_owned(), 6),
        "a rename keeps its mark"
    );
}

#[test]
fn a_failure_between_the_writes_of_add_leaves_an_orphan_never_a_dangling_table() {
    let root = dir();
    let config = root.path().join("config.toml");
    run(&config, add("first", "k1")).unwrap();
    let before = read(&config);
    let failed = apply_with(
        &config,
        PathShown::Default,
        add("second", "k2"),
        &NO_KEYCHAIN,
        WAIT,
        &|| Err(ChangeError::Interrupted),
    );
    assert_eq!(failed.unwrap_err(), ChangeError::Interrupted);
    assert_eq!(read(&config), before, "config.toml never names the new key");
    assert_eq!(key_of(&config, "second").as_deref(), Some("k2"));
}

#[test]
fn a_failure_between_the_writes_of_remove_leaves_the_record_as_an_orphan() {
    let root = dir();
    let config = root.path().join("config.toml");
    run(&config, add("first", "k1")).unwrap();
    run(
        &config,
        Change::SetDefault {
            name: name("first"),
        },
    )
    .unwrap();
    let failed = apply_with(
        &config,
        PathShown::Default,
        remove("first"),
        &NO_KEYCHAIN,
        WAIT,
        &|| Err(ChangeError::Interrupted),
    );
    assert_eq!(failed.unwrap_err(), ChangeError::Interrupted);
    let text = read(&config);
    assert!(!text.contains("[endpoints.first]"), "{text}");
    assert!(!text.contains("default"), "{text}");
    assert_eq!(key_of(&config, "first").as_deref(), Some("k1"));
    let applied = run(&config, remove("first")).unwrap();
    assert_eq!(applied.outcome, Outcome::LeftoverRemoved);
    assert_eq!(key_of(&config, "first"), None);
}

#[test]
fn remove_deletes_the_table_the_default_and_the_record() {
    let root = dir();
    let config = root.path().join("config.toml");
    run(&config, add("first", "k1")).unwrap();
    run(&config, add("second", "k2")).unwrap();
    run(
        &config,
        Change::SetDefault {
            name: name("first"),
        },
    )
    .unwrap();
    let applied = run(&config, remove("first")).unwrap();
    assert_eq!(
        applied.outcome,
        Outcome::Removed {
            store: Some(StoreKind::File),
            deleted: true,
            was_default: true,
            kept: None,
            new_start: Some(name("second")),
        }
    );
    let text = read(&config);
    assert!(!text.contains("first"), "{text}");
    assert!(text.contains("default = \"second\""), "{text}");
    assert_eq!(key_of(&config, "first"), None);
    assert_eq!(key_of(&config, "second").as_deref(), Some("k2"));
}

#[test]
fn remove_of_a_variable_table_touches_no_store() {
    let root = dir();
    let config = root.path().join("config.toml");
    std::fs::write(
        &config,
        "default = \"work\"\n# my work key\n[endpoints.work]\nauth = \"api_key\"\nkey_env = \"WORK_KEY\"\n\n[endpoints.keep]\nauth = \"api_key\"\nkey_env = \"KEEP\"\n",
    )
    .unwrap();
    let applied = run(&config, remove("work")).unwrap();
    assert_eq!(
        applied.outcome,
        Outcome::Removed {
            store: None,
            deleted: false,
            was_default: true,
            kept: None,
            new_start: Some(name("keep")),
        }
    );
    assert!(!root.path().join(CREDENTIALS).exists());
    assert_eq!(
        read(&config),
        "default = \"keep\"\n\n[endpoints.keep]\nauth = \"api_key\"\nkey_env = \"KEEP\"\n"
    );
}

#[test]
fn names_are_checked_again_under_the_locks() {
    let root = dir();
    let config = root.path().join("config.toml");
    run(&config, add("direct", "k")).unwrap();
    assert_eq!(
        run(&config, add("direct", "k2")).unwrap_err(),
        ChangeError::NameTaken(name("direct"))
    );
    assert_eq!(
        run(&config, add("env", "k2")).unwrap_err(),
        ChangeError::NameTaken(name("env"))
    );
    assert_eq!(
        run(&config, remove("nope")).unwrap_err(),
        ChangeError::NoSuchEndpoint(name("nope"))
    );
    assert_eq!(
        run(&config, Change::SetDefault { name: name("nope") }).unwrap_err(),
        ChangeError::NoSuchEndpoint(name("nope"))
    );
    assert_eq!(key_of(&config, "direct").as_deref(), Some("k"));
}

#[test]
fn a_corrupt_credentials_file_is_never_overwritten() {
    let root = dir();
    let config = root.path().join("config.toml");
    run(&config, add("direct", "k")).unwrap();
    let credentials = root.path().join(CREDENTIALS);
    std::fs::write(&credentials, "{ not json CANARY").unwrap();
    let before = read(&config);
    for change in [add("other", "k2"), remove("direct")] {
        let error = run(&config, change).unwrap_err();
        assert_eq!(error, ChangeError::Store(StoreError::Refused));
        assert!(!format!("{error} {error:?}").contains("CANARY"));
    }
    assert_eq!(read(&credentials), "{ not json CANARY");
    assert_eq!(read(&config), before);
}

#[test]
fn a_config_that_does_not_parse_is_never_edited() {
    let root = dir();
    let config = root.path().join("config.toml");
    std::fs::write(&config, "[endpoints.x\nCANARY").unwrap();
    let error = run(&config, add("direct", "k")).unwrap_err();
    assert!(matches!(error, ChangeError::Config(_)), "{error:?}");
    assert_eq!(read(&config), "[endpoints.x\nCANARY");
    assert!(!root.path().join(CREDENTIALS).exists());
}

#[test]
fn a_held_lock_makes_a_writer_give_up_without_writing() {
    let root = dir();
    let config = root.path().join("config.toml");
    run(&config, add("direct", "k")).unwrap();
    let before = read(&config);
    let held = lock(&[(root.path(), LockKind::Config)], PathShown::Default, WAIT).unwrap();
    let error = run(&config, add("other", "k2")).unwrap_err();
    assert_eq!(error, ChangeError::Store(StoreError::Busy));
    assert_eq!(
        error.to_string(),
        "another wisp is changing accounts; try again"
    );
    drop(held);
    assert_eq!(read(&config), before);
    run(&config, add("other", "k2")).unwrap();
}

#[test]
fn a_copy_in_the_same_folder_detaches_on_its_first_write() {
    let root = dir();
    let original = root.path().join("config.toml");
    run(&original, add("direct", "k-direct")).unwrap();
    run(&original, add("work", "k-work")).unwrap();
    let copy = root.path().join("copy.toml");
    std::fs::copy(&original, &copy).unwrap();
    let original_id = store_id(&original).unwrap();
    let original_records = store_of(&original).read_all(&original_id).unwrap();
    let applied = run(&copy, add("extra", "k-extra")).unwrap();
    assert_eq!(
        applied.detached,
        Some(DetachNote {
            from: vec![Some(canonical(&original))]
        })
    );
    let copy_id = store_id(&copy).unwrap();
    assert_ne!(copy_id, original_id);
    assert_eq!(
        store_of(&original).read_all(&original_id).unwrap(),
        original_records,
        "the original's records are never replaced"
    );
    let copied = records(&copy);
    assert_eq!(copied.len(), 3);
    for stored in copied.values() {
        assert_eq!(stored.owner.as_deref(), Some(canonical(&copy).as_path()));
    }
    assert_eq!(key_of(&copy, "direct").as_deref(), Some("k-direct"));
    let again = run(&copy, add("more", "k-more")).unwrap();
    assert!(
        again.detached.is_none(),
        "the second write changes in place"
    );
    assert_eq!(store_id(&copy).unwrap(), copy_id);
}

#[test]
fn remove_through_a_copy_never_deletes_the_originals_record() {
    let root = dir();
    let original = root.path().join("config.toml");
    run(&original, add("direct", "k-direct")).unwrap();
    let copy = root.path().join("copy.toml");
    std::fs::copy(&original, &copy).unwrap();
    let credentials = root.path().join(CREDENTIALS);
    let id = store_id(&original);
    let applied = run(&copy, remove("direct")).unwrap();
    assert!(applied.detached.is_none(), "no record was copied");
    assert!(matches!(
        applied.outcome,
        Outcome::Removed { deleted: false, .. }
    ));
    assert_ne!(
        store_id(&copy),
        id,
        "a writer that doesn't own the record detaches"
    );
    assert!(!read(&copy).contains("direct"));
    assert_eq!(key_of(&original, "direct").as_deref(), Some("k-direct"));
    assert!(read(&credentials).contains("k-direct"));
}

#[test]
fn copy_then_move_the_original_then_remove_through_the_copy_keeps_the_original_record() {
    let root = dir();
    let original = root.path().join("config.toml");
    run(&original, add("direct", "k-direct")).unwrap();
    let copy = root.path().join("copy.toml");
    std::fs::copy(&original, &copy).unwrap();
    let moved = root.path().join("moved.toml");
    std::fs::rename(&original, &moved).unwrap();
    let id = store_id(&moved).unwrap();
    let before = store_of(&moved).read_all(&id).unwrap();
    run(&copy, remove("direct")).unwrap();
    assert_eq!(store_of(&moved).read_all(&id).unwrap(), before);
    assert_eq!(key_of(&moved, "direct").as_deref(), Some("k-direct"));
}

#[test]
fn remove_through_a_copy_while_the_original_does_not_parse_keeps_its_record() {
    let root = dir();
    let original = root.path().join("config.toml");
    run(&original, add("direct", "k-direct")).unwrap();
    let copy = root.path().join("copy.toml");
    std::fs::copy(&original, &copy).unwrap();
    let id = store_id(&original).unwrap();
    let before = store_of(&original).read_all(&id).unwrap();
    std::fs::write(&original, "not [ toml").unwrap();
    run(&copy, remove("direct")).unwrap();
    assert_eq!(store_of(&copy).read_all(&id).unwrap(), before);
}

#[test]
fn a_moved_config_detaches_and_leaves_its_old_records_as_orphans() {
    let root = dir();
    let original = root.path().join("config.toml");
    run(&original, add("direct", "k-direct")).unwrap();
    let old_id = store_id(&original).unwrap();
    let moved = root.path().join("moved.toml");
    std::fs::rename(&original, &moved).unwrap();
    let applied = run(&moved, add("other", "k-other")).unwrap();
    assert_eq!(
        applied.detached,
        Some(DetachNote {
            from: vec![Some(
                root.path().canonicalize().unwrap().join("config.toml")
            )]
        })
    );
    let every = store_of(&moved).read_every().unwrap();
    assert!(every.contains_key(&old_id), "the old records stay");
    let config = ConfigText::read(&moved, PathShown::Default)
        .unwrap()
        .current()
        .unwrap();
    let found = crate::secrets::ownership(&config, &every);
    assert_eq!(found.orphans.len(), 1);
    assert_eq!(
        found.orphans[0].state,
        Some(crate::secrets::OwnerState::Gone)
    );
}

#[test]
fn a_record_without_an_owner_detaches_on_the_first_write() {
    let root = dir();
    let config = root.path().join("config.toml");
    std::fs::write(
        &config,
        "store_id = \"5f0c2a9e41d7b388\"\n[endpoints.m]\nauth = \"api_key\"\nkey_store = \"file\"\n",
    )
    .unwrap();
    std::fs::write(
        root.path().join(CREDENTIALS),
        "{\"version\": 1, \"stores\": {\"5f0c2a9e41d7b388\": {\"m\": {\"key\": \"k-hand\"}}}}",
    )
    .unwrap();
    let applied = run(&config, Change::SetDefault { name: name("m") }).unwrap();
    assert_eq!(applied.detached, Some(DetachNote { from: vec![None] }));
    assert_ne!(store_id(&config).unwrap().as_str(), "5f0c2a9e41d7b388");
    assert_eq!(key_of(&config, "m").as_deref(), Some("k-hand"));
    assert!(read(&config).contains("default = \"m\""));
}

#[test]
fn a_copy_from_another_folder_cannot_detach_until_its_keys_are_copied_in() {
    let root = dir();
    let original = root.path().join("a/config.toml");
    run(&original, add("direct", "k-direct")).unwrap();
    run(&original, add("work", "k-work")).unwrap();
    std::fs::create_dir_all(root.path().join("b")).unwrap();
    let copy = root.path().join("b/config.toml");
    std::fs::copy(&original, &copy).unwrap();
    let before = read(&copy);
    let error = run(&copy, add("extra", "k")).unwrap_err();
    assert_eq!(
        error,
        ChangeError::CantDetach {
            unreadable: vec![name("direct"), name("work")],
            credentials: root.path().join("b").join(CREDENTIALS),
            shown: PathShown::Default,
        }
    );
    let words = error.to_string();
    assert!(words.starts_with("nothing was changed: this config.toml must get its own copy of its keys first, and the keys of direct and work can't be read"), "{words}");
    assert!(!words.contains("wisp account remove"), "{words}");
    assert!(
        words.contains("(they aren't in its folder's credentials.json)"),
        "{words}"
    );
    assert!(words.ends_with("here first."), "{words}");
    assert!(
        !words.contains(&root.path().display().to_string()),
        "{words}"
    );
    assert_eq!(read(&copy), before);
    assert!(!root.path().join("b").join(CREDENTIALS).exists());
    assert!(
        run(&copy, remove("direct")).is_err(),
        "another key is unreadable too"
    );
    std::fs::copy(
        root.path().join("a").join(CREDENTIALS),
        root.path().join("b").join(CREDENTIALS),
    )
    .unwrap();
    let applied = run(&copy, add("extra", "k")).unwrap();
    assert!(applied.detached.is_some());
    assert_eq!(key_of(&copy, "direct").as_deref(), Some("k-direct"));
    assert_eq!(key_of(&copy, "work").as_deref(), Some("k-work"));
}

#[test]
fn remove_goes_ahead_when_only_its_own_record_is_missing() {
    let root = dir();
    let config = root.path().join("config.toml");
    run(&config, add("direct", "k-direct")).unwrap();
    run(&config, add("lost", "k-lost")).unwrap();
    let id = store_id(&config).unwrap();
    let locks = lock(
        &[(root.path(), LockKind::Credentials)],
        PathShown::Default,
        WAIT,
    )
    .unwrap();
    store_of(&config)
        .remove(&locks, &id, &name("lost"))
        .unwrap();
    drop(locks);
    let error = run(&config, add("extra", "k")).unwrap_err();
    assert!(
        error
            .to_string()
            .ends_with("(it isn't in its folder's credentials.json). If this config.toml was copied from another folder, copy that folder's credentials.json here first; or run wisp account remove lost to give it up, or wisp account edit lost --replace-key to give it a new key."),
        "{error}"
    );
    let credentials = read(&root.path().join(CREDENTIALS));
    let applied = run(&config, remove("lost")).unwrap();
    assert!(applied.detached.is_none());
    assert!(matches!(
        applied.outcome,
        Outcome::Removed { deleted: false, .. }
    ));
    assert!(!read(&config).contains("lost"));
    assert_eq!(store_id(&config).unwrap(), id, "no needless detach");
    assert_eq!(read(&root.path().join(CREDENTIALS)), credentials);
    assert_eq!(
        read(&root.path().join(CREDENTIALS))
            .matches("k-direct")
            .count(),
        1
    );
    assert_eq!(key_of(&config, "direct").as_deref(), Some("k-direct"));
    run(&config, add("extra", "k")).unwrap();
    assert_eq!(store_id(&config).unwrap(), id);
}

#[test]
fn two_configs_in_one_folder_keep_separate_records() {
    let root = dir();
    let one = root.path().join("config.toml");
    let two = root.path().join("other.toml");
    run(&one, add("direct", "k-one")).unwrap();
    run(&two, add("direct", "k-two")).unwrap();
    assert_ne!(store_id(&one), store_id(&two));
    assert_eq!(key_of(&one, "direct").as_deref(), Some("k-one"));
    assert_eq!(key_of(&two, "direct").as_deref(), Some("k-two"));
    run(&two, remove("direct")).unwrap();
    assert_eq!(key_of(&one, "direct").as_deref(), Some("k-one"));
}

#[cfg(unix)]
#[test]
fn a_linked_config_is_written_through_its_link_with_keys_beside_the_link() {
    use std::os::unix::fs::PermissionsExt;
    let root = dir();
    let dotfiles = root.path().join("dotfiles");
    std::fs::create_dir_all(&dotfiles).unwrap();
    let target = dotfiles.join("config.toml");
    std::fs::write(&target, "# kept\n").unwrap();
    std::fs::set_permissions(&target, std::fs::Permissions::from_mode(0o640)).unwrap();
    let home = root.path().join("home");
    std::fs::create_dir_all(&home).unwrap();
    let link = home.join("config.toml");
    std::os::unix::fs::symlink(&target, &link).unwrap();
    run(&link, add("direct", "k")).unwrap();
    assert!(
        std::fs::symlink_metadata(&link)
            .unwrap()
            .file_type()
            .is_symlink()
    );
    assert!(read(&target).starts_with("# kept\n"));
    assert!(read(&target).contains("[endpoints.direct]"));
    assert_eq!(
        std::fs::metadata(&target).unwrap().permissions().mode() & 0o777,
        0o640
    );
    assert!(home.join(CREDENTIALS).exists());
    assert!(!dotfiles.join(CREDENTIALS).exists());
    assert_eq!(
        records(&link)[&name("direct")].owner.as_deref(),
        Some(canonical(&target).as_path())
    );
    let held = lock(
        &[(dotfiles.as_path(), LockKind::Config)],
        PathShown::Default,
        WAIT,
    )
    .unwrap();
    assert_eq!(
        run(&link, add("other", "k")).unwrap_err(),
        ChangeError::Store(StoreError::Busy),
        "the link and its target share the config lock"
    );
    drop(held);
}

#[cfg(unix)]
#[test]
fn an_unwritable_target_folder_changes_nothing() {
    use std::os::unix::fs::PermissionsExt;
    let root = dir();
    let dotfiles = root.path().join("dotfiles");
    std::fs::create_dir_all(&dotfiles).unwrap();
    let target = dotfiles.join("config.toml");
    std::fs::write(&target, "").unwrap();
    let home = root.path().join("home");
    std::fs::create_dir_all(&home).unwrap();
    let link = home.join("config.toml");
    std::os::unix::fs::symlink(&target, &link).unwrap();
    std::fs::set_permissions(&dotfiles, std::fs::Permissions::from_mode(0o500)).unwrap();
    let probe = dotfiles.join("probe");
    let writable = std::fs::write(&probe, "").is_ok();
    let result = run(&link, Change::SetDefault { name: name("env") });
    std::fs::set_permissions(&dotfiles, std::fs::Permissions::from_mode(0o700)).unwrap();
    if writable {
        return;
    }
    let Err(ChangeError::Config(error)) = result else {
        panic!("{result:?}");
    };
    assert!(
        matches!(&error.problem, crate::config_file::WriteProblem::TargetNotWritable(note) if note.path == canonical(&target)),
        "{error:?}"
    );
    assert_eq!(read(&target), "");
    assert!(!home.join(crate::secrets::LOCK_FILE).exists());
    assert!(!dotfiles.join(crate::secrets::LOCK_FILE).exists());
}

#[test]
fn notes_and_errors_hide_a_given_path_in_debug() {
    let error = ChangeError::CantDetach {
        unreadable: vec![name("direct")],
        credentials: PathBuf::from("/tmp/CANARYFOLDER/credentials.json"),
        shown: PathShown::Given,
    };
    for shown in [
        format!("{error:?}"),
        format!("{error:#?}"),
        error.to_string(),
    ] {
        assert!(!shown.contains("CANARY"), "{shown}");
    }
    let note = DetachNote {
        from: vec![Some(PathBuf::from("/tmp/CANARYFOLDER/config.toml")), None],
    };
    assert_eq!(format!("{note:?}"), "DetachNote { from: 2 owners }");
}

#[test]
fn two_writers_through_one_copy_detach_once() {
    let root = dir();
    let original = root.path().join("config.toml");
    run(&original, add("direct", "k-direct")).unwrap();
    let copy = root.path().join("copy.toml");
    std::fs::copy(&original, &copy).unwrap();
    let original_id = store_id(&original);
    let (started, waiting) = std::sync::mpsc::channel::<()>();
    let first = {
        let copy = copy.clone();
        std::thread::spawn(move || {
            let hook = move || {
                started.send(()).unwrap();
                std::thread::sleep(Duration::from_millis(300));
                Ok(())
            };
            apply_with(
                &copy,
                PathShown::Default,
                Change::SetDefault {
                    name: name("direct"),
                },
                &NO_KEYCHAIN,
                WAIT,
                &hook,
            )
        })
    };
    waiting.recv().unwrap();
    let second = apply(
        &copy,
        PathShown::Default,
        add("more", "k-more"),
        &NO_KEYCHAIN,
        Duration::from_secs(10),
    )
    .unwrap();
    let first = first.join().unwrap().unwrap();
    assert!(first.detached.is_some());
    assert!(
        second.detached.is_none(),
        "the second write changes in place"
    );
    let copy_id = store_id(&copy);
    assert_ne!(copy_id, original_id);
    assert_eq!(store_id(&original), original_id);
    let copied = records(&copy);
    assert_eq!(copied.len(), 2);
    for stored in copied.values() {
        assert_eq!(stored.owner.as_deref(), Some(canonical(&copy).as_path()));
    }
    let text = read(&root.path().join(CREDENTIALS));
    assert_eq!(text.matches("k-direct").count(), 2, "{text}");
    assert_eq!(text.matches("k-more").count(), 1, "{text}");
    let config = read(&copy);
    assert!(config.contains("default = \"direct\""), "{config}");
    assert!(config.contains("[endpoints.more]"), "{config}");
}

#[cfg(unix)]
#[test]
fn a_hard_linked_credentials_file_gets_the_note() {
    let root = dir();
    let config = root.path().join("config.toml");
    run(&config, add("direct", "k-direct")).unwrap();
    let credentials = root.path().join(CREDENTIALS);
    std::fs::hard_link(&credentials, root.path().join("backup.json")).unwrap();
    let applied = run(&config, remove("direct")).unwrap();
    assert!(
        applied.notes.iter().any(
            |n| n.kind == crate::secrets::PathNoteKind::CredentialsHardLinks
                && n.path == credentials
        ),
        "{:?}",
        applied.notes
    );
    assert!(read(&root.path().join("backup.json")).contains("k-direct"));
}

#[test]
fn a_copy_that_removed_its_last_endpoint_adds_under_its_own_store() {
    let root = dir();
    let original = root.path().join("config.toml");
    run(&original, add("direct", "k-direct")).unwrap();
    let original_id = store_id(&original);
    let copy = root.path().join("copy.toml");
    std::fs::copy(&original, &copy).unwrap();
    run(&copy, remove("direct")).unwrap();
    let added = run(&copy, add("work", "k-work")).unwrap();
    assert!(added.detached.is_none());
    assert_ne!(store_id(&copy), original_id);
    assert_eq!(key_of(&copy, "work").as_deref(), Some("k-work"));
    assert_eq!(store_id(&original), original_id);
    let originals = records(&original);
    assert_eq!(
        originals.len(),
        1,
        "work never lands in the original's store"
    );
    let more = run(&original, add("more", "k-more")).unwrap();
    assert!(more.detached.is_none(), "the original owns its records");
    assert_eq!(store_id(&original), original_id);
    assert_eq!(key_of(&original, "direct").as_deref(), Some("k-direct"));
}

fn put_by_hand(config: &Path, endpoint: &str, key: &str) {
    let id = store_id(config).unwrap();
    let locks = lock(
        &[(config.parent().unwrap(), LockKind::Credentials)],
        PathShown::Default,
        WAIT,
    )
    .unwrap();
    store_of(config)
        .put(
            &locks,
            &id,
            &name(endpoint),
            Stored {
                record: record(key),
                owner: None,
            },
        )
        .unwrap();
}

#[test]
fn a_detach_names_only_the_records_it_copied() {
    let root = dir();
    let original = root.path().join("config.toml");
    run(&original, add("direct", "k-direct")).unwrap();
    let copy = root.path().join("copy.toml");
    std::fs::write(
        &copy,
        format!("store_id = \"{}\"\n", store_id(&original).unwrap().as_str()),
    )
    .unwrap();
    let applied = run(&copy, add("direct", "k-copy")).unwrap();
    assert!(applied.detached.is_none(), "nothing was copied");
    assert_ne!(store_id(&copy), store_id(&original));
    assert_eq!(key_of(&original, "direct").as_deref(), Some("k-direct"));

    let root = dir();
    let original = root.path().join("config.toml");
    run(&original, add("a", "k-a")).unwrap();
    put_by_hand(&original, "b", "k-b");
    let copy = root.path().join("copy.toml");
    std::fs::copy(&original, &copy).unwrap();
    let applied = run(&copy, add("b", "k-b2")).unwrap();
    assert_eq!(
        applied.detached,
        Some(DetachNote {
            from: vec![Some(canonical(&original))]
        }),
        "the hand-written b was not copied"
    );
}

#[test]
fn check_refuses_without_taking_a_lock() {
    let root = dir();
    let original = root.path().join("a/config.toml");
    run(&original, add("a1", "k1")).unwrap();
    run(&original, add("a2", "k2")).unwrap();
    std::fs::create_dir_all(root.path().join("b")).unwrap();
    let copy = root.path().join("b/config.toml");
    std::fs::copy(&original, &copy).unwrap();
    let change = Change::SetDefault { name: name("a2") };
    let error = check(&copy, PathShown::Default, &change, &NO_KEYCHAIN).unwrap_err();
    assert!(matches!(error, ChangeError::CantDetach { .. }), "{error:?}");
    assert!(
        !root
            .path()
            .join("b")
            .join(crate::secrets::LOCK_FILE)
            .exists()
    );
    let error = check(&copy, PathShown::Default, &remove("typo"), &NO_KEYCHAIN).unwrap_err();
    assert_eq!(error, ChangeError::NoSuchEndpoint(name("typo")));
    assert!(
        !root
            .path()
            .join("b")
            .join(crate::secrets::LOCK_FILE)
            .exists()
    );
    assert!(check(&original, PathShown::Default, &change, &NO_KEYCHAIN).is_ok());
}

#[test]
fn a_refusal_under_the_locks_returns_their_notes() {
    let root = dir();
    crate::instructions::make_repository(root.path()).unwrap();
    let original = root.path().join("a/config.toml");
    run(&original, add("a1", "k1")).unwrap();
    std::fs::create_dir_all(root.path().join("b")).unwrap();
    let copy = root.path().join("b/config.toml");
    std::fs::copy(&original, &copy).unwrap();
    let failed = apply_noting(
        &copy,
        PathShown::Default,
        Change::SetDefault { name: name("a1") },
        &NO_KEYCHAIN,
        WAIT,
    )
    .unwrap_err();
    assert!(matches!(failed.error, ChangeError::CantDetach { .. }));
    assert!(
        failed
            .notes
            .iter()
            .any(|n| matches!(n.kind, crate::secrets::PathNoteKind::LockInRepo(_))),
        "{:?}",
        failed.notes
    );
}

#[cfg(unix)]
#[test]
fn a_lock_timeout_keeps_the_note_for_a_lock_file_it_created() {
    let root = dir();
    crate::instructions::make_repository(root.path()).unwrap();
    std::fs::create_dir_all(root.path().join("a")).unwrap();
    std::fs::create_dir_all(root.path().join("b")).unwrap();
    let target = root.path().join("b/config.toml");
    std::fs::write(&target, "").unwrap();
    let link = root.path().join("a/config.toml");
    std::os::unix::fs::symlink(&target, &link).unwrap();
    let held = lock(
        &[(root.path().join("b").as_path(), LockKind::Config)],
        PathShown::Default,
        WAIT,
    )
    .unwrap();
    let failed = apply_noting(
        &link,
        PathShown::Default,
        Change::SetDefault {
            name: EndpointName::env(),
        },
        &NO_KEYCHAIN,
        Duration::from_millis(100),
    )
    .unwrap_err();
    drop(held);
    assert_eq!(failed.error, ChangeError::Store(StoreError::Busy));
    assert_eq!(
        failed.notes.iter().map(|n| n.kind).collect::<Vec<_>>(),
        [crate::secrets::PathNoteKind::LockInRepo(
            LockKind::Credentials
        )]
    );
    assert!(
        root.path()
            .join("a")
            .join(crate::secrets::LOCK_FILE)
            .exists()
    );
}

fn edit(text: &str, edit: Edit) -> Change {
    Change::Edit {
        name: name(text),
        edit,
    }
}

fn record_with_url(key: &str, url: &str) -> StoredRecord {
    StoredRecord::Key {
        key: Secret::new(key),
        url: Some(Secret::new(url)),
    }
}

fn url_of(config: &Path, endpoint: &str) -> Option<String> {
    records(config).get(&name(endpoint)).and_then(|s| {
        let StoredRecord::Key { url, .. } = &s.record;
        url.as_ref().map(|u| u.expose().to_owned())
    })
}

#[test]
fn a_rename_writes_the_new_record_then_the_table_then_deletes_the_old_one() {
    let root = dir();
    let config = root.path().join("config.toml");
    run(&config, add("old", "k-old")).unwrap();
    run(&config, Change::SetDefault { name: name("old") }).unwrap();
    let rename = |to: &str| Edit {
        rename: Some(name(to)),
        ..Edit::default()
    };
    let failed = apply_with(
        &config,
        PathShown::Default,
        edit("old", rename("new")),
        &NO_KEYCHAIN,
        WAIT,
        &|| Err(ChangeError::Interrupted),
    );
    assert_eq!(failed.unwrap_err(), ChangeError::Interrupted);
    assert!(read(&config).contains("[endpoints.old]"));
    assert_eq!(
        key_of(&config, "new").as_deref(),
        Some("k-old"),
        "an orphan at most"
    );
    assert_eq!(key_of(&config, "old").as_deref(), Some("k-old"));
    let applied = run(&config, edit("old", rename("new"))).unwrap();
    assert_eq!(applied.outcome, Outcome::Edited { name: name("new") });
    let text = read(&config);
    assert!(text.contains("default = \"new\""), "{text}");
    assert!(text.contains("[endpoints.new]"), "{text}");
    assert!(!text.contains("old"), "{text}");
    assert_eq!(key_of(&config, "new").as_deref(), Some("k-old"));
    assert_eq!(key_of(&config, "old"), None);
    run(&config, add("other", "k")).unwrap();
    assert_eq!(
        run(&config, edit("new", rename("other"))).unwrap_err(),
        ChangeError::NameTaken(name("other"))
    );
}

#[test]
fn moving_a_url_out_of_config_toml_preserves_the_old_pair_on_failure() {
    let root = dir();
    let config = root.path().join("config.toml");
    run(&config, add("gw", "k-gw")).unwrap();
    let text = read(&config).replace(
        "key_store = \"file\"\n",
        "key_store = \"file\"\nurl = \"https://old.example.com\"\n",
    );
    std::fs::write(&config, text).unwrap();
    let move_url = Edit {
        record: Some(record_with_url("k-gw", "https://new.example.com/p")),
        url: Some(None),
        ..Edit::default()
    };
    let failed = apply_with(
        &config,
        PathShown::Default,
        edit("gw", move_url.clone()),
        &NO_KEYCHAIN,
        WAIT,
        &|| Err(ChangeError::Interrupted),
    );
    assert_eq!(failed.unwrap_err(), ChangeError::Interrupted);
    assert!(read(&config).contains("url = \"https://old.example.com\""));
    assert_eq!(url_of(&config, "gw").as_deref(), None);
    let parsed = ConfigText::read(&config, PathShown::Default)
        .unwrap()
        .current()
        .unwrap();
    let secrets = crate::secrets::Prefetched::load(Some(&parsed));
    let entries = crate::endpoints::EndpointBook::entries(
        Some(&parsed),
        &secrets,
        &absent(&|_| None, None, false),
    );
    assert!(matches!(
        &entries[&name("gw")],
        crate::endpoints::Availability::Ready(_)
    ));
    run(&config, edit("gw", move_url)).unwrap();
    assert!(!read(&config).contains("url ="), "{}", read(&config));
    assert_eq!(
        url_of(&config, "gw").as_deref(),
        Some("https://new.example.com/p")
    );
}

#[test]
fn edits_change_fields_and_keep_the_rest() {
    let root = dir();
    let config = root.path().join("config.toml");
    run(&config, add("direct", "k")).unwrap();
    let id = store_id(&config);
    let credentials = read(&root.path().join(CREDENTIALS));
    run(
        &config,
        edit(
            "direct",
            Edit {
                auth: Some(AuthKind::Bearer),
                pricing: Some(PricingMode::None),
                ..Edit::default()
            },
        ),
    )
    .unwrap();
    assert_eq!(
        read(&config).split_once("[endpoints.direct]").unwrap().1,
        "\nauth = \"bearer\"\nkey_store = \"file\"\npricing = \"none\"\n"
    );
    assert_eq!(read(&root.path().join(CREDENTIALS)), credentials);
    run(
        &config,
        edit(
            "direct",
            Edit {
                pricing: Some(PricingMode::List),
                record: Some(record("k-new")),
                ..Edit::default()
            },
        ),
    )
    .unwrap();
    assert!(!read(&config).contains("pricing"));
    assert_eq!(key_of(&config, "direct").as_deref(), Some("k-new"));
    assert_ne!(store_id(&config), id);
    assert_eq!(
        run(&config, edit("nope", Edit::default())).unwrap_err(),
        ChangeError::NoSuchEndpoint(name("nope"))
    );
}

#[test]
fn a_rename_without_a_readable_record_is_refused() {
    let root = dir();
    let config = root.path().join("config.toml");
    run(&config, add("direct", "k")).unwrap();
    run(&config, add("lost", "k-lost")).unwrap();
    let id = store_id(&config).unwrap();
    let locks = lock(
        &[(root.path(), LockKind::Credentials)],
        PathShown::Default,
        WAIT,
    )
    .unwrap();
    store_of(&config)
        .remove(&locks, &id, &name("lost"))
        .unwrap();
    drop(locks);
    let before = read(&config);
    let error = run(
        &config,
        edit(
            "lost",
            Edit {
                rename: Some(name("found")),
                ..Edit::default()
            },
        ),
    )
    .unwrap_err();
    assert_eq!(error, ChangeError::NoRecord(name("lost")));
    assert!(
        error
            .to_string()
            .ends_with("wisp account edit lost --replace-key"),
        "{error}"
    );
    assert_eq!(read(&config), before);
    let applied = run(
        &config,
        edit(
            "lost",
            Edit {
                record: Some(record("k-again")),
                ..Edit::default()
            },
        ),
    )
    .unwrap();
    assert!(applied.detached.is_none(), "no needless detach");
    assert_eq!(store_id(&config).unwrap(), id);
    assert_eq!(key_of(&config, "lost").as_deref(), Some("k-again"));
}

#[test]
fn a_new_key_through_a_copy_from_another_folder_goes_ahead_only_for_its_own_record() {
    let root = dir();
    let original = root.path().join("a/config.toml");
    run(&original, add("direct", "k-direct")).unwrap();
    std::fs::create_dir_all(root.path().join("b")).unwrap();
    let copy = root.path().join("b/config.toml");
    std::fs::copy(&original, &copy).unwrap();
    let applied = run(
        &copy,
        edit(
            "direct",
            Edit {
                record: Some(record("k-copy")),
                ..Edit::default()
            },
        ),
    )
    .unwrap();
    assert!(applied.detached.is_none(), "nothing to copy");
    assert_eq!(key_of(&copy, "direct").as_deref(), Some("k-copy"));
    assert_eq!(key_of(&original, "direct").as_deref(), Some("k-direct"));

    run(&original, add("work", "k-work")).unwrap();
    let second = root.path().join("c/config.toml");
    std::fs::create_dir_all(root.path().join("c")).unwrap();
    std::fs::copy(&original, &second).unwrap();
    let before = read(&second);
    let error = run(
        &second,
        edit(
            "direct",
            Edit {
                record: Some(record("k")),
                ..Edit::default()
            },
        ),
    )
    .unwrap_err();
    assert!(matches!(error, ChangeError::CantDetach { .. }), "{error:?}");
    assert_eq!(read(&second), before);
}

#[test]
fn a_new_key_through_a_same_folder_copy_detaches_and_keeps_the_originals_key() {
    let root = dir();
    let original = root.path().join("config.toml");
    run(&original, add("direct", "k-direct")).unwrap();
    run(&original, add("work", "k-work")).unwrap();
    let copy = root.path().join("copy.toml");
    std::fs::copy(&original, &copy).unwrap();
    let applied = run(
        &copy,
        edit(
            "direct",
            Edit {
                record: Some(record("k-copy")),
                ..Edit::default()
            },
        ),
    )
    .unwrap();
    assert!(applied.detached.is_some());
    assert_ne!(store_id(&copy), store_id(&original));
    assert_eq!(key_of(&copy, "direct").as_deref(), Some("k-copy"));
    assert_eq!(key_of(&copy, "work").as_deref(), Some("k-work"));
    assert_eq!(key_of(&original, "direct").as_deref(), Some("k-direct"));
}

#[test]
fn a_variable_table_is_converted_to_a_stored_one_keeping_comments() {
    let root = dir();
    let config = root.path().join("config.toml");
    std::fs::write(
        &config,
        "# mine\n[endpoints.gw]\nauth = \"bearer\" # a token\n# the work key\nkey_env = \"GW_KEY\"\n# the gateway\nurl_env = \"GW_URL\"\n",
    )
    .unwrap();
    run(
        &config,
        edit(
            "gw",
            Edit {
                record: Some(record_with_url("k-gw", "https://gw.example.com")),
                store: Some(StoreKind::File),
                ..Edit::default()
            },
        ),
    )
    .unwrap();
    let text = read(&config);
    assert!(text.starts_with("store_id = "), "{text}");
    assert!(
        text.ends_with(
            "# mine\n[endpoints.gw]\nauth = \"bearer\" # a token\n# the work key\n# the gateway\nkey_store = \"file\"\n"
        ),
        "{text}"
    );
    assert_eq!(key_of(&config, "gw").as_deref(), Some("k-gw"));
    assert_eq!(
        url_of(&config, "gw").as_deref(),
        Some("https://gw.example.com")
    );
}

#[test]
fn a_variable_table_edits_names_and_literal_fields_only() {
    let root = dir();
    let config = root.path().join("config.toml");
    std::fs::write(
        &config,
        "default = \"gw\"\n[endpoints.gw]\nauth = \"bearer\"\nkey_env = \"GW_KEY\"\nurl_env = \"GW_URL\"\n",
    )
    .unwrap();
    let var = |t: &str| VariableName::parse(t).unwrap();
    run(
        &config,
        edit(
            "gw",
            Edit {
                key_env: Some(var("NEW_KEY")),
                url: Some(Some("https://gw.example.com".to_owned())),
                url_env: Some(None),
                rename: Some(name("work")),
                ..Edit::default()
            },
        ),
    )
    .unwrap();
    assert_eq!(
        read(&config),
        "default = \"work\"\n[endpoints.work]\nauth = \"bearer\"\nkey_env = \"NEW_KEY\"\nurl = \"https://gw.example.com\"\n"
    );
    assert!(!root.path().join(CREDENTIALS).exists());
    run(
        &config,
        edit(
            "work",
            Edit {
                url_env: Some(Some(var("GW_URL"))),
                ..Edit::default()
            },
        ),
    )
    .unwrap();
    assert_eq!(
        read(&config),
        "default = \"work\"\n[endpoints.work]\nauth = \"bearer\"\nkey_env = \"NEW_KEY\"\nurl_env = \"GW_URL\"\n"
    );
}

#[test]
fn add_table_writes_what_a_hand_written_table_says() {
    let root = dir();
    let config = root.path().join("config.toml");
    run(
        &config,
        Change::AddTable {
            name: name("gw"),
            table: NewTable {
                provider: ProviderKind::Anthropic,
                auth: AuthKind::Bearer,
                key_env: VariableName::parse("GW_KEY").unwrap(),
                url: None,
                url_env: VariableName::parse("GW_URL"),
                pricing: PricingMode::List,
            },
            make_default: true,
        },
    )
    .unwrap();
    assert_eq!(
        read(&config),
        "default = \"gw\"\n\n[endpoints.gw]\nauth = \"bearer\"\nkey_env = \"GW_KEY\"\nurl_env = \"GW_URL\"\n"
    );
    assert!(!root.path().join(CREDENTIALS).exists());
}

mod keychain {
    use super::*;
    use crate::secrets::keyring::{
        BackendError, Budget, KeyringStore, MemoryKeyring, MemoryMode, Unreachable, account, decode,
    };

    const LONG: Duration = Duration::from_secs(30);

    fn keyring_add(text: &str, key: &str) -> Change {
        let Change::Add {
            name,
            mut endpoint,
            record,
            make_default,
        } = add(text, key)
        else {
            unreachable!()
        };
        endpoint.store = StoreKind::Keyring;
        Change::Add {
            name,
            endpoint,
            record,
            make_default,
        }
    }

    fn with(
        config: &Path,
        change: Change,
        keys: &dyn KeyringAccess,
    ) -> Result<Applied, ChangeError> {
        apply(config, PathShown::Default, change, keys, WAIT)
    }

    fn working(memory: &MemoryKeyring) -> KeyringStore {
        memory.store(LONG)
    }

    fn locked(memory: &MemoryKeyring) -> KeyringStore {
        memory.set_mode(MemoryMode::Failing(BackendError::NoStorageAccess));
        memory.store(LONG)
    }

    fn item(memory: &MemoryKeyring, config: &Path, endpoint: &str) -> Option<Stored> {
        let id = store_id(config)?;
        memory
            .items()
            .get(&account(&id, &name(endpoint)))
            .map(|text| decode(text, &name(endpoint)).unwrap())
    }

    fn item_key(memory: &MemoryKeyring, config: &Path, endpoint: &str) -> Option<String> {
        item(memory, config, endpoint).map(|s| {
            let StoredRecord::Key { key, .. } = &s.record;
            key.expose().to_owned()
        })
    }

    fn remove_keeping(text: &str) -> Change {
        Change::Remove {
            name: name(text),
            keep_key: true,
            chat: None,
        }
    }

    fn setup(memory: &MemoryKeyring) -> (tempfile::TempDir, PathBuf) {
        let root = dir();
        let config = root.path().join("config.toml");
        let keys = working(memory);
        with(&config, keyring_add("direct", "k-direct"), &keys).unwrap();
        with(&config, add("file", "k-file"), &keys).unwrap();
        (root, config)
    }

    struct Snapshot {
        config: String,
        credentials: String,
        items: BTreeMap<String, String>,
    }

    fn snapshot(memory: &MemoryKeyring, config: &Path) -> Snapshot {
        Snapshot {
            config: read(config),
            credentials: read(&config.parent().unwrap().join(CREDENTIALS)),
            items: memory.items(),
        }
    }

    fn unchanged(memory: &MemoryKeyring, config: &Path, before: &Snapshot) {
        assert_eq!(read(config), before.config, "config.toml changed");
        assert_eq!(
            read(&config.parent().unwrap().join(CREDENTIALS)),
            before.credentials,
            "credentials.json changed"
        );
        assert_eq!(memory.items(), before.items, "the keychain changed");
    }

    #[test]
    fn add_to_the_keychain_writes_the_item_owned_by_this_config_then_the_table() {
        let memory = MemoryKeyring::new();
        let root = dir();
        let config = root.path().join("config.toml");
        with(
            &config,
            keyring_add("direct", "k-direct"),
            &working(&memory),
        )
        .unwrap();
        let text = read(&config);
        assert!(text.contains("key_store = \"keyring\""), "{text}");
        let stored = item(&memory, &config, "direct").unwrap();
        assert_eq!(stored.owner.as_deref(), Some(canonical(&config).as_path()));
        assert_eq!(
            item_key(&memory, &config, "direct").as_deref(),
            Some("k-direct")
        );
        assert!(!root.path().join(CREDENTIALS).exists());
    }

    #[test]
    fn a_keychain_that_fails_a_write_changes_nothing() {
        let memory = MemoryKeyring::new();
        let root = dir();
        let config = root.path().join("config.toml");
        let error = with(&config, keyring_add("direct", "k"), &locked(&memory)).unwrap_err();
        assert_eq!(
            error,
            ChangeError::KeychainWrite {
                name: name("direct"),
                error: KeyringError::Locked
            }
        );
        assert!(!config.exists());
        assert!(memory.items().is_empty());
        let down = Unreachable(KeyringError::TimedOut);
        let error = with(&config, keyring_add("direct", "k"), &down).unwrap_err();
        assert_eq!(
            error.to_string(),
            "the keychain didn't answer, so nothing was saved"
        );
    }

    #[test]
    fn a_failure_between_the_writes_of_a_keychain_add_leaves_an_orphan_item() {
        let memory = MemoryKeyring::new();
        let root = dir();
        let config = root.path().join("config.toml");
        with(&config, add("file", "k-file"), &working(&memory)).unwrap();
        let before = read(&config);
        let failed = apply_with(
            &config,
            PathShown::Default,
            keyring_add("direct", "k"),
            &working(&memory),
            WAIT,
            &|| Err(ChangeError::Interrupted),
        );
        assert_eq!(failed.unwrap_err(), ChangeError::Interrupted);
        assert_eq!(read(&config), before, "no table names a missing item");
        assert!(item(&memory, &config, "direct").is_some(), "an orphan item");
    }

    #[test]
    fn remove_deletes_the_item_and_keep_key_always_leaves_it() {
        let memory = MemoryKeyring::new();
        let (_root, config) = setup(&memory);
        let id = store_id(&config).unwrap();
        let applied = with(&config, remove_keeping("direct"), &locked(&memory)).unwrap();
        assert_eq!(
            applied.outcome,
            Outcome::Removed {
                store: Some(StoreKind::Keyring),
                deleted: false,
                was_default: false,
                kept: Some(format!("{}/direct", id.as_str())),
                new_start: None,
            }
        );
        assert!(!read(&config).contains("[endpoints.direct]"));
        memory.set_mode(MemoryMode::Working);
        assert!(item(&memory, &config, "direct").is_some(), "kept");
        let applied = with(&config, remove("direct"), &working(&memory)).unwrap();
        assert_eq!(applied.outcome, Outcome::LeftoverRemoved);
        assert!(item(&memory, &config, "direct").is_none());

        let memory = MemoryKeyring::new();
        let (_root, config) = setup(&memory);
        let applied = with(&config, remove("direct"), &working(&memory)).unwrap();
        assert!(matches!(
            applied.outcome,
            Outcome::Removed {
                store: Some(StoreKind::Keyring),
                deleted: true,
                kept: None,
                ..
            }
        ));
        assert!(memory.items().is_empty());
    }

    #[test]
    fn keep_key_is_refused_for_a_file_or_variable_endpoint() {
        let memory = MemoryKeyring::new();
        let (_root, config) = setup(&memory);
        let mut text = read(&config);
        text.push_str("\n[endpoints.work]\nauth = \"api_key\"\nkey_env = \"WORK_KEY\"\n");
        std::fs::write(&config, &text).unwrap();
        let before = snapshot(&memory, &config);
        let error = with(&config, remove_keeping("file"), &working(&memory)).unwrap_err();
        assert_eq!(
            error.to_string(),
            "--keep-key is for keys in the keychain; file's key is in credentials.json"
        );
        let error = with(&config, remove_keeping("work"), &working(&memory)).unwrap_err();
        assert_eq!(
            error.to_string(),
            "--keep-key is for keys in the keychain; work's key is in a variable, which remove never touches"
        );
        unchanged(&memory, &config, &before);
    }

    #[test]
    fn remove_of_an_unreachable_item_is_refused_naming_keep_key() {
        let memory = MemoryKeyring::new();
        let (_root, config) = setup(&memory);
        let before = snapshot(&memory, &config);
        let error = with(&config, remove("direct"), &locked(&memory)).unwrap_err();
        assert_eq!(
            error,
            ChangeError::Keychain {
                name: name("direct"),
                error: KeyringError::Locked,
                keep_key: true
            }
        );
        assert!(error.to_string().contains("--keep-key"), "{error}");
        unchanged(&memory, &config, &before);
        let off = Unreachable(KeyringError::Off {
            origin: "WISP_KEYCHAIN".to_owned(),
        });
        let error = with(&config, remove("direct"), &off).unwrap_err();
        assert!(
            error
                .to_string()
                .starts_with("direct's key is in the keychain, which is off (WISP_KEYCHAIN); run this with --keychain on"),
            "{error}"
        );
        unchanged(&memory, &config, &before);
    }

    #[test]
    fn writes_that_need_an_unreadable_item_are_refused_and_others_go_ahead() {
        let memory = MemoryKeyring::new();
        let (_root, config) = setup(&memory);
        let id = store_id(&config).unwrap();
        let keys = locked(&memory);
        let refused = [
            remove("direct"),
            edit(
                "direct",
                Edit {
                    record: Some(record("k-new")),
                    ..Edit::default()
                },
            ),
            edit(
                "direct",
                Edit {
                    store: Some(StoreKind::File),
                    ..Edit::default()
                },
            ),
            edit(
                "direct",
                Edit {
                    rename: Some(name("renamed")),
                    ..Edit::default()
                },
            ),
            edit(
                "direct",
                Edit {
                    provider: Some(ProviderKind::Anthropic),
                    ..Edit::default()
                },
            ),
        ];
        for change in refused {
            let before = snapshot(&memory, &config);
            let error = with(&config, change.clone(), &keys).unwrap_err();
            assert!(
                matches!(
                    &error,
                    ChangeError::Keychain {
                        error: KeyringError::Locked,
                        ..
                    }
                ),
                "{change:?}: {error:?}"
            );
            unchanged(&memory, &config, &before);
        }
        with(
            &config,
            edit(
                "direct",
                Edit {
                    pricing: Some(PricingMode::None),
                    ..Edit::default()
                },
            ),
            &keys,
        )
        .unwrap();
        assert!(read(&config).contains("pricing = \"none\""));
        with(
            &config,
            Change::SetDefault {
                name: name("direct"),
            },
            &keys,
        )
        .unwrap();
        let applied = with(&config, add("new", "k-new"), &keys).unwrap();
        assert!(applied.detached.is_none());
        assert_eq!(store_id(&config), Some(id.clone()));
        let stored = &records(&config)[&name("new")];
        assert_eq!(stored.owner.as_deref(), Some(canonical(&config).as_path()));
    }

    #[test]
    fn an_add_over_another_configs_record_is_refused_while_an_item_is_unreadable() {
        let memory = MemoryKeyring::new();
        let (root, config) = setup(&memory);
        let id = store_id(&config).unwrap();
        let locks = lock(
            &[(root.path(), LockKind::Credentials)],
            PathShown::Default,
            WAIT,
        )
        .unwrap();
        store_of(&config)
            .change(&locks, |every| {
                every.entry(id.clone()).or_default().insert(
                    name("new"),
                    Stored {
                        record: record("k-other"),
                        owner: Some(root.path().join("other.toml")),
                    },
                );
            })
            .unwrap();
        drop(locks);
        let before = snapshot(&memory, &config);
        let error = with(&config, add("new", "k-new"), &locked(&memory)).unwrap_err();
        assert!(
            matches!(error, ChangeError::CantDetachKeychain { .. }),
            "{error:?}"
        );
        unchanged(&memory, &config, &before);
    }

    fn copy_of(config: &Path) -> PathBuf {
        let copy = config.parent().unwrap().join("copy.toml");
        std::fs::copy(config, &copy).unwrap();
        copy
    }

    #[test]
    fn a_copy_cannot_detach_while_the_keychain_is_down_and_can_once_it_answers() {
        let memory = MemoryKeyring::new();
        let (_root, original) = setup(&memory);
        let original_id = store_id(&original).unwrap();
        let copy = copy_of(&original);
        let before = snapshot(&memory, &copy);
        let error = with(
            &copy,
            Change::SetDefault { name: name("file") },
            &locked(&memory),
        )
        .unwrap_err();
        assert_eq!(
            error,
            ChangeError::CantDetachKeychain {
                unreadable: vec![name("direct")],
                error: KeyringError::Locked
            }
        );
        unchanged(&memory, &copy, &before);
        let off = Unreachable(KeyringError::Off {
            origin: "--keychain".to_owned(),
        });
        let error = with(&copy, Change::SetDefault { name: name("file") }, &off).unwrap_err();
        assert!(error.to_string().contains("the keychain is off"), "{error}");
        assert!(!error.to_string().contains("didn't answer"), "{error}");
        memory.set_mode(MemoryMode::Working);
        let applied = with(
            &copy,
            Change::SetDefault { name: name("file") },
            &working(&memory),
        )
        .unwrap();
        assert!(applied.detached.is_some());
        let copy_id = store_id(&copy).unwrap();
        assert_ne!(copy_id, original_id);
        let copied = item(&memory, &copy, "direct").unwrap();
        assert_eq!(copied.owner.as_deref(), Some(canonical(&copy).as_path()));
        assert_eq!(
            item_key(&memory, &copy, "direct").as_deref(),
            Some("k-direct")
        );
        assert_eq!(key_of(&copy, "file").as_deref(), Some("k-file"));
        assert_eq!(
            item(&memory, &original, "direct").unwrap().owner.as_deref(),
            Some(canonical(&original).as_path()),
            "the original's item is untouched"
        );
    }

    #[test]
    fn the_detach_exception_is_for_file_reasons_only() {
        let memory = MemoryKeyring::new();
        let (_root, original) = setup(&memory);
        let copy = copy_of(&original);
        let id = store_id(&copy);
        let keys = locked(&memory);
        for change in [
            edit(
                "direct",
                Edit {
                    record: Some(record("k-new")),
                    ..Edit::default()
                },
            ),
            remove("direct"),
        ] {
            let before = snapshot(&memory, &copy);
            let error = with(&copy, change.clone(), &keys).unwrap_err();
            assert!(
                matches!(
                    error,
                    ChangeError::Keychain { .. } | ChangeError::CantDetachKeychain { .. }
                ),
                "{change:?}: {error:?}"
            );
            assert!(error.to_string().contains("locked"), "{error}");
            assert!(
                !error.to_string().contains("--keep-key") || matches!(change, Change::Edit { .. }),
                "{error}"
            );
            unchanged(&memory, &copy, &before);
            assert_eq!(store_id(&copy), id, "no new store_id");
        }
    }

    #[test]
    fn a_store_move_writes_the_new_store_then_the_config_then_deletes_the_old() {
        let memory = MemoryKeyring::new();
        let (_root, config) = setup(&memory);
        let to_file = edit(
            "direct",
            Edit {
                store: Some(StoreKind::File),
                ..Edit::default()
            },
        );
        let before = read(&config);
        let failed = apply_with(
            &config,
            PathShown::Default,
            to_file.clone(),
            &working(&memory),
            WAIT,
            &|| Err(ChangeError::Interrupted),
        );
        assert_eq!(failed.unwrap_err(), ChangeError::Interrupted);
        assert_eq!(read(&config), before);
        assert!(
            item(&memory, &config, "direct").is_some(),
            "the old store is kept"
        );
        let applied = with(&config, to_file, &working(&memory)).unwrap();
        assert!(applied.left.is_empty());
        let text = read(&config);
        assert!(!text.contains("key_store = \"keyring\""), "{text}");
        assert_eq!(key_of(&config, "direct").as_deref(), Some("k-direct"));
        assert!(item(&memory, &config, "direct").is_none());

        let to_keyring = edit(
            "file",
            Edit {
                store: Some(StoreKind::Keyring),
                ..Edit::default()
            },
        );
        let before = read(&config);
        let failed = apply_with(
            &config,
            PathShown::Default,
            to_keyring.clone(),
            &working(&memory),
            WAIT,
            &|| Err(ChangeError::Interrupted),
        );
        assert_eq!(failed.unwrap_err(), ChangeError::Interrupted);
        assert_eq!(read(&config), before);
        assert_eq!(key_of(&config, "file").as_deref(), Some("k-file"));
        with(&config, to_keyring, &working(&memory)).unwrap();
        assert_eq!(
            item_key(&memory, &config, "file").as_deref(),
            Some("k-file")
        );
        assert!(key_of(&config, "file").is_none());
    }

    struct NoDelete(KeyringStore);

    impl KeyringAccess for NoDelete {
        fn read(&self, id: &StoreId, name: &EndpointName) -> Result<Option<Stored>, KeyringError> {
            self.0.read(id, name, Budget::Start)
        }

        fn write(
            &self,
            id: &StoreId,
            name: &EndpointName,
            stored: &Stored,
        ) -> Result<(), KeyringError> {
            self.0.write(id, name, stored, Budget::Start)
        }

        fn delete(&self, _: &StoreId, _: &EndpointName) -> Result<bool, KeyringError> {
            Err(KeyringError::TimedOut)
        }
    }

    #[test]
    fn an_old_item_that_cant_be_deleted_after_a_move_is_reported_never_hidden() {
        let memory = MemoryKeyring::new();
        let (_root, config) = setup(&memory);
        let id = store_id(&config).unwrap();
        let keys = NoDelete(working(&memory));
        let applied = with(
            &config,
            edit(
                "direct",
                Edit {
                    store: Some(StoreKind::File),
                    ..Edit::default()
                },
            ),
            &keys,
        )
        .unwrap();
        assert_eq!(applied.left.len(), 1);
        assert_eq!(applied.left[0].name, name("direct"));
        assert_eq!(applied.left[0].account, format!("{}/direct", id.as_str()));
        assert_eq!(
            applied.left[0].error,
            StoreError::Keychain(KeyringError::TimedOut)
        );
        assert_eq!(key_of(&config, "direct").as_deref(), Some("k-direct"));
        let renamed = with(
            &config,
            edit(
                "file",
                Edit {
                    store: Some(StoreKind::Keyring),
                    ..Edit::default()
                },
            ),
            &keys,
        )
        .unwrap();
        assert!(
            renamed.left.is_empty(),
            "a file delete never goes through the keychain"
        );
    }

    #[test]
    fn a_rename_moves_the_item_to_its_new_account() {
        let memory = MemoryKeyring::new();
        let (_root, config) = setup(&memory);
        with(
            &config,
            edit(
                "direct",
                Edit {
                    rename: Some(name("main")),
                    ..Edit::default()
                },
            ),
            &working(&memory),
        )
        .unwrap();
        assert!(item(&memory, &config, "direct").is_none());
        assert_eq!(
            item_key(&memory, &config, "main").as_deref(),
            Some("k-direct")
        );
        assert!(read(&config).contains("[endpoints.main]"));
    }

    #[test]
    fn a_newer_or_malformed_item_is_never_rewritten_or_deleted() {
        for text in [r#"{"v":9,"key":"k"}"#, r#"{"v":1}"#] {
            let memory = MemoryKeyring::new();
            let (_root, config) = setup(&memory);
            let id = store_id(&config).unwrap();
            memory.insert(&account(&id, &name("direct")), text);
            let before = snapshot(&memory, &config);
            for change in [
                remove("direct"),
                edit(
                    "direct",
                    Edit {
                        store: Some(StoreKind::File),
                        ..Edit::default()
                    },
                ),
                edit(
                    "direct",
                    Edit {
                        record: Some(record("k-new")),
                        ..Edit::default()
                    },
                ),
            ] {
                let error = with(&config, change, &working(&memory)).unwrap_err();
                assert!(
                    error.to_string().contains("the keychain item for direct"),
                    "{error}"
                );
                unchanged(&memory, &config, &before);
            }
        }
    }

    #[test]
    fn the_session_reads_each_item_once_per_write() {
        let memory = MemoryKeyring::new();
        let (_root, config) = setup(&memory);
        let keys = working(&memory);
        let calls = memory.state().calls;
        with(&config, Change::SetDefault { name: name("file") }, &keys).unwrap();
        with(
            &config,
            Change::SetDefault {
                name: name("direct"),
            },
            &keys,
        )
        .unwrap();
        assert_eq!(memory.state().calls, calls + 1);
    }

    #[test]
    fn a_write_long_after_start_up_still_reaches_the_keychain() {
        let memory = MemoryKeyring::new();
        let keys = memory.store(Duration::from_millis(100));
        std::thread::sleep(Duration::from_millis(250));
        let root = dir();
        let config = root.path().join("config.toml");
        with(&config, keyring_add("direct", "k-direct"), &keys).unwrap();
        assert_eq!(
            item_key(&memory, &config, "direct").as_deref(),
            Some("k-direct")
        );
    }
}

#[test]
fn failed_relogin_keeps_the_previous_credential_and_config_resolvable() {
    let root = dir();
    let config = root.path().join("config.toml");
    run(
        &config,
        add("plan", "oauth:00000000000000000000000000000001"),
    )
    .unwrap();
    run(&config, add("other", "synthetic-other")).unwrap();
    let before = read(&config);
    let replacement = edit(
        "plan",
        Edit {
            provider: Some(ProviderKind::OpenAi),
            auth: Some(AuthKind::ChatGptSubscription),
            record: Some(record("oauth:00000000000000000000000000000002")),
            make_default: true,
            ..Edit::default()
        },
    );
    let failed = apply_with(
        &config,
        PathShown::Default,
        replacement.clone(),
        &NO_KEYCHAIN,
        WAIT,
        &|| Err(ChangeError::Interrupted),
    );
    assert!(failed.is_err());
    assert_eq!(read(&config), before);
    assert_eq!(
        key_of(&config, "plan").as_deref(),
        Some("oauth:00000000000000000000000000000001")
    );
    assert_eq!(key_of(&config, "other").as_deref(), Some("synthetic-other"));
    run(&config, replacement).unwrap();
    assert_eq!(
        key_of(&config, "plan").as_deref(),
        Some("oauth:00000000000000000000000000000002")
    );
    assert_eq!(key_of(&config, "other").as_deref(), Some("synthetic-other"));
    assert!(read(&config).contains("default = \"plan\""));
}

#[test]
fn relogin_ignores_unrelated_missing_records_but_keeps_available_ones() {
    let root = dir();
    let config = root.path().join("config.toml");
    for endpoint in ["plan", "missing", "other"] {
        run(&config, add(endpoint, "synthetic-old")).unwrap();
    }
    let id = store_id(&config).unwrap();
    let store = store_of(&config);
    let folder = store.folder();
    let locks = lock(
        &[(&folder, LockKind::Credentials)],
        PathShown::Default,
        WAIT,
    )
    .unwrap();
    store.remove(&locks, &id, &name("missing")).unwrap();
    drop(locks);
    run(
        &config,
        edit(
            "plan",
            Edit {
                record: Some(record("synthetic-new")),
                ..Edit::default()
            },
        ),
    )
    .unwrap();
    assert!(key_of(&config, "plan").as_deref() == Some("synthetic-new"));
    assert!(key_of(&config, "other").as_deref() == Some("synthetic-old"));
    assert_eq!(key_of(&config, "missing"), None);
    assert!(
        ConfigText::read(&config, PathShown::Default)
            .unwrap()
            .current()
            .unwrap()
            .endpoints
            .contains_key(&name("missing"))
    );
}

#[test]
fn relogin_names_an_unreadable_keyring_record_that_must_be_copied() {
    use crate::secrets::keyring::{BackendError, MemoryKeyring, MemoryMode};
    let root = dir();
    let config = root.path().join("config.toml");
    run(&config, add("plan", "synthetic-old")).unwrap();
    let memory = MemoryKeyring::new();
    let keys = memory.store(WAIT);
    let mut blocked = add("blocked", "synthetic-other");
    if let Change::Add { endpoint, .. } = &mut blocked {
        endpoint.store = StoreKind::Keyring;
    }
    apply(&config, PathShown::Default, blocked, &keys, WAIT).unwrap();
    let before = read(&config);
    memory.set_mode(MemoryMode::Failing(BackendError::NoStorageAccess));
    let keys = memory.store(WAIT);
    let error = apply(
        &config,
        PathShown::Default,
        edit(
            "plan",
            Edit {
                record: Some(record("synthetic-new")),
                ..Edit::default()
            },
        ),
        &keys,
        WAIT,
    )
    .unwrap_err();
    let text = error.to_string();
    assert!(
        text.contains("blocked") && text.contains("unlock"),
        "{text}"
    );
    assert!(!text.contains("own copy"), "{text}");
    assert_eq!(read(&config), before);
    assert!(key_of(&config, "plan").as_deref() == Some("synthetic-old"));
}

#[cfg(unix)]
#[test]
fn failed_source_file_removal_after_a_move_is_returned_as_a_left_note() {
    use crate::secrets::keyring::MemoryKeyring;
    let root = dir();
    let config = root.path().join("config.toml");
    run(&config, add("plan", "synthetic-old")).unwrap();
    let credentials = store_of(&config).path().to_owned();
    let backup = root.path().join("saved-credentials.json");
    let before = std::fs::read(&credentials).unwrap();
    let memory = MemoryKeyring::new();
    let keys = memory.store(WAIT);
    let applied = apply_with(
        &config,
        PathShown::Default,
        edit(
            "plan",
            Edit {
                store: Some(StoreKind::Keyring),
                ..Edit::default()
            },
        ),
        &keys,
        WAIT,
        &|| {
            std::fs::rename(&credentials, &backup).unwrap();
            std::os::unix::fs::symlink(&backup, &credentials).unwrap();
            Ok(())
        },
    )
    .unwrap();
    assert_eq!(applied.left.len(), 1);
    assert_eq!(applied.left[0].name, name("plan"));
    assert!(matches!(applied.left[0].error, StoreError::Linked(_)));
    assert!(std::fs::read(&credentials).unwrap() == before);
    let parsed = ConfigText::read(&config, PathShown::Default)
        .unwrap()
        .current()
        .unwrap();
    assert_eq!(
        parsed.endpoints[&name("plan")].store(),
        Some(StoreKind::Keyring)
    );
    let stored = crate::secrets::keyring::KeyringAccess::read(
        &keys,
        &parsed.store_id.unwrap(),
        &name("plan"),
    )
    .unwrap()
    .unwrap();
    let StoredRecord::Key { key, .. } = stored.record;
    assert!(key.expose() == "synthetic-old");
}
