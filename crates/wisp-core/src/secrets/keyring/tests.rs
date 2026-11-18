use std::time::{Duration, Instant};

use super::*;
use crate::test_guard::name;

fn id() -> StoreId {
    StoreId::parse("0123456789abcdef").unwrap()
}

fn stored(key: &str, url: Option<&str>, owner: Option<&str>) -> Stored {
    Stored {
        record: StoredRecord::Key {
            key: Secret::new(key),
            url: url.map(Secret::new),
        },
        owner: owner.map(PathBuf::from),
    }
}

fn key_of(stored: &Stored) -> &str {
    let StoredRecord::Key { key, .. } = &stored.record;
    key.expose()
}

const LONG: Duration = Duration::from_secs(30);

#[test]
fn an_item_is_versioned_json_under_the_wisp_service_and_store_account() {
    assert_eq!(SERVICE, "wisp");
    assert_eq!(account(&id(), &name("direct")), "0123456789abcdef/direct");
    let text = encode(&stored(
        "k-1",
        Some("https://gw.example"),
        Some("/c/config.toml"),
    ));
    let value: Value = serde_json::from_str(&text).unwrap();
    assert_eq!(value["v"], 1);
    assert_eq!(value["key"], "k-1");
    assert_eq!(value["url"], "https://gw.example");
    assert_eq!(value["owner"], "/c/config.toml");
    let back = decode(&text, &name("direct")).unwrap();
    assert_eq!(key_of(&back), "k-1");
    assert_eq!(
        back.owner.as_deref(),
        Some(std::path::Path::new("/c/config.toml"))
    );
    let bare = decode(&encode(&stored("k-2", None, None)), &name("direct")).unwrap();
    assert!(bare.owner.is_none());
}

#[test]
fn a_newer_item_is_unavailable_and_never_parsed_further() {
    let error = decode(r#"{"v":2,"key":"k","extra":true}"#, &name("direct")).unwrap_err();
    assert_eq!(
        error,
        KeyringError::Newer {
            name: name("direct")
        }
    );
    assert_eq!(
        error.to_string(),
        "the keychain item for direct is from a newer Wisp"
    );
}

#[test]
fn a_malformed_item_names_its_problem_and_never_its_text() {
    let canary = "canary-secret-0123456789abcdef";
    let cases = [
        (format!("{canary} not json"), ItemProblem::NotJson),
        (format!("[\"{canary}\"]"), ItemProblem::NotAnObject),
        (format!("{{\"key\":\"{canary}\"}}"), ItemProblem::NoVersion),
        (
            format!("{{\"v\":0,\"key\":\"{canary}\"}}"),
            ItemProblem::BadVersion,
        ),
        (
            format!("{{\"v\":1,\"key\":\"{canary}\",\"note\":1}}"),
            ItemProblem::UnknownField,
        ),
        (
            format!("{{\"v\":1,\"url\":\"{canary}\"}}"),
            ItemProblem::NoKey,
        ),
        ("{\"v\":1,\"key\":7}".to_owned(), ItemProblem::KeyNotString),
        (
            format!("{{\"v\":1,\"key\":\"{canary}\",\"owner\":2}}"),
            ItemProblem::OwnerNotString,
        ),
        (
            format!("{{\"v\":1,\"key\":\"{canary}\",\"url\":[]}}"),
            ItemProblem::UrlNotString,
        ),
    ];
    for (text, problem) in cases {
        let error = decode(&text, &name("direct")).unwrap_err();
        assert_eq!(
            error,
            KeyringError::BadItem {
                name: name("direct"),
                problem
            },
            "{text}"
        );
        let shown = format!("{error} {error:?}");
        assert!(!shown.contains(canary), "{shown}");
    }
    assert_eq!(
        decode("{\"v\":1}", &name("direct"))
            .unwrap_err()
            .to_string(),
        "the keychain item for direct isn't a valid Wisp record (a record needs a key)"
    );
}

#[test]
fn each_backend_error_maps_by_kind_and_only_session_faults_take_the_keychain_down() {
    let cases = [
        (BackendError::NoStorageAccess, KeyringError::Locked, true),
        (BackendError::PlatformFailure, KeyringError::Failed, true),
        (BackendError::TooLong, KeyringError::TooLong, false),
        (
            BackendError::BadEncoding,
            KeyringError::BadText { name: name("a") },
            false,
        ),
        (
            BackendError::BadDataFormat,
            KeyringError::BadText { name: name("a") },
            false,
        ),
        (BackendError::Invalid, KeyringError::Refused, false),
        (BackendError::Ambiguous, KeyringError::Refused, false),
        (BackendError::Unsupported, KeyringError::Refused, false),
        (BackendError::Other, KeyringError::Refused, false),
    ];
    for (backend, mapped, downs) in cases {
        let memory = MemoryKeyring::new();
        memory.set_mode(MemoryMode::Failing(backend));
        let store = memory.store(LONG);
        assert_eq!(
            store.read(&id(), &name("a"), Budget::Start),
            Err(mapped.clone())
        );
        assert_eq!(store.down().is_some(), downs, "{backend:?}");
        memory.set_mode(MemoryMode::Working);
        memory.insert(
            &account(&id(), &name("b")),
            &encode(&stored("k", None, None)),
        );
        let next = store.read(&id(), &name("b"), Budget::Start);
        if downs {
            assert_eq!(next, Err(mapped), "down after the first failure");
            assert_eq!(memory.state().calls, 1);
        } else {
            assert_eq!(
                next.map(|s| s.map(|s| key_of(&s).to_owned())),
                Ok(Some("k".into()))
            );
        }
    }
}

#[test]
fn a_missing_item_is_none_and_every_item_is_read_once() {
    let memory = MemoryKeyring::new();
    memory.insert(
        &account(&id(), &name("a")),
        &encode(&stored("k-a", None, None)),
    );
    memory.insert(&account(&id(), &name("bad")), "{\"v\":1}");
    let store = memory.store(LONG);
    for _ in 0..3 {
        let a = store
            .read(&id(), &name("a"), Budget::Start)
            .unwrap()
            .unwrap();
        assert_eq!(key_of(&a), "k-a");
        assert_eq!(store.read(&id(), &name("gone"), Budget::Short), Ok(None));
        assert!(matches!(
            store.read(&id(), &name("bad"), Budget::Start),
            Err(KeyringError::BadItem { .. })
        ));
    }
    let state = memory.state();
    assert_eq!(state.calls, 3);
    assert_eq!(state.opened, 1, "one connection");
}

#[test]
fn a_keychain_that_never_answers_costs_one_deadline_in_total() {
    let memory = MemoryKeyring::new();
    memory.set_mode(MemoryMode::Hanging);
    let store = memory.store(Duration::from_millis(400));
    let started = Instant::now();
    assert_eq!(
        store.read(&id(), &name("a"), Budget::Start),
        Err(KeyringError::TimedOut)
    );
    for other in ["b", "c", "d"] {
        assert_eq!(
            store.read(&id(), &name(other), Budget::Start),
            Err(KeyringError::TimedOut)
        );
    }
    let spent = started.elapsed();
    assert!(spent >= Duration::from_millis(350), "{spent:?}");
    assert!(spent < Duration::from_millis(1500), "{spent:?}");
    assert_eq!(memory.state().calls, 1);
    assert_eq!(store.fixed(), Some(KeyringError::TimedOut));
}

#[test]
fn an_endpoint_not_started_on_waits_at_most_the_short_budget() {
    let memory = MemoryKeyring::new();
    memory.set_mode(MemoryMode::Hanging);
    let store = memory.store(LONG);
    let started = Instant::now();
    assert_eq!(
        store.read(&id(), &name("other"), Budget::Short),
        Err(KeyringError::TimedOut)
    );
    let spent = started.elapsed();
    assert!(
        spent < SHORT_BUDGET + Duration::from_millis(700),
        "{spent:?}"
    );
    assert!(
        spent >= SHORT_BUDGET - Duration::from_millis(50),
        "{spent:?}"
    );
}

#[test]
fn a_keychain_that_cant_be_opened_is_not_running_and_opened_once() {
    let memory = MemoryKeyring::new();
    memory.set_mode(MemoryMode::Unopenable);
    let store = memory.store(LONG);
    assert_eq!(
        store.read(&id(), &name("a"), Budget::Start),
        Err(KeyringError::NotRunning)
    );
    assert_eq!(
        store.write(&id(), &name("a"), &stored("k", None, None), Budget::Start),
        Err(KeyringError::NotRunning)
    );
    let state = memory.state();
    assert_eq!(state.opened, 1);
    assert_eq!(state.calls, 0);
}

#[test]
fn a_keychain_down_from_the_start_is_never_opened() {
    let memory = MemoryKeyring::new();
    let store = memory.store(LONG).down_from_start(KeyringError::BusSilent);
    assert_eq!(
        store.read(&id(), &name("a"), Budget::Start),
        Err(KeyringError::BusSilent)
    );
    assert_eq!(memory.state().opened, 0);
    assert_eq!(store.fixed(), Some(KeyringError::BusSilent));
}

#[test]
fn the_session_runs_outside_any_runtime_even_when_started_inside_one() {
    let memory = MemoryKeyring::new();
    memory.insert(
        &account(&id(), &name("a")),
        &encode(&stored("k", None, None)),
    );
    let store = memory.store(LONG);
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(1)
        .enable_all()
        .build()
        .unwrap();
    let found = runtime.block_on(async {
        assert!(tokio::runtime::Handle::try_current().is_ok());
        store.read(&id(), &name("a"), Budget::Start)
    });
    assert!(found.unwrap().is_some());
    assert_eq!(memory.state().on_runtime_thread, vec![false]);
}

#[test]
fn a_write_is_read_back_and_a_delete_reports_whether_the_item_was_there() {
    let memory = MemoryKeyring::new();
    let store = memory.store(LONG);
    let record = stored("k-new", Some("https://gw.example"), Some("/c/config.toml"));
    store
        .write(&id(), &name("a"), &record, Budget::Start)
        .unwrap();
    let items = memory.items();
    let text = &items[&account(&id(), &name("a"))];
    assert_eq!(decode(text, &name("a")).unwrap(), record);
    assert_eq!(memory.state().calls, 2, "set, then read back");
    assert_eq!(
        store.read(&id(), &name("a"), Budget::Start),
        Ok(Some(record))
    );
    assert_eq!(memory.state().calls, 2, "the write filled the cache");
    assert_eq!(store.delete(&id(), &name("a"), Budget::Start), Ok(true));
    assert!(memory.items().is_empty());
    assert_eq!(store.read(&id(), &name("a"), Budget::Start), Ok(None));
    assert_eq!(store.delete(&id(), &name("a"), Budget::Start), Ok(false));
}

#[test]
fn a_write_that_is_too_long_is_refused_and_keeps_the_session_up() {
    let memory = MemoryKeyring::new();
    memory.set_mode(MemoryMode::Failing(BackendError::TooLong));
    let store = memory.store(LONG);
    let error = store
        .write(&id(), &name("a"), &stored("k", None, None), Budget::Start)
        .unwrap_err();
    assert_eq!(error, KeyringError::TooLong);
    assert_eq!(error.which().as_deref(), Some("refused the request"));
    assert!(store.down().is_none());
}

#[test]
fn unreachable_fails_every_call_with_its_reason() {
    let off = Unreachable(KeyringError::Off {
        origin: "WISP_KEYCHAIN".to_owned(),
    });
    assert_eq!(off.fixed(), Some(off.0.clone()));
    assert_eq!(off.read(&id(), &name("a")), Err(off.0.clone()));
    assert_eq!(
        off.write(&id(), &name("a"), &stored("k", None, None)),
        Err(off.0.clone())
    );
    assert_eq!(off.delete(&id(), &name("a")), Err(off.0.clone()));
    assert_eq!(off.0.to_string(), "the keychain is off (WISP_KEYCHAIN)");
}

#[test]
fn the_waiting_notice_is_given_once_and_only_for_reads_that_wait_the_long_deadline() {
    let memory = MemoryKeyring::new();
    memory.set_mode(MemoryMode::Hanging);
    let told = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let counter = told.clone();
    let store = memory
        .store(SLOW_NOTICE + Duration::from_millis(300))
        .with_notice(Box::new(move || {
            counter.fetch_add(1, Ordering::Relaxed);
        }));
    assert_eq!(
        store.read(&id(), &name("a"), Budget::Start),
        Err(KeyringError::TimedOut)
    );
    let _ = store.read(&id(), &name("b"), Budget::Start);
    assert_eq!(told.load(Ordering::Relaxed), 1);
}

#[test]
fn a_write_phase_gets_a_fresh_deadline_after_the_start_up_one_has_passed() {
    let memory = MemoryKeyring::new();
    let store = memory.store(Duration::from_millis(100));
    std::thread::sleep(Duration::from_millis(250));
    store.begin_phase();
    store
        .write(&id(), &name("a"), &stored("k", None, None), Budget::Start)
        .unwrap();
    assert_eq!(
        memory.state().calls,
        2,
        "set and read-back reached the backend"
    );
    assert!(store.down().is_none());
}

#[test]
fn a_read_back_that_differs_fails_the_write_and_deletes_the_item() {
    let memory = MemoryKeyring::new();
    memory.set_mode(MemoryMode::Garbling);
    let store = memory.store(LONG);
    assert_eq!(
        store.write(&id(), &name("a"), &stored("k", None, None), Budget::Start),
        Err(KeyringError::Failed)
    );
    assert!(
        memory.items().is_empty(),
        "the unconfirmed item was deleted"
    );
}

#[test]
fn a_read_back_that_fails_says_the_key_may_have_been_saved_and_names_the_account() {
    let memory = MemoryKeyring::new();
    memory.set_mode(MemoryMode::ReadsFail(BackendError::PlatformFailure));
    let store = memory.store(LONG);
    let error = store
        .write(&id(), &name("a"), &stored("k", None, None), Budget::Start)
        .unwrap_err();
    assert_eq!(
        error,
        KeyringError::Unconfirmed {
            name: name("a"),
            account: account(&id(), &name("a"))
        }
    );
    assert_eq!(
        error.to_string(),
        "the keychain didn't confirm the write, so a's key may have been saved there (account 0123456789abcdef/a)"
    );
}

fn until(done: impl Fn() -> bool) {
    let started = Instant::now();
    while !done() {
        assert!(
            started.elapsed() < Duration::from_secs(10),
            "never happened"
        );
        std::thread::sleep(Duration::from_millis(5));
    }
}

fn fresh_write(store: &std::sync::Arc<KeyringStore>) -> mpsc::Receiver<Result<(), KeyringError>> {
    let (tx, rx) = mpsc::channel();
    let store = std::sync::Arc::clone(store);
    std::thread::spawn(move || {
        let written = store.write(
            &id(),
            &name("a"),
            &stored("k", None, None),
            Budget::Fresh(TERMINAL_DEADLINE),
        );
        drop(tx.send(written));
    });
    rx
}

#[test]
fn a_fresh_write_to_a_locked_keychain_waits_from_its_own_start_and_saves_once_unlocked() {
    let clock = FakeClock::default();
    let memory = MemoryKeyring::new();
    memory.set_mode(MemoryMode::Locked);
    let store = std::sync::Arc::new(memory.store_on(TERMINAL_DEADLINE, clock.clone()));
    clock.advance(TERMINAL_DEADLINE + Duration::from_secs(1));
    assert_eq!(store.wait(Limit::Start), Duration::ZERO);
    let written = fresh_write(&store);
    until(|| memory.state().calls == 1);
    for _ in 0..5 {
        clock.advance(Duration::from_secs(1));
        std::thread::sleep(Duration::from_millis(20));
    }
    assert!(
        written.try_recv().is_err(),
        "past the short budget the write still waits"
    );
    memory.set_mode(MemoryMode::Working);
    assert_eq!(
        written.recv_timeout(Duration::from_secs(10)).unwrap(),
        Ok(())
    );
    assert!(store.down().is_none());
    assert_eq!(memory.items().len(), 1);
}

#[test]
fn a_fresh_write_that_never_answers_times_out_only_when_its_own_limit_passes() {
    let clock = FakeClock::default();
    let memory = MemoryKeyring::new();
    memory.set_mode(MemoryMode::Hanging);
    let store = std::sync::Arc::new(memory.store_on(TERMINAL_DEADLINE, clock.clone()));
    clock.advance(TERMINAL_DEADLINE + Duration::from_secs(1));
    let written = fresh_write(&store);
    until(|| memory.state().calls == 1);
    clock.advance(TERMINAL_DEADLINE - Duration::from_secs(1));
    std::thread::sleep(Duration::from_millis(50));
    assert!(written.try_recv().is_err(), "59 s in, still waiting");
    assert!(store.down().is_none());
    clock.advance(Duration::from_secs(1));
    assert_eq!(
        written.recv_timeout(Duration::from_secs(10)).unwrap(),
        Err(KeyringError::TimedOut)
    );
    assert_eq!(store.down(), Some(KeyringError::TimedOut));
}

#[test]
fn a_fresh_write_shares_one_limit_between_the_write_and_its_read_back() {
    let clock = FakeClock::default();
    let memory = MemoryKeyring::new();
    memory.set_mode(MemoryMode::Locked);
    let store = std::sync::Arc::new(memory.store_on(TERMINAL_DEADLINE, clock.clone()));
    clock.advance(TERMINAL_DEADLINE + Duration::from_secs(1));
    let written = fresh_write(&store);
    until(|| memory.state().calls == 1);
    clock.advance(TERMINAL_DEADLINE - Duration::from_secs(1));
    std::thread::sleep(Duration::from_millis(20));
    memory.set_mode(MemoryMode::ReadsHang);
    until(|| memory.state().calls == 2);
    std::thread::sleep(Duration::from_millis(50));
    assert!(written.try_recv().is_err(), "59 s in, the read-back waits");
    clock.advance(Duration::from_secs(1));
    assert_eq!(
        written.recv_timeout(Duration::from_secs(5)).unwrap(),
        Err(KeyringError::Unconfirmed {
            name: name("a"),
            account: account(&id(), &name("a")),
        })
    );
    assert_eq!(memory.items().len(), 1, "the set reached the keychain");
}
