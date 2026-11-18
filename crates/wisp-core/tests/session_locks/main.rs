#![allow(clippy::unwrap_used, reason = "lock tests fail on any setup error")]

use std::fs;
use std::io;
use std::path::Path;

use wisp_core::session::{
    JsonlStore, Line, LogLock, ResumeLock, SessionLog, SessionStore, new_session_id, parse_jsonl,
    short_id,
};
use wisp_protocol::log::{LOG_SCHEMA_VERSION, LogEvent, LogRecord};

#[path = "../../../wisp-tui/tests/support/pick.rs"]
mod pick;
mod resume;
mod shared;

use pick::pick;
use shared::{record, temp_dir};

fn started() -> LogEvent {
    LogEvent::SessionEnded {
        turns: 7,
        interrupted: None,
    }
}

fn sample_records() -> Vec<LogRecord> {
    (0..4)
        .map(|seq| {
            record(
                seq,
                LogEvent::SessionEnded {
                    turns: seq,
                    interrupted: None,
                },
            )
        })
        .collect()
}

fn kept(lines: &[Line]) -> Vec<LogRecord> {
    pick!(lines, Line::Record(record) => (**record).clone())
}

fn written(dir: &Path, records: &[LogRecord]) -> String {
    let mut store = JsonlStore::open(dir).unwrap();
    let id = new_session_id();
    for r in records {
        store.append(&id, r).unwrap();
    }
    id
}

#[test]
fn a_session_open_in_one_store_is_refused_by_another() {
    let dir = temp_dir("lock-held");
    let mut first = JsonlStore::open(&dir).unwrap();
    let id = new_session_id();
    assert_eq!(first.open_session(&id).unwrap(), LogLock::Held);
    first.append(&id, &record(0, started())).unwrap();

    let mut second = JsonlStore::open(&dir).unwrap();
    let refused = second.open_session(&id).unwrap_err();
    assert_eq!(refused.kind(), io::ErrorKind::WouldBlock);
    assert_eq!(
        refused.to_string(),
        format!(
            "session {} is open in another Wisp",
            wisp_protocol::short_id(&id)
        )
    );
    assert!(second.append(&id, &record(1, started())).is_err());
    assert_eq!(second.read(&id).unwrap().len(), 1);

    drop(first);
    assert_eq!(second.open_session(&id).unwrap(), LogLock::Held);
    second.append(&id, &record(1, started())).unwrap();
    assert_eq!(second.read(&id).unwrap().len(), 2);
    fs::remove_dir_all(&dir).unwrap();
}

#[test]
fn a_store_writes_through_the_handle_it_locked() {
    let dir = temp_dir("lock-one-handle");
    let mut store = JsonlStore::open(&dir).unwrap();
    let id = new_session_id();
    assert_eq!(store.open_session(&id).unwrap(), LogLock::Held);
    assert_eq!(store.open_session(&id).unwrap(), LogLock::Held);
    for r in sample_records() {
        store.append(&id, &r).unwrap();
    }
    store.sync(&id).unwrap();
    assert_eq!(store.read(&id).unwrap(), sample_records());
    assert!(JsonlStore::open(&dir).unwrap().open_session(&id).is_err());
    fs::remove_dir_all(&dir).unwrap();
}

#[test]
fn the_first_append_locks_a_session_opened_lazily() {
    let dir = temp_dir("lock-lazy");
    let mut store = JsonlStore::open(&dir).unwrap();
    let id = new_session_id();
    store.append(&id, &record(0, started())).unwrap();
    let refused = JsonlStore::open(&dir)
        .unwrap()
        .open_session(&id)
        .unwrap_err();
    assert_eq!(refused.kind(), io::ErrorKind::WouldBlock);

    let other = new_session_id();
    store.append(&other, &record(0, started())).unwrap();
    assert_eq!(
        JsonlStore::open(&dir).unwrap().open_session(&id).unwrap(),
        LogLock::Held
    );
    assert!(
        JsonlStore::open(&dir)
            .unwrap()
            .open_session(&other)
            .is_err()
    );
    fs::remove_dir_all(&dir).unwrap();
}

#[test]
fn a_session_log_holds_its_lock_until_it_is_dropped() {
    let dir = temp_dir("lock-log");
    let mut store = JsonlStore::open(&dir).unwrap();
    let id = new_session_id();
    store.open_session(&id).unwrap();
    let mut log = SessionLog::new(Box::new(store), id.clone());
    assert_eq!(log.record(started()), None);
    assert!(JsonlStore::open(&dir).unwrap().open_session(&id).is_err());
    drop(log);
    assert_eq!(
        JsonlStore::open(&dir).unwrap().open_session(&id).unwrap(),
        LogLock::Held
    );
    fs::remove_dir_all(&dir).unwrap();
}

#[test]
fn appends_reuse_the_locked_handle_instead_of_reopening_the_path() {
    let dir = temp_dir("lock-reuse");
    let mut store = JsonlStore::open(&dir).unwrap();
    let id = new_session_id();
    assert_eq!(store.open_session(&id).unwrap(), LogLock::Held);
    fs::remove_file(store.path(&id)).unwrap();
    store.append(&id, &record(0, started())).unwrap();
    store.sync(&id).unwrap();
    assert!(!store.path(&id).exists());
    fs::remove_dir_all(&dir).unwrap();
}
#[test]
fn resume_reopens_an_existing_log_and_writes_on_through_its_lock() {
    let dir = temp_dir("reopen");
    let id = written(&dir, &sample_records());
    let mut store = JsonlStore::open(&dir).unwrap();
    let reopened = store.reopen(&id, ResumeLock::Required).unwrap();
    assert_eq!(kept(&reopened.lines), sample_records());
    assert_eq!(reopened.next_seq, 4);
    assert_eq!(reopened.truncated_bytes, 0);
    assert_eq!(reopened.lock, LogLock::Held);
    let refused = JsonlStore::open(&dir)
        .unwrap()
        .reopen(&id, ResumeLock::Required)
        .unwrap_err();
    assert_eq!(refused.kind(), io::ErrorKind::WouldBlock);
    assert_eq!(
        refused.to_string(),
        format!("session {} is open in another Wisp", short_id(&id))
    );

    let mut log = SessionLog::continuing(Box::new(store), id.clone(), reopened.next_seq);
    assert_eq!(
        log.record(LogEvent::SessionEnded {
            turns: 0,
            interrupted: None
        }),
        None
    );
    let all = JsonlStore::open(&dir).unwrap().read(&id).unwrap();
    assert_eq!(all.len(), 5);
    assert_eq!(all[4].seq, 4);
    assert!(JsonlStore::open(&dir).unwrap().probe(&id).is_err());
    drop(log);
    assert_eq!(
        JsonlStore::open(&dir).unwrap().probe(&id).unwrap(),
        LogLock::Held
    );
    fs::remove_dir_all(&dir).unwrap();
}

#[test]
fn resuming_a_mistyped_id_creates_nothing() {
    let dir = temp_dir("reopen-missing");
    let mut store = JsonlStore::open(&dir).unwrap();
    let id = new_session_id();
    let missing = store.reopen(&id, ResumeLock::Required).unwrap_err();
    assert_eq!(missing.kind(), io::ErrorKind::NotFound);
    assert!(!store.path(&id).exists());
    assert_eq!(
        store.probe(&id).unwrap_err().kind(),
        io::ErrorKind::NotFound
    );
    assert!(!store.path(&id).exists());
    assert_eq!(store.list().unwrap(), Vec::<String>::new());
    assert!(store.reopen("../x", ResumeLock::Required).is_err());
    fs::remove_dir_all(&dir).unwrap();
}

#[test]
fn a_cut_last_line_is_truncated_through_the_locked_handle_on_the_first_write() {
    let dir = temp_dir("reopen-cut");
    let id = written(&dir, &sample_records());
    let path = JsonlStore::open(&dir).unwrap().path(&id);
    let whole = fs::read(&path).unwrap();
    let cut = br#"{"schema_version":20,"seq":4,"at":"x","type":"turn_"#;
    fs::write(&path, [whole.as_slice(), cut].concat()).unwrap();

    let mut store = JsonlStore::open(&dir).unwrap();
    let reopened = store.reopen(&id, ResumeLock::Required).unwrap();
    assert_eq!(kept(&reopened.lines), sample_records());
    assert_eq!(reopened.truncated_bytes, cut.len() as u64);
    assert_eq!(
        fs::read(&path).unwrap().len(),
        whole.len() + cut.len(),
        "nothing changes until the session writes"
    );
    store
        .append(
            &id,
            &record(
                4,
                LogEvent::SessionEnded {
                    turns: 0,
                    interrupted: None,
                },
            ),
        )
        .unwrap();
    store.sync(&id).unwrap();
    let after = fs::read(&path).unwrap();
    assert!(after.starts_with(&whole));
    let records = parse_jsonl(&after).unwrap();
    assert_eq!(records.len(), 5);
    assert_eq!(
        records[4].event,
        LogEvent::SessionEnded {
            turns: 0,
            interrupted: None
        }
    );

    fs::remove_file(&path).unwrap();
    store
        .append(
            &id,
            &record(
                5,
                LogEvent::SessionEnded {
                    turns: 0,
                    interrupted: None,
                },
            ),
        )
        .unwrap();
    assert!(
        !path.exists(),
        "a resumed session writes through its handle"
    );
    fs::remove_dir_all(&dir).unwrap();
}

#[test]
fn a_log_with_newer_records_is_refused_and_left_alone() {
    let dir = temp_dir("reopen-newer");
    let id = written(&dir, &sample_records()[..1]);
    let path = JsonlStore::open(&dir).unwrap().path(&id);
    let newer = format!(
        "{{\"schema_version\":{},\"seq\":1,\"at\":\"x\",\"type\":\"turn_started\",\"renamed\":1}}\n{{\"seq\":2",
        LOG_SCHEMA_VERSION + 1
    );
    let mut bytes = fs::read(&path).unwrap();
    bytes.extend_from_slice(newer.as_bytes());
    fs::write(&path, &bytes).unwrap();
    let mut store = JsonlStore::open(&dir).unwrap();
    let refused = store.reopen(&id, ResumeLock::Required).unwrap_err();
    assert_eq!(refused.kind(), io::ErrorKind::InvalidData);
    assert_eq!(
        refused.to_string(),
        "a newer Wisp wrote 1 of this log's records, so only that version can resume it"
    );
    drop(store);
    assert_eq!(fs::read(&path).unwrap(), bytes);
    fs::remove_dir_all(&dir).unwrap();
}

#[cfg(unix)]
fn copied<T>(take_lock: impl FnOnce() -> T) -> (T, Vec<fs::File>) {
    wisp_core::lock_copies::keep_from_now();
    let out = take_lock();
    (out, wisp_core::lock_copies::take())
}

#[cfg(unix)]
fn newer_log(dir: &Path) -> String {
    let id = written(dir, &sample_records()[..1]);
    let path = JsonlStore::open(dir).unwrap().path(&id);
    let newer = format!(
        "{{\"schema_version\":{},\"seq\":1,\"at\":\"x\",\"type\":\"turn_started\"}}\n",
        LOG_SCHEMA_VERSION + 1
    );
    let mut bytes = fs::read(&path).unwrap();
    bytes.extend_from_slice(newer.as_bytes());
    fs::write(&path, &bytes).unwrap();
    id
}

#[cfg(unix)]
#[test]
fn a_copy_of_a_lock_handle_that_outlives_it_does_not_keep_the_lock() {
    let dir = temp_dir("lock-copies");
    let id = written(&dir, &sample_records());
    let free = |id: &str| JsonlStore::open(&dir).unwrap().probe(id).unwrap() == LogLock::Held;

    let (store, copies) = copied(|| {
        let mut store = JsonlStore::open(&dir).unwrap();
        assert_eq!(store.open_session(&id).unwrap(), LogLock::Held);
        store
    });
    assert_eq!(copies.len(), 1);
    drop(store);
    assert!(free(&id), "a store that opened the session");
    drop(copies);

    let (store, copies) = copied(|| {
        let mut store = JsonlStore::open(&dir).unwrap();
        store.reopen(&id, ResumeLock::Required).unwrap();
        store
    });
    assert_eq!(copies.len(), 1);
    drop(store);
    assert!(free(&id), "a store that reopened the session");
    drop(copies);

    let (lock, copies) = copied(|| JsonlStore::open(&dir).unwrap().probe(&id).unwrap());
    assert_eq!(lock, LogLock::Held);
    assert_eq!(copies.len(), 1);
    assert!(free(&id), "a probe");
    drop(copies);

    let newer = newer_log(&dir);
    let (refused, copies) = copied(|| {
        JsonlStore::open(&dir)
            .unwrap()
            .reopen(&newer, ResumeLock::Required)
            .unwrap_err()
    });
    assert_eq!(refused.kind(), io::ErrorKind::InvalidData);
    assert_eq!(copies.len(), 1);
    assert!(free(&newer), "a refused reopen");
    drop(copies);
    fs::remove_dir_all(&dir).unwrap();
}

#[cfg(unix)]
#[test]
fn a_copy_of_a_folder_lock_that_outlives_it_does_not_keep_the_lock() {
    use std::time::Duration;
    use wisp_core::secrets::{LockKind, PathShown, lock};
    let root = temp_dir("folder-copies");
    let (one, two) = (root.join("one"), root.join("two"));
    let take = |folders: &[&Path]| {
        let wanted: Vec<(&Path, LockKind)> = folders
            .iter()
            .map(|f| (*f, LockKind::Credentials))
            .collect();
        lock(&wanted, PathShown::Default, Duration::ZERO)
    };

    let (held, copies) = copied(|| take(&[&one]).unwrap());
    assert_eq!(copies.len(), 1);
    drop(held);
    assert!(take(&[&one]).is_ok(), "a folder lock");
    drop(copies);

    let mut locked_first = 0;
    for (busy, other) in [(&one, &two), (&two, &one)] {
        let holder = take(&[busy]).unwrap();
        let (refused, copies) = copied(|| take(&[&one, &two]));
        assert!(refused.is_err());
        drop(holder);
        locked_first += copies.len();
        assert!(take(&[other]).is_ok(), "a folder locked before a busy one");
        drop(copies);
    }
    assert_eq!(
        locked_first, 1,
        "one order locks a folder before the busy one"
    );
    fs::remove_dir_all(&root).unwrap();
}
