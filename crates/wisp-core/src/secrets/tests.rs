use std::sync::{Arc, Barrier};

use super::*;
use crate::endpoints::{
    Availability, EndpointBook, EndpointError, FileError, Problem, UrlSource, missing_record,
};
use crate::provider::credential::Source;
use crate::test_guard::absent;
use crate::test_guard::dir;
use crate::test_guard::name;

const ID: &str = "5f0c2a9e41d7b388";
const OTHER_ID: &str = "0123456789abcdef";

fn id(text: &str) -> StoreId {
    StoreId::parse(text).unwrap()
}

fn stored(key: &str, url: Option<&str>, owner: Option<&Path>) -> Stored {
    Stored {
        record: StoredRecord::Key {
            key: Secret::new(key),
            url: url.map(Secret::new),
        },
        owner: owner.map(Path::to_owned),
    }
}

fn store_in(folder: &Path) -> FileStore {
    FileStore::beside(&folder.join("config.toml"), PathShown::Default)
}

fn write_credentials(folder: &Path, text: &str) -> FileStore {
    std::fs::write(folder.join(CREDENTIALS), text).unwrap();
    store_in(folder)
}

fn locked(folder: &Path) -> Locks {
    lock(
        &[(folder, LockKind::Credentials)],
        PathShown::Default,
        LOCK_WAIT,
    )
    .unwrap()
}

#[test]
fn a_store_id_is_sixteen_lowercase_hex_characters_and_never_printed() {
    assert!(StoreId::parse(ID).is_some());
    for bad in [
        "",
        "5f0c2a9e41d7b38",
        "5f0c2a9e41d7b3888",
        "5F0C2A9E41D7B388",
        "5f0c2a9e41d7b38g",
    ] {
        assert!(StoreId::parse(bad).is_none(), "{bad}");
    }
    let canary = id("cafe0000cafe0001");
    assert_eq!(format!("{canary:?}"), "StoreId(<hidden>)");
    assert_eq!(format!("{canary:#?}"), "StoreId(<hidden>)");
    let a = StoreId::new_random();
    let b = StoreId::new_random();
    assert_ne!(a, b);
    assert!(StoreId::parse(a.as_str()).is_some());
}

#[test]
fn a_hand_made_file_is_read_grouped_by_store() {
    let folder = dir();
    let store = write_credentials(
        folder.path(),
        &format!(
            r#"{{
  "version": 1,
  "stores": {{
    "{ID}": {{
      "manifest": {{
        "key": "CANARYKEY01",
        "owner": "/home/you/.config/wisp/config.toml",
        "url": "https://gw.example.com/CANARYPATH"
      }},
      "direct": {{ "key": "CANARYKEY02" }}
    }},
    "{OTHER_ID}": {{ "work": {{ "key": "CANARYKEY03" }} }}
  }}
}}
"#
        ),
    );
    let every = store.read_every().unwrap();
    assert_eq!(every.len(), 2);
    let ours = store.read_all(&id(ID)).unwrap();
    assert_eq!(
        ours[&name("manifest")],
        stored(
            "CANARYKEY01",
            Some("https://gw.example.com/CANARYPATH"),
            Some(Path::new("/home/you/.config/wisp/config.toml"))
        )
    );
    assert_eq!(ours[&name("direct")], stored("CANARYKEY02", None, None));
    assert!(store.read_all(&id("ffffffffffffffff")).unwrap().is_empty());
    let shown = format!("{every:?} {every:#?}");
    for canary in ["CANARYKEY", "CANARYPATH", "/home/you", ID, OTHER_ID] {
        assert!(!shown.contains(canary), "{canary} in {shown}");
    }
}

#[test]
fn a_missing_file_holds_no_records() {
    let folder = dir();
    let store = store_in(folder.path());
    assert!(!store.exists());
    assert!(store.read_every().unwrap().is_empty());
}

#[test]
fn every_file_error_names_its_line_and_no_value() {
    let record = |body: &str| {
        format!(
            "{{\n  \"version\": 1,\n  \"stores\": {{\n    \"{ID}\": {{\n      \"m\": {body}\n    }}\n  }}\n}}\n"
        )
    };
    let cases = [
        ("[\"CANARY01\"]".to_owned(), 1, RecordProblem::NotAnObject),
        (
            "{\"version\": 1, \"stores\": {}, \"CANARY02\": 1}".to_owned(),
            1,
            RecordProblem::UnknownTopField,
        ),
        ("{\"stores\": {}}".to_owned(), 1, RecordProblem::NoVersion),
        (
            "{\n\"version\": \"CANARY03\",\n\"stores\": {}}".to_owned(),
            2,
            RecordProblem::BadVersion,
        ),
        (
            "{\"version\": 0, \"stores\": {}}".to_owned(),
            1,
            RecordProblem::BadVersion,
        ),
        ("{\"version\": 1}".to_owned(), 1, RecordProblem::NoStores),
        (
            "{\"version\": 1, \"stores\": [\"CANARY04\"]}".to_owned(),
            1,
            RecordProblem::StoresNotObject,
        ),
        (
            "{\n  \"version\": 1,\n  \"stores\": {\n    \"CANARY05notanid\": {}\n  }\n}".to_owned(),
            4,
            RecordProblem::BadStoreId,
        ),
        (
            format!("{{\"version\": 1, \"stores\": {{\"{ID}\": \"CANARY06\"}}}}"),
            1,
            RecordProblem::StoreNotObject,
        ),
        (
            format!(
                "{{\n\"version\": 1,\n\"stores\": {{\"{ID}\": {{\n\"CANARY07\": {{\"key\": \"k\"}}}}}}}}"
            ),
            4,
            RecordProblem::BadName,
        ),
        (record("\"CANARY08\""), 5, RecordProblem::RecordNotObject),
        (
            record("{ \"key\": \"k\", \"CANARY09\": \"x\" }"),
            5,
            RecordProblem::UnknownRecordField,
        ),
        (
            record("{ \"owner\": \"/CANARY10\" }"),
            5,
            RecordProblem::NoKey,
        ),
        (
            record("{ \"key\": 12345678901 }"),
            5,
            RecordProblem::KeyNotString,
        ),
        (
            record("{ \"key\": [\"CANARY11\"] }"),
            5,
            RecordProblem::KeyNotString,
        ),
        (
            record("{ \"key\": { \"x\": \"CANARY12\" } }"),
            5,
            RecordProblem::KeyNotString,
        ),
        (record("{ \"key\": true }"), 5, RecordProblem::KeyNotString),
        (record("{ \"key\": null }"), 5, RecordProblem::KeyNotString),
        (
            record("{ \"key\": \"k\", \"owner\": 13131313 }"),
            5,
            RecordProblem::OwnerNotString,
        ),
        (
            record("{ \"key\": \"k\", \"url\": 1.5 }"),
            5,
            RecordProblem::UrlNotString,
        ),
        (
            record("{ \"key\": \"CANARY14\", \"key\": \"CANARY15\" }"),
            5,
            RecordProblem::Repeated,
        ),
    ];
    for (text, line, problem) in cases {
        let folder = dir();
        let err = write_credentials(folder.path(), &text)
            .read_every()
            .unwrap_err();
        assert_eq!(err, StoreError::Record { line, problem }, "{text}");
        let shown = format!("{err} {err:?}");
        assert_eq!(
            err.to_string(),
            format!("credentials.json line {line}: {problem}")
        );
        for canary in ["CANARY", "12345678901", "13131313"] {
            assert!(!shown.contains(canary), "{canary} in {shown}");
        }
    }
}

#[test]
fn each_record_problem_has_its_exact_words() {
    for (problem, words) in [
        (RecordProblem::NoKey, "a record needs a key"),
        (RecordProblem::KeyNotString, "key must be a string"),
        (RecordProblem::OwnerNotString, "owner must be a string"),
        (RecordProblem::UrlNotString, "url must be a string"),
    ] {
        assert_eq!(
            StoreError::Record { line: 5, problem }.to_string(),
            format!("credentials.json line 5: {words}")
        );
    }
    assert_eq!(
        StoreError::Record {
            line: 3,
            problem: RecordProblem::BadStoreId
        }
        .to_string(),
        "credentials.json line 3: a store ID must be 16 lowercase hex characters"
    );
    assert_eq!(
        StoreError::Record {
            line: 4,
            problem: RecordProblem::BadName
        }
        .to_string(),
        format!(
            "credentials.json line 4: {}",
            wisp_protocol::account_name_error()
        )
    );
}

#[test]
fn a_syntax_error_keeps_only_its_line() {
    let folder = dir();
    let text = format!(
        "{{\n  \"version\": 1,\n  \"stores\": {{\n    \"{ID}\": {{ \"m\": {{ \"key\": \"CANARY99 }} }}\n"
    );
    let err = write_credentials(folder.path(), &text)
        .read_every()
        .unwrap_err();
    assert!(matches!(err, StoreError::Syntax { .. }), "{err:?}");
    let shown = format!("{err} {err:?}");
    assert!(!shown.contains("CANARY"), "{shown}");
    let broken = write_credentials(folder.path(), "{\n\"version\": 1,\n CANARY98\n}")
        .read_every()
        .unwrap_err();
    assert_eq!(broken, StoreError::Syntax { line: 3 });
    assert_eq!(
        broken.to_string(),
        "credentials.json line 3: not valid JSON"
    );
    let empty = write_credentials(folder.path(), "")
        .read_every()
        .unwrap_err();
    assert_eq!(empty.to_string(), "credentials.json line 1: not valid JSON");
}

#[test]
fn a_newer_file_is_refused_whole() {
    let folder = dir();
    let store = write_credentials(
        folder.path(),
        r#"{"version": 2, "stores": {}, "future": "CANARY"}"#,
    );
    let err = store.read_every().unwrap_err();
    assert_eq!(err.to_string(), "credentials.json is from a newer Wisp");
    let before = std::fs::read(store.path()).unwrap();
    let locks = locked(folder.path());
    let refused = store
        .put(&locks, &id(ID), &name("m"), stored("k", None, None))
        .unwrap_err();
    assert_eq!(refused, StoreError::Refused);
    assert_eq!(std::fs::read(store.path()).unwrap(), before);
}

#[cfg(unix)]
#[test]
fn writes_are_atomic_owner_only_and_sorted() {
    use std::os::unix::fs::PermissionsExt;
    let root = dir();
    let folder = root.path().join("wisp");
    let store = store_in(&folder);
    let locks = locked(&folder);
    let mode = |p: &Path| std::fs::metadata(p).unwrap().permissions().mode() & 0o777;
    assert_eq!(mode(&folder), 0o700);
    store
        .put(&locks, &id(ID), &name("zeta"), stored("k1", None, None))
        .unwrap();
    std::fs::write(folder.join(".wisp-tmp-leftover"), "junk").unwrap();
    assert_eq!(store.read_all(&id(ID)).unwrap().len(), 1);
    store
        .put(
            &locks,
            &id(ID),
            &name("alpha"),
            stored(
                "k2",
                Some("https://gw.example.com/p"),
                Some(Path::new("/c/config.toml")),
            ),
        )
        .unwrap();
    assert!(!folder.join(".wisp-tmp-leftover").exists());
    assert_eq!(mode(store.path()), 0o600);
    let text = std::fs::read_to_string(store.path()).unwrap();
    assert_eq!(
        text,
        format!(
            "{{\n  \"stores\": {{\n    \"{ID}\": {{\n      \"alpha\": {{\n        \"key\": \"k2\",\n        \"owner\": \"/c/config.toml\",\n        \"url\": \"https://gw.example.com/p\"\n      }},\n      \"zeta\": {{\n        \"key\": \"k1\"\n      }}\n    }}\n  }},\n  \"version\": 1\n}}\n"
        )
    );
    std::fs::set_permissions(store.path(), std::fs::Permissions::from_mode(0o644)).unwrap();
    store.remove(&locks, &id(ID), &name("zeta")).unwrap();
    assert_eq!(mode(store.path()), 0o600);
    store.remove(&locks, &id(ID), &name("alpha")).unwrap();
    assert!(store.read_every().unwrap().is_empty());
    let leftovers: Vec<_> = std::fs::read_dir(&folder)
        .unwrap()
        .flatten()
        .filter(|e| e.file_name().to_string_lossy().starts_with(".wisp-tmp-"))
        .collect();
    assert!(leftovers.is_empty());
}

#[cfg(unix)]
#[test]
fn replace_atomic_renames_after_the_check_and_keeps_other_temps() {
    use std::os::unix::fs::{MetadataExt, PermissionsExt};
    let folder = dir();
    let target = folder.path().join("record.json");
    let other = folder.path().join(".wisp-tmp-other");
    std::fs::write(&target, "old").unwrap();
    std::fs::write(&other, "in flight").unwrap();
    let names = || {
        let mut names: Vec<String> = std::fs::read_dir(folder.path())
            .unwrap()
            .map(|e| e.unwrap().file_name().into_string().unwrap())
            .collect();
        names.sort();
        names
    };
    let refused = replace_atomic(&target, b"new", 0o600, || {
        let temps: Vec<String> = names()
            .into_iter()
            .filter(|n| n.starts_with(TEMP_PREFIX) && *n != ".wisp-tmp-other")
            .collect();
        assert_eq!(temps.len(), 1, "{temps:?}");
        assert_eq!(
            std::fs::read(folder.path().join(&temps[0])).unwrap(),
            b"new"
        );
        assert_eq!(std::fs::read(&target).unwrap(), b"old");
        Err(io::Error::other("refused before the rename"))
    })
    .unwrap_err();
    assert_eq!(refused.to_string(), "refused before the rename");
    assert_eq!(std::fs::read(&target).unwrap(), b"old");
    assert_eq!(names(), [".wisp-tmp-other", "record.json"]);
    let before = std::fs::metadata(&target).unwrap().ino();
    replace_atomic(&target, b"new", 0o600, || Ok(())).unwrap();
    let after = std::fs::metadata(&target).unwrap();
    assert_ne!(after.ino(), before, "replaced by a rename");
    assert_eq!(after.permissions().mode() & 0o777, 0o600);
    assert_eq!(std::fs::read(&target).unwrap(), b"new");
    assert_eq!(names(), [".wisp-tmp-other", "record.json"]);
}

#[test]
fn a_corrupt_file_is_never_overwritten() {
    let folder = dir();
    let text = "{\"version\": 1, \"stores\": CANARY}";
    let store = write_credentials(folder.path(), text);
    let locks = locked(folder.path());
    assert_eq!(
        store
            .put(&locks, &id(ID), &name("m"), stored("k", None, None))
            .unwrap_err()
            .to_string(),
        "fix or move aside credentials.json first; nothing was changed"
    );
    assert_eq!(
        store.remove(&locks, &id(ID), &name("m")).unwrap_err(),
        StoreError::Refused
    );
    assert_eq!(std::fs::read_to_string(store.path()).unwrap(), text);
}

#[test]
fn a_write_needs_the_lock_of_its_folder() {
    let a = dir();
    let b = dir();
    let locks = locked(a.path());
    let store = store_in(b.path());
    assert_eq!(
        store
            .put(&locks, &id(ID), &name("m"), stored("k", None, None))
            .unwrap_err(),
        StoreError::NotLocked
    );
    assert!(!store.exists());
}

#[test]
fn two_configs_in_one_folder_keep_separate_records() {
    let folder = dir();
    let locks = locked(folder.path());
    let first = FileStore::beside(&folder.path().join("config.toml"), PathShown::Default);
    let second = FileStore::beside(&folder.path().join("other.toml"), PathShown::Given);
    assert_eq!(first.path(), second.path());
    first
        .put(&locks, &id(ID), &name("m"), stored("first", None, None))
        .unwrap();
    second
        .put(
            &locks,
            &id(OTHER_ID),
            &name("m"),
            stored("second", None, None),
        )
        .unwrap();
    assert_eq!(
        first.read_all(&id(ID)).unwrap()[&name("m")],
        stored("first", None, None)
    );
    assert_eq!(
        second.read_all(&id(OTHER_ID)).unwrap()[&name("m")],
        stored("second", None, None)
    );
}

#[test]
fn two_harness_writers_serialise_and_both_land() {
    let folder = dir();
    let path = folder.path().to_owned();
    let held = locked(&path);
    assert_eq!(
        lock(
            &[(path.as_path(), LockKind::Credentials)],
            PathShown::Default,
            Duration::from_millis(100)
        )
        .err(),
        Some(StoreError::Busy)
    );
    assert_eq!(
        StoreError::Busy.to_string(),
        "another wisp is changing accounts; try again"
    );
    drop(held);
    let start = Arc::new(Barrier::new(2));
    let writers: Vec<_> = ["one", "two"]
        .into_iter()
        .map(|endpoint| {
            let path = path.clone();
            let start = Arc::clone(&start);
            std::thread::spawn(move || {
                start.wait();
                let locks = locked(&path);
                let store = store_in(&path);
                std::thread::sleep(Duration::from_millis(50));
                store
                    .put(
                        &locks,
                        &id(ID),
                        &name(endpoint),
                        stored(endpoint, None, None),
                    )
                    .unwrap();
            })
        })
        .collect();
    for writer in writers {
        writer.join().unwrap();
    }
    let names: Vec<String> = store_in(&path)
        .read_all(&id(ID))
        .unwrap()
        .keys()
        .map(|n| n.as_str().to_owned())
        .collect();
    assert_eq!(names, ["one", "two"]);
}

#[cfg(unix)]
#[test]
fn a_folder_reached_two_ways_is_one_lock_and_crossed_links_never_deadlock() {
    let root = dir();
    let a = root.path().join("a");
    let b = root.path().join("b");
    std::fs::create_dir_all(&a).unwrap();
    std::fs::create_dir_all(&b).unwrap();
    let a_link = root.path().join("a-link");
    let b_link = root.path().join("b-link");
    std::os::unix::fs::symlink(&a, &a_link).unwrap();
    std::os::unix::fs::symlink(&b, &b_link).unwrap();
    let both = lock(
        &[
            (a.as_path(), LockKind::Config),
            (a_link.as_path(), LockKind::Credentials),
        ],
        PathShown::Default,
        Duration::from_millis(200),
    )
    .unwrap();
    assert!(both.covers(&a) && both.covers(&a_link));
    assert_eq!(
        lock(
            &[(a_link.as_path(), LockKind::Credentials)],
            PathShown::Default,
            Duration::from_millis(100)
        )
        .err(),
        Some(StoreError::Busy)
    );
    drop(both);
    let pairs = [(a.clone(), b_link.clone()), (b.clone(), a_link.clone())];
    let start = Arc::new(Barrier::new(2));
    let workers: Vec<_> = pairs
        .into_iter()
        .map(|(first, second)| {
            let start = Arc::clone(&start);
            std::thread::spawn(move || {
                start.wait();
                for _ in 0..20 {
                    let locks = lock(
                        &[
                            (first.as_path(), LockKind::Config),
                            (second.as_path(), LockKind::Credentials),
                        ],
                        PathShown::Default,
                        Duration::from_secs(5),
                    )
                    .unwrap();
                    std::thread::sleep(Duration::from_millis(1));
                    drop(locks);
                }
            })
        })
        .collect();
    for worker in workers {
        worker.join().unwrap();
    }
}

#[cfg(unix)]
#[test]
fn a_linked_credentials_file_is_unavailable_and_never_written() {
    let root = dir();
    let target = root.path().join("elsewhere.json");
    std::fs::write(
        &target,
        format!("{{\"version\": 1, \"stores\": {{\"{ID}\": {{\"m\": {{\"key\": \"CANARY\"}}}}}}}}"),
    )
    .unwrap();
    let folder = root.path().join("wisp");
    std::fs::create_dir_all(&folder).unwrap();
    std::os::unix::fs::symlink(&target, folder.join(CREDENTIALS)).unwrap();
    let store = store_in(&folder);
    let err = store.read_every().unwrap_err();
    assert_eq!(
        err.to_string(),
        "credentials.json is a symbolic link; Wisp keeps keys only in a regular file"
    );
    let StoreError::Linked(note) = &err else {
        panic!("{err:?}");
    };
    assert_eq!(note.kind, PathNoteKind::CredentialsLinked);
    let before = std::fs::read(&target).unwrap();
    let locks = locked(&folder);
    assert!(matches!(
        store.put(&locks, &id(ID), &name("m"), stored("k", None, None)),
        Err(StoreError::Linked(_))
    ));
    assert_eq!(std::fs::read(&target).unwrap(), before);
    assert!(
        std::fs::symlink_metadata(folder.join(CREDENTIALS))
            .unwrap()
            .file_type()
            .is_symlink()
    );
    let notes = path_notes(&folder.join("config.toml"), PathShown::Default);
    assert!(
        notes
            .iter()
            .any(|n| n.kind == PathNoteKind::CredentialsLinked)
    );
}

#[test]
fn a_work_tree_needs_a_real_git_directory_or_a_gitdir_file() {
    let root = dir();
    let real_root = std::fs::canonicalize(root.path()).unwrap();
    let folder = root.path().join("repo/wisp");
    std::fs::create_dir_all(&folder).unwrap();
    std::fs::create_dir_all(root.path().join(".git")).unwrap();
    std::fs::write(root.path().join("repo/.git"), "not a gitfile\n").unwrap();
    assert_eq!(work_tree(&folder), None);

    std::fs::write(root.path().join("repo/.git"), "gitdir: elsewhere\n").unwrap();
    let real = real_root.join("repo/wisp");
    assert_eq!(work_tree(&folder), Some((real, real_root.join("repo"))));
}

#[test]
fn writes_inside_a_git_work_tree_return_their_notes() {
    let repo = dir();
    crate::instructions::make_repository(repo.path()).unwrap();
    let folder = repo.path().join("wisp");
    let real_repo = std::fs::canonicalize(repo.path()).unwrap();
    let locks = lock(
        &[(folder.as_path(), LockKind::Credentials)],
        PathShown::Default,
        LOCK_WAIT,
    )
    .unwrap();
    let real = std::fs::canonicalize(&folder).unwrap();
    assert_eq!(
        locks.notes(),
        [PathNote {
            kind: PathNoteKind::LockInRepo(LockKind::Credentials),
            path: real.clone(),
            repo: Some(real_repo.clone()),
            shown: PathShown::Default,
        }]
    );
    let written = store_in(&folder)
        .put(&locks, &id(ID), &name("m"), stored("k", None, None))
        .unwrap();
    assert_eq!(
        written.notes,
        [PathNote {
            kind: PathNoteKind::CredentialsInRepo {
                through_link: false
            },
            path: real.clone(),
            repo: Some(real_repo.clone()),
            shown: PathShown::Default,
        }]
    );
    drop(locks);
    let again = lock(
        &[
            (folder.as_path(), LockKind::Credentials),
            (folder.as_path(), LockKind::Config),
        ],
        PathShown::Given,
        LOCK_WAIT,
    )
    .unwrap();
    assert!(again.notes().is_empty(), "the lock file already existed");
    drop(again);
    std::fs::remove_file(folder.join(LOCK_FILE)).unwrap();
    let one = lock(
        &[
            (folder.as_path(), LockKind::Credentials),
            (folder.as_path(), LockKind::Config),
        ],
        PathShown::Given,
        LOCK_WAIT,
    )
    .unwrap();
    assert_eq!(one.notes().len(), 1);
    assert_eq!(
        one.notes()[0].kind,
        PathNoteKind::LockInRepo(LockKind::Config)
    );
    assert_eq!(one.notes()[0].shown, PathShown::Given);
    drop(one);
    let notes = path_notes(&folder.join("config.toml"), PathShown::Default);
    assert!(notes.contains(&PathNote {
        kind: PathNoteKind::CredentialsInRepo {
            through_link: false
        },
        path: real.clone(),
        repo: Some(real_repo.clone()),
        shown: PathShown::Default,
    }));
    assert!(
        notes
            .iter()
            .any(|n| n.kind == PathNoteKind::LockInRepo(LockKind::Credentials)),
        "{notes:?}"
    );
}

#[cfg(unix)]
#[test]
fn a_readable_credentials_file_gets_a_note() {
    use std::os::unix::fs::PermissionsExt;
    let folder = dir();
    let store = write_credentials(folder.path(), "{\"version\": 1, \"stores\": {}}");
    let config = folder.path().join("config.toml");
    let readable = |shown| -> Vec<PathNote> {
        path_notes(&config, shown)
            .into_iter()
            .filter(|n| n.kind == PathNoteKind::CredentialsReadable)
            .collect()
    };
    std::fs::set_permissions(store.path(), std::fs::Permissions::from_mode(0o600)).unwrap();
    assert!(readable(PathShown::Default).is_empty());
    std::fs::set_permissions(store.path(), std::fs::Permissions::from_mode(0o640)).unwrap();
    assert_eq!(
        readable(PathShown::Given),
        [PathNote {
            kind: PathNoteKind::CredentialsReadable,
            path: store.path().to_owned(),
            repo: None,
            shown: PathShown::Given,
        }]
    );
}

#[test]
fn path_notes_print_a_given_path_nowhere() {
    let canary = PathBuf::from("/tmp/CANARYFOLDER/wisp");
    let repo = PathBuf::from("/tmp/CANARYFOLDER");
    let given = PathNote {
        kind: PathNoteKind::CredentialsInRepo { through_link: true },
        path: canary.clone(),
        repo: Some(repo.clone()),
        shown: PathShown::Given,
    };
    for shown in [format!("{given:?}"), format!("{given:#?}")] {
        assert!(!shown.contains("CANARY"), "{shown}");
        assert!(shown.contains("<given>"), "{shown}");
    }
    let default = PathNote {
        shown: PathShown::Default,
        ..given
    };
    let shown = format!("{default:?}");
    assert!(shown.contains("/tmp/CANARYFOLDER/wisp"), "{shown}");
    assert!(
        shown.contains("repo: Some(\"/tmp/CANARYFOLDER\")"),
        "{shown}"
    );
}

#[test]
fn no_debug_output_shows_a_given_path_an_owner_or_a_store_id() {
    let canary = Path::new("/tmp/CANARYDIR/config.toml");
    let check = |what: &str, shown: String, expect_path: bool| {
        assert_eq!(shown.contains("CANARYDIR"), expect_path, "{what}: {shown}");
        assert!(!shown.contains("cafe0000cafe0001"), "{what}: {shown}");
        assert!(!shown.contains("CANARYKEY"), "{what}: {shown}");
    };
    let record = stored("CANARYKEY", Some("https://CANARYKEY.example"), Some(canary));
    check("stored", format!("{record:?} {record:#?}"), false);
    assert!(format!("{record:?}").contains("Some(<path>)"));
    for (shown, expect) in [(PathShown::Given, false), (PathShown::Default, true)] {
        let source = Source::CredentialFile {
            path: canary.to_owned(),
            shown,
        };
        check("source", format!("{source:?} {source:#?}"), expect);
        let store = FileStore::beside(canary, shown);
        check("file store", format!("{store:?} {store:#?}"), expect);
        let url = UrlSource::Config {
            path: canary.to_owned(),
            shown,
        };
        check("url source", format!("{url:?} {url:#?}"), expect);
        let text = "store_id = \"cafe0000cafe0001\"\n[endpoints.m]\nauth = \"api_key\"\nkey_store = \"file\"\n";
        let config = ConfigFile::parse(canary, shown, text).unwrap();
        check("config", format!("{config:?} {config:#?}"), expect);
        let file_error = FileError {
            path: canary.to_owned(),
            shown,
            line: Some(1),
            problem: Problem::Syntax,
        };
        check(
            "file error",
            format!("{file_error:?} {file_error:#?}"),
            expect,
        );
        let endpoint_error = EndpointError::File(file_error);
        check(
            "endpoint error",
            format!("{endpoint_error:?} {endpoint_error:#?}"),
            expect,
        );
        let secrets = Prefetched::from_records(BTreeMap::from([(
            name("m"),
            stored("sk-ant-CANARYKEY", None, Some(canary)),
        )]));
        let entries =
            EndpointBook::entries(Some(&config), &secrets, &absent(&|_| None, None, false));
        let Some(Availability::Ready(resolved)) = entries.get(&name("m")) else {
            panic!("{entries:?}");
        };
        check(
            "resolved",
            format!("{resolved:?} {resolved:#?} {entries:?}"),
            expect,
        );
        let mut with_url = resolved.clone();
        with_url.url_source = UrlSource::Config {
            path: canary.to_owned(),
            shown,
        };
        check("resolved url", format!("{with_url:?}"), expect);
    }
}

#[test]
fn the_prefetched_map_hands_out_records_and_errors() {
    let secrets = Prefetched::from_records(BTreeMap::from([(
        name("m"),
        stored("k", None, Some(Path::new("/o"))),
    )]));
    assert_eq!(
        secrets.get(StoreKind::File, &name("m")).unwrap(),
        Some(stored("k", None, None).record)
    );
    assert_eq!(secrets.get(StoreKind::File, &name("x")).unwrap(), None);
    assert_eq!(secrets.get(StoreKind::Keyring, &name("m")).unwrap(), None);
    let failing = Prefetched::failing(StoreError::Newer);
    assert_eq!(
        failing.get(StoreKind::File, &name("m")),
        Err(StoreError::Newer)
    );
    assert_eq!(
        format!("{secrets:?}"),
        "Prefetched(1 records, 0 keychain lookups)"
    );
}

fn config_text(store_id: &str, names: &[&str]) -> String {
    let mut text = format!("store_id = \"{store_id}\"\n");
    for name in names {
        text.push_str(&format!(
            "[endpoints.{name}]\nauth = \"api_key\"\nkey_store = \"file\"\n"
        ));
    }
    text
}

fn write_config(folder: &Path, file: &str, text: &str) -> PathBuf {
    std::fs::create_dir_all(folder).unwrap();
    let path = folder.join(file);
    std::fs::write(&path, text).unwrap();
    path
}

fn read_config(path: &Path, shown: PathShown) -> ConfigFile {
    ConfigFile::parse(path, shown, &std::fs::read_to_string(path).unwrap()).unwrap()
}

#[test]
fn a_copy_in_the_same_folder_reads_its_originals_records() {
    let root = dir();
    let folder = root.path().join("wisp");
    let original = write_config(&folder, "config.toml", &config_text(ID, &["m"]));
    let copy = write_config(&folder, "copy.toml", &config_text(ID, &["m"]));
    let locks = locked(&folder);
    store_in(&folder)
        .put(
            &locks,
            &id(ID),
            &name("m"),
            stored(
                "sk-ant-copied",
                None,
                Some(&std::fs::canonicalize(&original).unwrap()),
            ),
        )
        .unwrap();
    let config = read_config(&copy, PathShown::Given);
    let secrets = Prefetched::load(Some(&config));
    let book = EndpointBook::resolve(
        Some(&config),
        &secrets,
        &absent(&|_| None, None, false),
        None,
    )
    .unwrap();
    assert_eq!(book.start.as_str(), "m");
    assert_eq!(
        book.active.source,
        Source::CredentialFile {
            path: folder.join(CREDENTIALS),
            shown: PathShown::Given
        }
    );
}

#[test]
fn a_copy_in_another_folder_says_its_keys_stayed_behind() {
    let root = dir();
    let first = root.path().join("first");
    let second = root.path().join("second");
    write_config(&first, "config.toml", &config_text(ID, &["manifest"]));
    let copy = write_config(&second, "config.toml", &config_text(ID, &["manifest"]));
    let locks = locked(&first);
    store_in(&first)
        .put(
            &locks,
            &id(ID),
            &name("manifest"),
            stored("sk-ant-k", None, None),
        )
        .unwrap();
    for shown in [PathShown::Given, PathShown::Default] {
        let config = read_config(&copy, shown);
        let secrets = Prefetched::load(Some(&config));
        let entries =
            EndpointBook::entries(Some(&config), &secrets, &absent(&|_| None, None, false));
        let Some(Availability::Unavailable(why)) = entries.get(&name("manifest")) else {
            panic!("{entries:?}");
        };
        let file = match shown {
            PathShown::Given => "credentials.json beside the file given with --config".to_owned(),
            PathShown::Default => crate::workspace::home_shown(&second.join(CREDENTIALS)),
        };
        assert_eq!(
            why.reason,
            format!(
                "no key for manifest in {file}. If this config.toml was copied from another folder, its keys stayed in that folder's credentials.json: copy that file here too, or run wisp account edit manifest --replace-key"
            )
        );
        assert_eq!(
            why.reason,
            missing_record(&name("manifest"), &second.join(CREDENTIALS), shown)
        );
        if shown == PathShown::Given {
            assert!(!why.reason.contains(&root.path().display().to_string()));
        }
    }
}

#[test]
fn the_owner_of_a_record_is_read_by_its_state() {
    let root = dir();
    let folder = root.path();
    let uses = write_config(folder, "uses.toml", &config_text(ID, &["m"]));
    let other = write_config(folder, "other.toml", &config_text(OTHER_ID, &["m"]));
    let none = write_config(folder, "none.toml", "default = \"env\"\n");
    let broken = write_config(folder, "broken.toml", "store_id = CANARY\n");
    let sub = folder.join("a-folder");
    std::fs::create_dir_all(&sub).unwrap();
    let large = write_config(
        folder,
        "large.toml",
        &format!("store_id = \"{ID}\"\n{}", "#\n".repeat(600_000)),
    );
    assert_eq!(owner_state(&uses, &id(ID)), OwnerState::Uses);
    assert_eq!(owner_state(&other, &id(ID)), OwnerState::NoLongerUses);
    assert_eq!(owner_state(&none, &id(ID)), OwnerState::NoLongerUses);
    assert_eq!(owner_state(&broken, &id(ID)), OwnerState::Unreadable);
    assert_eq!(owner_state(&sub, &id(ID)), OwnerState::Unreadable);
    assert_eq!(owner_state(&large, &id(ID)), OwnerState::Unreadable);
    assert_eq!(
        owner_state(&folder.join("gone.toml"), &id(ID)),
        OwnerState::Gone
    );
    let grows = write_config(folder, "grows.toml", &config_text(ID, &["m"]));
    let grow = || {
        let mut text = std::fs::read_to_string(&grows).unwrap();
        text.push_str(&"#\n".repeat(600_000));
        std::fs::write(&grows, text).unwrap();
    };
    assert_eq!(
        owner_state_with(&grows, &id(ID), &grow),
        OwnerState::Unreadable
    );
}

#[cfg(unix)]
#[test]
fn an_unreadable_owner_or_a_fifo_never_blocks_or_reads_as_gone() {
    use std::os::unix::fs::PermissionsExt;
    let root = dir();
    let fifo = root.path().join("fifo.toml");
    let status = std::process::Command::new("mkfifo")
        .arg(&fifo)
        .status()
        .unwrap();
    assert!(status.success());
    assert_eq!(owner_state(&fifo, &id(ID)), OwnerState::Unreadable);
    let locked_out = write_config(root.path(), "locked.toml", &config_text(ID, &["m"]));
    std::fs::set_permissions(&locked_out, std::fs::Permissions::from_mode(0o000)).unwrap();
    if std::fs::read(&locked_out).is_err() {
        assert_eq!(owner_state(&locked_out, &id(ID)), OwnerState::Unreadable);
    }
}

#[test]
fn ownership_finds_records_owned_elsewhere_and_orphans() {
    let root = dir();
    let folder = root.path();
    let this = write_config(folder, "config.toml", &config_text(ID, &["mine", "copied"]));
    let original = write_config(folder, "original.toml", &config_text(ID, &["copied"]));
    let moved_on = write_config(folder, "moved.toml", &config_text(OTHER_ID, &[]));
    let canonical = |p: &Path| std::fs::canonicalize(p).unwrap();
    let gone = folder.join("gone.toml");
    let third = "aaaaaaaaaaaaaaaa";
    let fourth = "bbbbbbbbbbbbbbbb";
    let every: Every = BTreeMap::from([
        (
            id(ID),
            BTreeMap::from([
                (name("mine"), stored("k", None, Some(&canonical(&this)))),
                (
                    name("copied"),
                    stored("k", None, Some(&canonical(&original))),
                ),
                (name("old"), stored("k", None, Some(&canonical(&this)))),
            ]),
        ),
        (
            id(OTHER_ID),
            BTreeMap::from([
                (name("a"), stored("k", None, Some(&canonical(&moved_on)))),
                (name("b"), stored("k", None, Some(&canonical(&moved_on)))),
            ]),
        ),
        (
            id(third),
            BTreeMap::from([
                (name("c"), stored("k", None, Some(&gone))),
                (name("d"), stored("k", None, None)),
                (name("e"), stored("k", None, Some(&canonical(&this)))),
            ]),
        ),
        (
            id(fourth),
            BTreeMap::from([(name("f"), stored("k", None, Some(&canonical(&original))))]),
        ),
    ]);
    let config = read_config(&this, PathShown::Default);
    let found = ownership(&config, &every);
    assert_eq!(
        found.not_owned,
        [NotOwned {
            name: name("copied"),
            owner: canonical(&original),
            state: OwnerState::Uses,
        }]
    );
    assert_eq!(found.unnamed, [name("old")]);
    let mut orphans: Vec<(Option<PathBuf>, Option<OwnerState>, usize)> = found
        .orphans
        .iter()
        .map(|o| (o.owner.clone(), o.state, o.count))
        .collect();
    orphans.sort();
    let mut want = vec![
        (None, None, 1),
        (Some(gone), Some(OwnerState::Gone), 1),
        (Some(canonical(&this)), Some(OwnerState::NoLongerUses), 1),
        (
            Some(canonical(&original)),
            Some(OwnerState::NoLongerUses),
            1,
        ),
    ];
    want.sort();
    assert_eq!(
        orphans, want,
        "moved.toml still uses its store, so a and b are not reported"
    );
    assert!(!format!("{found:?}").contains(&folder.display().to_string()));
}

#[cfg(unix)]
#[test]
fn a_repository_reached_through_a_link_is_told_apart_from_a_linked_home() {
    let root = dir();
    let real_home = root.path().join("real-home");
    crate::instructions::make_repository(&real_home).unwrap();
    std::fs::create_dir_all(real_home.join(".config/wisp")).unwrap();
    let home = root.path().join("home");
    std::os::unix::fs::symlink(&real_home, &home).unwrap();
    let folder = home.join(".config/wisp");
    let repo = std::fs::canonicalize(&real_home).unwrap();
    assert!(!through_link(&folder, &repo));
    let in_repo = |notes: Vec<PathNote>| -> Vec<PathNoteKind> {
        notes
            .into_iter()
            .map(|n| n.kind)
            .filter(|k| matches!(k, PathNoteKind::CredentialsInRepo { .. }))
            .collect()
    };
    let config = folder.join("config.toml");
    write_credentials(&folder, "{\"version\": 1, \"stores\": {}}");
    assert_eq!(
        in_repo(path_notes(&config, PathShown::Default)),
        [PathNoteKind::CredentialsInRepo {
            through_link: false
        }]
    );

    let dotfiles = root.path().join("dotfiles");
    crate::instructions::make_repository(&dotfiles).unwrap();
    std::fs::create_dir_all(dotfiles.join("wisp")).unwrap();
    let plain = root.path().join("plain");
    std::fs::create_dir_all(plain.join(".config")).unwrap();
    let linked = plain.join(".config/wisp");
    std::os::unix::fs::symlink(dotfiles.join("wisp"), &linked).unwrap();
    let repo = std::fs::canonicalize(&dotfiles).unwrap();
    assert!(through_link(&linked, &repo));
    write_credentials(&linked, "{\"version\": 1, \"stores\": {}}");
    assert_eq!(
        in_repo(path_notes(&linked.join("config.toml"), PathShown::Default)),
        [PathNoteKind::CredentialsInRepo { through_link: true }]
    );
    let locks = lock(
        &[(linked.as_path(), LockKind::Credentials)],
        PathShown::Default,
        LOCK_WAIT,
    )
    .unwrap();
    let written = store_in(&linked)
        .put(&locks, &id(ID), &name("m"), stored("k", None, None))
        .unwrap();
    assert_eq!(
        written.notes.iter().map(|n| n.kind).collect::<Vec<_>>(),
        [PathNoteKind::CredentialsInRepo { through_link: true }]
    );
}

#[test]
fn a_record_whose_endpoint_reads_a_variable_is_told_apart() {
    let root = dir();
    let folder = root.path();
    let text = format!(
        "{}[endpoints.work]\nauth = \"api_key\"\nkey_env = \"K\"\n",
        config_text(ID, &["m"])
    );
    let this = write_config(folder, "config.toml", &text);
    let owner = std::fs::canonicalize(&this).unwrap();
    let every: Every = BTreeMap::from([(
        id(ID),
        BTreeMap::from([
            (name("m"), stored("k", None, Some(&owner))),
            (name("work"), stored("k", None, Some(&owner))),
            (name("gone"), stored("k", None, Some(&owner))),
        ]),
    )]);
    let found = ownership(&read_config(&this, PathShown::Default), &every);
    assert_eq!(found.variable, [name("work")]);
    assert_eq!(found.unnamed, [name("gone")]);
    assert!(found.not_owned.is_empty());
}
